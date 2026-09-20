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
            "DELETE FROM migration WHERE family='view';
             DELETE FROM family_shape WHERE family='view';
             DROP TABLE view_state;
             CREATE TRIGGER interrupt_view_migration
             BEFORE INSERT ON migration
             WHEN NEW.family='view'
             BEGIN
                 SELECT RAISE(ABORT, 'interrupt view migration after DDL');
             END;",
        )
        .expect("install committed migration obstacle");
    drop(connection);

    assert_open_error(&path, StoreError::Io);
    let connection = Connection::open(&path).expect("inspect failed migration");
    let partial_schema: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='view_state')",
            [],
            |row| row.get(0),
        )
        .expect("partial schema check");
    let ledger_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM migration WHERE family='view'",
            [],
            |row| row.get(0),
        )
        .expect("migration ledger check");
    let shape_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM family_shape WHERE family='view'",
            [],
            |row| row.get(0),
        )
        .expect("family shape check");
    assert!(!partial_schema, "failed open must roll back migration DDL");
    assert_eq!(ledger_rows, 0, "failed open must not advance the ledger");
    assert_eq!(shape_rows, 0, "failed open must not publish the new shape");
    connection
        .execute("DROP TRIGGER interrupt_view_migration", [])
        .expect("remove migration obstacle");
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
    let ledger_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM migration WHERE family='view' AND id=1",
            [],
            |row| row.get(0),
        )
        .expect("completed migration ledger");
    let shape_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM family_shape WHERE family='view' AND shape=1",
            [],
            |row| row.get(0),
        )
        .expect("completed family shape");
    assert_eq!(ledger_rows, 1);
    assert_eq!(shape_rows, 1);
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
fn lifecycle_interrupted_quick_check_is_incomplete_not_corrupt() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("open");
    let connection = Connection::open(&path).expect("seed checked pages");
    connection
        .execute_batch(
            "PRAGMA synchronous=OFF;
             CREATE TABLE checked_pages(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             WITH RECURSIVE n(x) AS (
                 VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000
             )
             INSERT INTO checked_pages
             SELECT printf('checked-%08d',x),printf('value-%08d',x) FROM n;",
        )
        .expect("seed enough pages to interrupt quick check");
    drop(connection);

    // The deadline has to outlast the work before the integrity statement
    // and still cut the statement short. Where that sits depends on how fast
    // the disk is, so start at the shortest and give way to a slower one
    // until the statement runs at all.
    let mut deadline = Duration::from_millis(25);
    let report = loop {
        let started = Instant::now();
        let report = runtime()
            .block_on(store.maintain(
                Budget {
                    retired_rows_per_table: 0,
                    vacuum_steps: 0,
                    ..Budget::default()
                },
                deadline,
            ))
            .expect("an interrupted integrity statement is incomplete, not corrupt");
        assert!(started.elapsed() >= deadline);
        assert!(report.deadline_reached);
        if report.quick_check_started {
            break report;
        }
        deadline *= 2;
        assert!(
            deadline <= Duration::from_secs(4),
            "maintenance never reached its integrity statement: {report:?}"
        );
    };
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
          "moved_files":[],
          "durable_state":{"status":"unknown"},
          "durable_unresolved":true,
          "moved":false,
          "complete":false
        }"#,
    )
    .expect("manifest");

    let store = runtime().block_on(Store::open(&path)).expect("recover");
    let report = runtime()
        .block_on(store.quarantine_report())
        .expect("unknown durable-state report");
    assert_eq!(
        report.quarantines[0].durable_state,
        store::QuarantineDurableState::Unknown
    );
    assert!(
        report
            .to_string()
            .contains("durable families: could not be determined")
    );
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

fn request_known_quarantine(path: &Path) {
    let root = path.parent().unwrap().join("quarantine");
    fs::create_dir_all(&root).expect("quarantine root");
    fs::write(
        root.join("request.json"),
        r#"{
          "database":"store.sqlite",
          "durable_state":{"status":"present","families":["meta","view"]}
        }"#,
    )
    .expect("quarantine request");
}

fn seed_unresolved_quarantine(temp: &TempDir) -> (PathBuf, store::QuarantineReport) {
    let path = database(temp);
    let store = runtime().block_on(Store::open(&path)).expect("seed store");
    runtime()
        .block_on(store.view_set("remembered", "chat", "kept only in quarantine"))
        .expect("seed durable value");
    runtime().block_on(store.close());
    request_known_quarantine(&path);

    let store = runtime()
        .block_on(Store::open(&path))
        .expect("complete quarantine");
    let report = runtime()
        .block_on(store.quarantine_report())
        .expect("quarantine report");
    runtime().block_on(store.close());
    (path, report)
}

#[test]
fn lifecycle_explicit_resolution_restores_durable_service_without_recovering_empty_tables() {
    let temp = TempDir::new().expect("tempdir");
    let (path, report) = seed_unresolved_quarantine(&temp);
    assert_eq!(report.len(), 1);
    assert_eq!(
        report.quarantines[0].durable_state,
        store::QuarantineDurableState::Present(vec!["meta".to_owned(), "view".to_owned()])
    );
    assert_eq!(
        report.quarantines[0].named_files,
        ["store.sqlite", "store.sqlite-wal", "store.sqlite-shm"]
    );
    assert_eq!(report.quarantines[0].moved_files.len(), 1);
    assert!(report.quarantines[0].moved_files[0].exists());
    let rendered = report.to_string();
    assert!(rendered.contains("durable families: present (meta, view)"));
    assert!(
        rendered
            .contains("files named by manifest: store.sqlite, store.sqlite-wal, store.sqlite-shm")
    );
    assert!(rendered.contains("manifest.json"));
    assert!(rendered.contains("store.sqlite"));
    println!("{rendered}");

    let store = runtime()
        .block_on(Store::open(&path))
        .expect("open blocked store");
    assert_eq!(
        runtime().block_on(store.view_get("remembered", "chat")),
        Err(StoreError::RecoveryRequired)
    );
    assert_eq!(
        runtime().block_on(store.view_set("remembered", "chat", "new value")),
        Err(StoreError::RecoveryRequired)
    );
    runtime().block_on(store.close());

    runtime()
        .block_on(Store::resolve_quarantine(&path, &report))
        .expect("explicit resolution");
    let store = runtime()
        .block_on(Store::open(&path))
        .expect("reopen resolved store");
    assert_eq!(
        runtime()
            .block_on(store.view_get("remembered", "chat"))
            .expect("durable read"),
        None,
        "the fresh empty durable table must not be presented as recovered data"
    );
    runtime()
        .block_on(store.view_set("remembered", "chat", "new value"))
        .expect("durable write");
    assert_eq!(
        runtime()
            .block_on(store.view_get("remembered", "chat"))
            .expect("durable reread"),
        Some("new value".to_owned())
    );
    runtime().block_on(store.close());
}

#[test]
fn lifecycle_resolution_refuses_while_another_process_holds_the_store() {
    let temp = TempDir::new().expect("tempdir");
    let (path, report) = seed_unresolved_quarantine(&temp);
    let ready = temp.path().join("resolve-child-ready");
    let release = temp.path().join("resolve-child-release");
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

    assert_eq!(
        runtime().block_on(Store::resolve_quarantine(&path, &report)),
        Err(StoreError::Busy)
    );
    fs::write(&release, b"release").expect("release helper");
    assert!(child.wait().expect("wait helper").success());

    let store = runtime()
        .block_on(Store::open(&path))
        .expect("reopen unresolved store");
    assert_eq!(
        runtime().block_on(store.view_get("remembered", "chat")),
        Err(StoreError::RecoveryRequired)
    );
    runtime().block_on(store.close());
}

#[test]
fn lifecycle_interrupted_resolution_is_atomic() {
    let temp = TempDir::new().expect("tempdir");
    let (path, _) = seed_unresolved_quarantine(&temp);
    let connection = Connection::open(&path).expect("open raw store");
    let first_manifest: String = connection
        .query_row("SELECT manifest FROM quarantine LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("first manifest");
    let mut second_manifest: serde_json::Value =
        serde_json::from_str(&first_manifest).expect("parse first manifest");
    second_manifest["id"] = serde_json::Value::String("second".to_owned());
    let second_directory = temp.path().join("quarantine/second");
    fs::create_dir_all(&second_directory).expect("second quarantine directory");
    fs::write(
        second_directory.join("manifest.json"),
        serde_json::to_vec_pretty(&second_manifest).expect("encode second manifest"),
    )
    .expect("second manifest file");
    connection
        .execute(
            "INSERT INTO quarantine(id,manifest,durable_unresolved) VALUES ('second',?1,1)",
            [serde_json::to_string_pretty(&second_manifest).expect("encode manifest row")],
        )
        .expect("second quarantine row");
    connection
        .execute_batch(
            "CREATE TRIGGER interrupt_quarantine_resolution
             BEFORE UPDATE OF durable_unresolved ON quarantine
             WHEN OLD.id='second'
             BEGIN
                 SELECT RAISE(ABORT, 'interrupted resolution');
             END;",
        )
        .expect("resolution interruption");
    drop(connection);

    let store = runtime()
        .block_on(Store::open(&path))
        .expect("inspect two quarantines");
    let report = runtime()
        .block_on(store.quarantine_report())
        .expect("two-row report");
    runtime().block_on(store.close());
    assert_eq!(report.len(), 2);
    assert!(
        runtime()
            .block_on(Store::resolve_quarantine(&path, &report))
            .is_err()
    );
    let connection = Connection::open(&path).expect("inspect interrupted resolution");
    let unresolved: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM quarantine WHERE durable_unresolved=1",
            [],
            |row| row.get(0),
        )
        .expect("unresolved count");
    assert_eq!(
        unresolved, 2,
        "an interrupted resolution must clear no rows"
    );
    connection
        .execute("DROP TRIGGER interrupt_quarantine_resolution", [])
        .expect("remove interruption");
    drop(connection);

    runtime()
        .block_on(Store::resolve_quarantine(&path, &report))
        .expect("complete resolution");
    let connection = Connection::open(path).expect("inspect completed resolution");
    let unresolved: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM quarantine WHERE durable_unresolved=1",
            [],
            |row| row.get(0),
        )
        .expect("resolved count");
    assert_eq!(
        unresolved, 0,
        "a completed resolution must clear every confirmed row"
    );
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
