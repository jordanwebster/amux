use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use store::{Budget, Store, StoreError};
use tempfile::TempDir;

fn database(temp: &TempDir) -> PathBuf {
    temp.path().join("store.sqlite")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn assert_open_error(path: &Path, expected: StoreError) {
    let result = runtime().block_on(Store::open(path));
    match result {
        Ok(store) => {
            runtime().block_on(store.close());
            panic!("store unexpectedly opened")
        }
        Err(error) => assert_eq!(error, expected),
    }
}

#[test]
fn lifecycle_bootstraps_registry_and_qualifies_library() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("open");
    assert!(store.library_report().version.starts_with("3."));
    assert!(!store.library_report().source_id.is_empty());
    assert_eq!(store.generations().fleet, 1);
    assert_eq!(store.generations().chat, 1);
    runtime().block_on(store.close());

    let connection = Connection::open(path).expect("inspect database");
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    let auto_vacuum: i64 = connection
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .expect("auto vacuum");
    let families: i64 = connection
        .query_row("SELECT COUNT(*) FROM family_shape", [], |row| row.get(0))
        .expect("family count");
    assert_eq!(journal, "wal");
    assert_eq!(auto_vacuum, 2);
    assert_eq!(families, 7);
}

#[test]
fn lifecycle_refuses_checksum_mismatch_and_unknown_newer_state() {
    for mutation in ["checksum", "unknown_migration", "newer_shape"] {
        let temp = TempDir::new().expect("tempdir");
        let path = database(&temp);
        let store = runtime().block_on(Store::open(&path)).expect("seed");
        runtime().block_on(store.close());
        let connection = Connection::open(&path).expect("open raw");
        match mutation {
            "checksum" => {
                connection
                    .execute(
                        "UPDATE migration SET checksum='wrong' WHERE family='view' AND id=1",
                        [],
                    )
                    .expect("mutate checksum");
            }
            "unknown_migration" => {
                connection
                    .execute(
                        "INSERT INTO migration(family,id,checksum,applied_at) VALUES ('future',1,'x',0)",
                        [],
                    )
                    .expect("insert future migration");
            }
            "newer_shape" => {
                connection
                    .execute("UPDATE family_shape SET shape=99 WHERE family='fleet'", [])
                    .expect("advance shape");
            }
            _ => unreachable!(),
        }
        drop(connection);
        assert_open_error(&path, StoreError::UnsupportedFormat);
    }
}

#[test]
fn lifecycle_interrupted_migration_restarts_from_ledger_prefix() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("seed");
    runtime().block_on(store.close());

    let connection = Connection::open(&path).expect("raw connection");
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             DELETE FROM migration WHERE family='view';
             DROP TABLE view_state;
             CREATE TABLE interrupted_migration(value TEXT);
             ROLLBACK;",
        )
        .expect("simulate interruption");
    drop(connection);

    let store = runtime().block_on(Store::open(&path)).expect("reopen");
    runtime().block_on(store.close());
    let connection = Connection::open(path).expect("inspect");
    let view_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='view_state')",
            [],
            |row| row.get(0),
        )
        .expect("view table");
    assert!(view_exists);
}

#[test]
fn lifecycle_two_concurrent_openers_share_the_store() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let store = runtime()
                .block_on(Store::open(&path))
                .expect("concurrent open");
            std::thread::sleep(Duration::from_millis(50));
            runtime().block_on(store.close());
        }));
    }
    barrier.wait();
    for handle in handles {
        handle.join().expect("opener thread");
    }
}

#[test]
fn lifecycle_exclusive_lease_times_out_as_busy_after_five_seconds() {
    let temp = TempDir::new().expect("tempdir");
    let lock_path = temp.path().join("store.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .expect("lock file");
    lock.try_lock().expect("exclusive lease");
    let started = Instant::now();
    assert_open_error(&database(&temp), StoreError::Busy);
    assert!(started.elapsed() >= Duration::from_millis(4_900));
}

#[test]
fn lifecycle_renames_dependency_closure_and_maintains_in_batches() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("seed");
    runtime().block_on(store.close());

    let mut connection = Connection::open(&path).expect("raw connection");
    let transaction = connection.transaction().expect("transaction");
    {
        let mut statement = transaction
            .prepare(
                "INSERT INTO chat_state(agent_id,revision,content_revision,segment_high_water,previous_through,needs_baseline)
                 VALUES (?1,0,0,0,NULL,0)",
            )
            .expect("insert statement");
        for row in 0..250 {
            statement
                .execute([format!("agent-{row}")])
                .expect("insert row");
        }
    }
    transaction
        .execute("UPDATE family_shape SET shape=0 WHERE family='chat'", [])
        .expect("age chat shape");
    transaction.commit().expect("commit fixture");
    drop(connection);

    let store = runtime().block_on(Store::open(&path)).expect("rebuild");
    assert_eq!(store.generations().chat, 2);
    assert_eq!(store.generations().claude_pty, 2);
    assert_eq!(store.generations().claude_sdk, 2);
    assert_eq!(store.generations().codex, 2);
    assert_eq!(store.generations().fleet, 1);

    let inspection = Connection::open(&path).expect("inspect renamed indexes");
    let old_index_table: String = inspection
        .query_row(
            "SELECT tbl_name FROM sqlite_schema WHERE type='index' AND name='segment_order_g1'",
            [],
            |row| row.get(0),
        )
        .expect("retired index");
    let fresh_index_table: String = inspection
        .query_row(
            "SELECT tbl_name FROM sqlite_schema WHERE type='index' AND name='segment_order_g2'",
            [],
            |row| row.get(0),
        )
        .expect("fresh index");
    assert_eq!(old_index_table, "segment_old_1");
    assert_eq!(fresh_index_table, "segment");
    drop(inspection);

    let report = runtime()
        .block_on(store.maintain(
            Budget {
                retired_rows_per_table: 100,
                vacuum_steps: 1,
                ..Budget::default()
            },
            Duration::from_secs(5),
        ))
        .expect("maintenance");
    assert_eq!(report.retired_rows_deleted, 250);
    assert!(report.retired_tables_dropped >= 1);
    assert!(report.quick_check_complete);
    assert!(report.checkpoint_complete);
    assert_eq!(report.vacuum_steps, 1);
    runtime().block_on(store.close());
}

#[test]
fn lifecycle_zero_deadline_is_incomplete_not_corrupt() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("open");
    let report = runtime()
        .block_on(store.maintain(Budget::default(), Duration::ZERO))
        .expect("deadline is not corruption");
    assert!(report.deadline_reached);
    assert!(!report.quick_check_complete);
    runtime().block_on(store.close());
}

#[test]
fn lifecycle_interrupted_quarantine_finishes_idempotent_moves() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("seed");
    runtime().block_on(store.close());

    let directory = temp.path().join("quarantine/interrupted");
    fs::create_dir_all(&directory).expect("quarantine directory");
    fs::rename(&path, directory.join("store.sqlite")).expect("partial move");
    fs::write(
        directory.join("manifest.json"),
        r#"{
          "id":"interrupted",
          "database":"store.sqlite",
          "files":["store.sqlite","store.sqlite-wal","store.sqlite-shm"],
          "durable_unresolved":true,
          "moved":false,
          "complete":false
        }"#,
    )
    .expect("manifest");

    let store = runtime().block_on(Store::open(&path)).expect("recover");
    runtime().block_on(store.close());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(manifest["complete"], true);
    let connection = Connection::open(path).expect("fresh database");
    let unresolved: i64 = connection
        .query_row(
            "SELECT durable_unresolved FROM quarantine WHERE id='interrupted'",
            [],
            |row| row.get(0),
        )
        .expect("quarantine row");
    assert_eq!(unresolved, 1);
}

#[test]
fn lifecycle_corruption_waits_for_the_other_process_before_quarantine() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("seed");
    runtime().block_on(store.close());

    let ready = temp.path().join("child-ready");
    let release = temp.path().join("child-release");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--ignored",
            "--exact",
            "lifecycle_helper_holds_shared_store",
            "--nocapture",
        ])
        .env("AMUX_STORE_HELPER_DB", &path)
        .env("AMUX_STORE_HELPER_READY", &ready)
        .env("AMUX_STORE_HELPER_RELEASE", &release)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helper");
    wait_for(&ready, Duration::from_secs(10));

    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open database bytes");
    file.seek(SeekFrom::Start(0)).expect("seek");
    file.write_all(b"not a sqlite db!").expect("corrupt header");
    file.sync_all().expect("sync corruption");
    drop(file);

    assert_open_error(&path, StoreError::Corrupt);
    assert!(path.exists(), "a live peer keeps the corrupt file in place");
    assert!(temp.path().join("quarantine/request.json").exists());

    fs::write(&release, b"release").expect("release helper");
    assert!(child.wait().expect("wait helper").success());

    let recovered = runtime()
        .block_on(Store::open(&path))
        .expect("quarantine and reopen");
    runtime().block_on(recovered.close());
    let connection = Connection::open(&path).expect("fresh database");
    let unresolved: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM quarantine WHERE durable_unresolved=1",
            [],
            |row| row.get(0),
        )
        .expect("unresolved quarantine");
    assert_eq!(unresolved, 1);
}

fn wait_for(path: &Path, timeout: Duration) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "spawned by lifecycle_corruption_waits_for_the_other_process_before_quarantine"]
fn lifecycle_helper_holds_shared_store() {
    let path = PathBuf::from(std::env::var_os("AMUX_STORE_HELPER_DB").expect("database path"));
    let ready = PathBuf::from(std::env::var_os("AMUX_STORE_HELPER_READY").expect("ready path"));
    let release =
        PathBuf::from(std::env::var_os("AMUX_STORE_HELPER_RELEASE").expect("release path"));
    let store = runtime().block_on(Store::open(&path)).expect("helper open");
    fs::write(&ready, b"ready").expect("ready marker");
    wait_for(&release, Duration::from_secs(30));
    runtime().block_on(store.close());
}
