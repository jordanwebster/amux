use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fold::{Generation, Generations, StoreError};
use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::families::{Family, REGISTRY, Regime};
use crate::quarantine::PendingQuarantine;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_SIZE_LIMIT: i64 = 16 * 1024 * 1024;

#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) const STORE_TARGET_BYTES: u64 = 200 * 1024 * 1024;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub(crate) const STORE_TARGET_BYTES: u64 = 500 * 1024 * 1024;
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) const STORE_CEILING_BYTES: u64 = 250 * 1024 * 1024;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub(crate) const STORE_CEILING_BYTES: u64 = 600 * 1024 * 1024;
pub(crate) const STORE_RESERVE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryReport {
    pub version: String,
    pub source_id: String,
    pub compile_options: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenReport {
    pub vm_steps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreGenerations {
    pub fleet: Generation,
    pub chat: Generation,
    pub claude_pty: Generation,
    pub claude_sdk: Generation,
    pub codex: Generation,
}

impl StoreGenerations {
    pub fn for_provider(self, provider: &'static str) -> Option<Generations> {
        let provider = match provider {
            "claude_pty" => self.claude_pty,
            "claude_sdk" => self.claude_sdk,
            "codex" => self.codex,
            _ => return None,
        };
        Some(Generations {
            fleet: self.fleet,
            chat: self.chat,
            provider,
        })
    }
}

pub(crate) struct OpenedDatabase {
    pub connection: Connection,
    pub generations: StoreGenerations,
    pub library: LibraryReport,
    pub open_report: OpenReport,
}

pub(crate) fn open_database(
    path: &Path,
    pending_quarantines: &[PendingQuarantine],
) -> Result<OpenedDatabase, StoreError> {
    let new_file = !path.exists() || path.metadata().map(|meta| meta.len() == 0).unwrap_or(false);
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(map_sqlite_error)?;

    const PROGRESS_INTERVAL: i32 = 100;
    let progress_calls = Arc::new(AtomicU64::new(0));
    let counted_calls = Arc::clone(&progress_calls);
    connection
        .progress_handler(
            PROGRESS_INTERVAL,
            Some(move || {
                counted_calls.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .map_err(map_sqlite_error)?;

    configure(&connection, new_file)?;
    let library = qualify_library(&connection)?;
    let generations = match initialize_once(&mut connection, pending_quarantines) {
        Err(StoreError::Busy) => initialize_once(&mut connection, pending_quarantines)?,
        result => result?,
    };
    set_synchronous(&connection, "NORMAL")?;
    connection
        .progress_handler(0, None::<fn() -> bool>)
        .map_err(map_sqlite_error)?;
    let open_report = OpenReport {
        vm_steps: progress_calls
            .load(Ordering::Relaxed)
            .saturating_mul(PROGRESS_INTERVAL as u64),
    };

    Ok(OpenedDatabase {
        connection,
        generations,
        library,
        open_report,
    })
}

fn configure(connection: &Connection, new_file: bool) -> Result<(), StoreError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(map_sqlite_error)?;

    if new_file {
        connection
            .execute_batch("PRAGMA auto_vacuum=INCREMENTAL")
            .map_err(map_sqlite_error)?;
        let mode: i64 = connection
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if mode != 2 {
            return Err(StoreError::UnsupportedFormat);
        }
    }

    let journal: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::UnsupportedFormat);
    }

    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;")
        .map_err(map_sqlite_error)?;
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let temp_store: i64 = connection
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let journal_limit: i64 = connection
        .query_row(
            &format!("PRAGMA journal_size_limit={JOURNAL_SIZE_LIMIT}"),
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    let auto_vacuum: i64 = connection
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    if foreign_keys != 1
        || busy_timeout != BUSY_TIMEOUT.as_millis() as i64
        || temp_store != 2
        || journal_limit != JOURNAL_SIZE_LIMIT
        || auto_vacuum != 2
    {
        return Err(StoreError::UnsupportedFormat);
    }
    Ok(())
}

pub fn qualify_library(connection: &Connection) -> Result<LibraryReport, StoreError> {
    let version: String = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let source_id: String = connection
        .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let mut statement = connection
        .prepare("PRAGMA compile_options")
        .map_err(map_sqlite_error)?;
    let compile_options = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;

    tracing::info!(
        sqlite_version = version,
        sqlite_source_id = source_id,
        compile_options = ?compile_options,
        "qualifying SQLite library"
    );

    let json_works: i64 = connection
        .query_row("SELECT json_valid('{\"qualified\":true}')", [], |row| {
            row.get(0)
        })
        .map_err(map_sqlite_error)?;
    if !library_is_qualified(&version, &compile_options, json_works == 1) {
        return Err(StoreError::UnsupportedFormat);
    }

    Ok(LibraryReport {
        version,
        source_id,
        compile_options,
    })
}

fn library_is_qualified(version: &str, compile_options: &[String], json_works: bool) -> bool {
    let Some(tuple) = parse_version(version) else {
        return false;
    };
    let has_wal_fix = tuple >= (3, 51, 3) || tuple == (3, 50, 7) || tuple == (3, 44, 6);
    let omitted_required = ["OMIT_AUTOVACUUM", "OMIT_FOREIGN_KEY", "OMIT_WAL"]
        .iter()
        .any(|required| compile_options.iter().any(|option| option == required));
    tuple >= (3, 43, 0) && has_wal_fix && !omitted_required && json_works
}

fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut pieces = version.split('.');
    Some((
        pieces.next()?.parse().ok()?,
        pieces.next()?.parse().ok()?,
        pieces.next()?.parse().ok()?,
    ))
}

fn initialize_once(
    connection: &mut Connection,
    pending_quarantines: &[PendingQuarantine],
) -> Result<StoreGenerations, StoreError> {
    set_synchronous(connection, "FULL")?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_error)?;
    bootstrap_meta(&transaction)?;
    validate_ledger(&transaction)?;
    apply_durable_migrations(&transaction)?;
    rebuild_derived_families(&transaction)?;
    record_quarantines(&transaction, pending_quarantines)?;
    let generations = read_generations(&transaction)?;
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(generations)
}

pub(crate) fn set_synchronous(connection: &Connection, value: &str) -> Result<(), StoreError> {
    connection
        .execute_batch(&format!("PRAGMA synchronous={value}"))
        .map_err(map_sqlite_error)?;
    let actual: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let expected = if value == "FULL" { 2 } else { 1 };
    if actual != expected {
        return Err(StoreError::UnsupportedFormat);
    }
    Ok(())
}

pub(crate) fn store_bytes(connection: &Connection) -> Result<u64, StoreError> {
    let mut statement = connection
        .prepare("PRAGMA database_list")
        .map_err(map_sqlite_error)?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    let mut total = 0u64;
    for path in paths.into_iter().filter(|path| !path.is_empty()) {
        for candidate in [path.clone(), format!("{path}-wal"), format!("{path}-shm")] {
            match std::fs::metadata(candidate) {
                Ok(metadata) => total = total.saturating_add(metadata.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Err(StoreError::Permission);
                }
                Err(_) => return Err(StoreError::Io),
            }
        }
    }
    Ok(total)
}

pub(crate) fn admit_growth(connection: &Connection, growth: u64) -> Result<(), StoreError> {
    let admission = STORE_CEILING_BYTES.saturating_sub(STORE_RESERVE_BYTES);
    if store_bytes(connection)?.saturating_add(growth) >= admission {
        Err(StoreError::DiskFull)
    } else {
        Ok(())
    }
}

fn bootstrap_meta(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let has_ledger: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='migration')",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    if has_ledger {
        return Ok(());
    }

    let meta = REGISTRY.get("meta").expect("meta family is registered");
    let Regime::Durable { migrations } = meta.regime else {
        unreachable!("meta is durable")
    };
    let migration = &migrations[0];
    transaction
        .execute_batch(migration.sql)
        .map_err(map_sqlite_error)?;
    let now = unix_seconds();
    transaction
        .execute(
            "INSERT INTO migration(family, id, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            params![meta.name, migration.id, checksum(migration.sql), now],
        )
        .map_err(map_sqlite_error)?;
    transaction
        .execute(
            "INSERT INTO family_shape(family, shape, generation, applied_at) VALUES (?1, ?2, 1, ?3)",
            params![meta.name, meta.shape, now],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn validate_ledger(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare("SELECT family, id, checksum FROM migration ORDER BY family, id")
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    let mut next_id = BTreeMap::<String, u32>::new();
    for (family_name, id, applied_checksum) in rows {
        let family = REGISTRY
            .get(&family_name)
            .ok_or(StoreError::UnsupportedFormat)?;
        let Regime::Durable { migrations } = family.regime else {
            return Err(StoreError::UnsupportedFormat);
        };
        let expected_id = next_id.entry(family_name.clone()).or_insert(1);
        if id != *expected_id {
            return Err(StoreError::UnsupportedFormat);
        }
        let migration = migrations
            .iter()
            .find(|migration| migration.id == id)
            .ok_or(StoreError::UnsupportedFormat)?;
        if applied_checksum != checksum(migration.sql) {
            return Err(StoreError::UnsupportedFormat);
        }
        *expected_id += 1;
    }

    let mut shapes = transaction
        .prepare("SELECT family, shape FROM family_shape")
        .map_err(map_sqlite_error)?;
    let shape_rows = shapes
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    for (family_name, shape) in shape_rows {
        let family = REGISTRY
            .get(&family_name)
            .ok_or(StoreError::UnsupportedFormat)?;
        if shape > family.shape {
            return Err(StoreError::UnsupportedFormat);
        }
    }
    Ok(())
}

fn apply_durable_migrations(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    for family in REGISTRY.families() {
        let Regime::Durable { migrations } = family.regime else {
            continue;
        };
        let applied: u32 = transaction
            .query_row(
                "SELECT COUNT(*) FROM migration WHERE family=?1",
                [family.name],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        for migration in migrations.iter().skip(applied as usize) {
            tracing::info!(
                family = family.name,
                migration = migration.id,
                "applying durable store migration"
            );
            transaction
                .execute_batch(migration.sql)
                .map_err(map_sqlite_error)?;
            transaction
                .execute(
                    "INSERT INTO migration(family, id, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
                    params![family.name, migration.id, checksum(migration.sql), unix_seconds()],
                )
                .map_err(map_sqlite_error)?;
        }
        transaction
            .execute(
                "INSERT INTO family_shape(family, shape, generation, applied_at) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(family) DO UPDATE SET shape=excluded.shape, applied_at=excluded.applied_at",
                params![family.name, family.shape, unix_seconds()],
            )
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn rebuild_derived_families(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let mut current = BTreeMap::<String, (u32, u64)>::new();
    {
        let mut statement = transaction
            .prepare("SELECT family, shape, generation FROM family_shape")
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, u32>(1)?,
                        u64::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Integer,
                                Box::new(error),
                            )
                        })?,
                    ),
                ))
            })
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        current.extend(rows);
    }

    let mut rebuild = BTreeSet::<&'static str>::new();
    for family in REGISTRY.families() {
        if !matches!(family.regime, Regime::Derived) {
            continue;
        }
        if current.get(family.name).map(|row| row.0) != Some(family.shape) {
            add_retirement_closure(family, &mut rebuild);
        }
    }

    for family in REGISTRY.families() {
        if !rebuild.contains(family.name) {
            continue;
        }
        let old_generation = current.get(family.name).map_or(0, |row| row.1);
        if old_generation > 0 {
            retire_tables(transaction, family, old_generation)?;
        }
        let generation = old_generation
            .checked_add(1)
            .ok_or(StoreError::UnsupportedFormat)?;
        transaction
            .execute_batch(&family.ddl.replace("$GEN", &generation.to_string()))
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO family_shape(family, shape, generation, applied_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(family) DO UPDATE SET shape=excluded.shape, generation=excluded.generation, applied_at=excluded.applied_at",
                params![
                    family.name,
                    family.shape,
                    i64::try_from(generation).map_err(|_| StoreError::UnsupportedFormat)?,
                    unix_seconds()
                ],
            )
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn add_retirement_closure(family: &'static Family, rebuild: &mut BTreeSet<&'static str>) {
    if !rebuild.insert(family.name) {
        return;
    }
    for dependent in family.retires_with {
        let family = REGISTRY
            .get(dependent)
            .expect("retirement family is registered");
        add_retirement_closure(family, rebuild);
    }
}

fn retire_tables(
    transaction: &Transaction<'_>,
    family: &Family,
    generation: Generation,
) -> Result<(), StoreError> {
    for table in family.tables {
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [*table],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if !exists {
            continue;
        }
        let retired = format!("{table}_old_{generation}");
        transaction
            .execute_batch(&format!(
                "ALTER TABLE {} RENAME TO {}",
                quote_identifier(table),
                quote_identifier(&retired)
            ))
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn record_quarantines(
    transaction: &Transaction<'_>,
    pending: &[PendingQuarantine],
) -> Result<(), StoreError> {
    for quarantine in pending {
        transaction
            .execute(
                "INSERT OR REPLACE INTO quarantine(id, manifest, durable_unresolved) VALUES (?1, ?2, ?3)",
                params![
                    quarantine.id,
                    quarantine.manifest_json,
                    i64::from(quarantine.durable_unresolved)
                ],
            )
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn read_generations(transaction: &Transaction<'_>) -> Result<StoreGenerations, StoreError> {
    let read = |family: &str| {
        transaction
            .query_row(
                "SELECT generation FROM family_shape WHERE family=?1",
                [family],
                |row| {
                    let value = row.get::<_, i64>(0)?;
                    u64::try_from(value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })
                },
            )
            .map_err(map_sqlite_error)
    };
    Ok(StoreGenerations {
        fleet: read("fleet")?,
        chat: read("chat")?,
        claude_pty: read("claude_pty")?,
        claude_sdk: read("claude_sdk")?,
        codex: read("codex")?,
    })
}

pub(crate) fn checksum(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

pub(crate) fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) fn map_sqlite_error(error: rusqlite::Error) -> StoreError {
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => StoreError::Busy,
        Some(ErrorCode::DiskFull | ErrorCode::OutOfMemory | ErrorCode::TooBig) => {
            StoreError::DiskFull
        }
        Some(
            ErrorCode::PermissionDenied
            | ErrorCode::ReadOnly
            | ErrorCode::AuthorizationForStatementDenied
            | ErrorCode::CannotOpen,
        ) => StoreError::Permission,
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => StoreError::Corrupt,
        _ => StoreError::Io,
    }
}

pub(crate) fn is_interrupted(error: &rusqlite::Error) -> bool {
    error.sqlite_error_code() == Some(ErrorCode::OperationInterrupted)
}

pub(crate) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_checks_every_connection_pragma() {
        let directory = tempfile::tempdir().expect("tempdir");
        let connection =
            Connection::open(directory.path().join("store.sqlite")).expect("connection");
        configure(&connection, true).expect("configure");
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign keys");
        let busy_timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy timeout");
        let temp_store: i64 = connection
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .expect("temp store");
        let journal_limit: i64 = connection
            .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
            .expect("journal limit");
        let auto_vacuum: i64 = connection
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .expect("auto vacuum");
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);
        assert_eq!(temp_store, 2);
        assert_eq!(journal_limit, JOURNAL_SIZE_LIMIT);
        assert_eq!(auto_vacuum, 2);
    }

    #[test]
    fn lifecycle_maps_sqlite_result_codes() {
        let cases = [
            (rusqlite::ffi::SQLITE_BUSY, StoreError::Busy),
            (rusqlite::ffi::SQLITE_LOCKED, StoreError::Busy),
            (rusqlite::ffi::SQLITE_FULL, StoreError::DiskFull),
            (rusqlite::ffi::SQLITE_PERM, StoreError::Permission),
            (rusqlite::ffi::SQLITE_READONLY, StoreError::Permission),
            (rusqlite::ffi::SQLITE_CANTOPEN, StoreError::Permission),
            (rusqlite::ffi::SQLITE_CORRUPT, StoreError::Corrupt),
            (rusqlite::ffi::SQLITE_NOTADB, StoreError::Corrupt),
            (rusqlite::ffi::SQLITE_IOERR, StoreError::Io),
        ];
        for (code, expected) in cases {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert_eq!(map_sqlite_error(error), expected);
        }
    }

    #[test]
    fn lifecycle_parses_sqlite_versions() {
        assert_eq!(parse_version("3.53.2"), Some((3, 53, 2)));
        assert_eq!(parse_version("3.44"), None);
    }

    #[test]
    fn lifecycle_qualifies_only_sqlite_with_the_wal_fix_and_required_capabilities() {
        let options = Vec::new();
        assert!(library_is_qualified("3.53.2", &options, true));
        assert!(library_is_qualified("3.50.7", &options, true));
        assert!(library_is_qualified("3.44.6", &options, true));
        assert!(!library_is_qualified("3.51.2", &options, true));
        assert!(!library_is_qualified("3.42.0", &options, true));
        assert!(!library_is_qualified("3.53.2", &options, false));
        assert!(!library_is_qualified(
            "3.53.2",
            &["OMIT_WAL".to_owned()],
            true,
        ));
    }

    #[test]
    fn lifecycle_reports_the_runtime_sqlite_library() {
        let connection = Connection::open_in_memory().expect("connection");
        let version: String = connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .expect("version");
        let source_id: String = connection
            .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
            .expect("source id");
        let options = connection
            .prepare("PRAGMA compile_options")
            .expect("compile options statement")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("compile options")
            .collect::<Result<Vec<_>, _>>()
            .expect("compile option rows");
        println!("sqlite_version={version}");
        println!("sqlite_source_id={source_id}");
        println!("sqlite_compile_options={}", options.join(","));
    }
}
