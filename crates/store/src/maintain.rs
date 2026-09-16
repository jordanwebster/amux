use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fold::StoreError;
use rusqlite::Connection;

use crate::db::{is_interrupted, map_sqlite_error, quote_identifier};
use crate::families::REGISTRY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub retired_rows_per_table: usize,
    pub vacuum_steps: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            retired_rows_per_table: 1_024,
            vacuum_steps: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub retired_rows_deleted: usize,
    pub retired_tables_dropped: usize,
    pub quick_check_complete: bool,
    pub checkpoint_complete: bool,
    pub vacuum_steps: usize,
    pub deadline_reached: bool,
}

pub(crate) fn run(
    connection: &Connection,
    budget: Budget,
    deadline: Duration,
) -> Result<MaintenanceReport, StoreError> {
    if deadline.is_zero() {
        return Ok(MaintenanceReport {
            deadline_reached: true,
            ..MaintenanceReport::default()
        });
    }
    let started = Instant::now();
    let finished = Arc::new(AtomicBool::new(false));
    let timer_finished = Arc::clone(&finished);
    let interrupt = connection.get_interrupt_handle();
    std::thread::spawn(move || {
        std::thread::sleep(deadline);
        if !timer_finished.load(Ordering::Acquire) {
            interrupt.interrupt();
        }
    });

    let result = run_inner(connection, budget, deadline, started);
    finished.store(true, Ordering::Release);
    result
}

fn run_inner(
    connection: &Connection,
    budget: Budget,
    deadline: Duration,
    started: Instant,
) -> Result<MaintenanceReport, StoreError> {
    let mut report = MaintenanceReport::default();
    let retired_tables = match retired_table_names(connection) {
        Ok(tables) => tables,
        Err(error) if is_interrupted(&error) => {
            report.deadline_reached = true;
            return Ok(report);
        }
        Err(error) => return Err(map_sqlite_error(error)),
    };

    for table in retired_tables {
        if expired(started, deadline) {
            report.deadline_reached = true;
            return Ok(report);
        }
        if budget.retired_rows_per_table == 0 {
            continue;
        }
        let sql = format!(
            "DELETE FROM {} WHERE rowid IN (SELECT rowid FROM {} LIMIT ?1)",
            quote_identifier(&table),
            quote_identifier(&table)
        );
        let deleted = match execute_retry_busy(connection, &sql, budget.retired_rows_per_table) {
            Ok(deleted) => deleted,
            Err(error) if is_interrupted(&error) => {
                report.deadline_reached = true;
                return Ok(report);
            }
            Err(error) => return Err(map_sqlite_error(error)),
        };
        report.retired_rows_deleted += deleted;
        if deleted < budget.retired_rows_per_table {
            match connection.execute_batch(&format!("DROP TABLE {}", quote_identifier(&table))) {
                Ok(()) => {}
                Err(error) if is_interrupted(&error) => {
                    report.deadline_reached = true;
                    return Ok(report);
                }
                Err(error) => return Err(map_sqlite_error(error)),
            }
            report.retired_tables_dropped += 1;
        }
    }

    if expired(started, deadline) {
        report.deadline_reached = true;
        return Ok(report);
    }
    let check = quick_check(connection);
    match check {
        Ok(true) => report.quick_check_complete = true,
        Ok(false) => return Err(StoreError::Corrupt),
        Err(error) if is_interrupted(&error) => {
            report.deadline_reached = true;
            return Ok(report);
        }
        Err(error) => return Err(map_sqlite_error(error)),
    }

    if expired(started, deadline) {
        report.deadline_reached = true;
        return Ok(report);
    }
    let checkpoint = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    match checkpoint {
        Ok((busy, _, _)) => report.checkpoint_complete = busy == 0,
        Err(error) if is_interrupted(&error) => {
            report.deadline_reached = true;
            return Ok(report);
        }
        Err(error) => return Err(map_sqlite_error(error)),
    }

    for _ in 0..budget.vacuum_steps {
        if expired(started, deadline) {
            report.deadline_reached = true;
            break;
        }
        match connection.execute_batch("PRAGMA incremental_vacuum(200)") {
            Ok(()) => report.vacuum_steps += 1,
            Err(error) if is_interrupted(&error) => {
                report.deadline_reached = true;
                break;
            }
            Err(error) => return Err(map_sqlite_error(error)),
        }
    }
    Ok(report)
}

fn quick_check(connection: &Connection) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare("PRAGMA quick_check")?;
    let mut rows = statement.query([])?;
    let mut ok = true;
    while let Some(row) = rows.next()? {
        ok &= row.get::<_, String>(0)? == "ok";
    }
    Ok(ok)
}

fn retired_table_names(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names
        .into_iter()
        .filter(|name| {
            REGISTRY.families().iter().any(|family| {
                family.tables.iter().any(|table| {
                    name.strip_prefix(&format!("{table}_old_"))
                        .is_some_and(|generation| generation.parse::<u64>().is_ok())
                })
            })
        })
        .collect())
}

fn execute_retry_busy(connection: &Connection, sql: &str, limit: usize) -> rusqlite::Result<usize> {
    match connection.execute(sql, [limit as i64]) {
        Err(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) =>
        {
            connection.execute(sql, [limit as i64])
        }
        result => result,
    }
}

fn expired(started: Instant, deadline: Duration) -> bool {
    started.elapsed() >= deadline
}
