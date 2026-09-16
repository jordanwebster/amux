use fold::StoreError;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::db::{admit_growth, map_sqlite_error, set_synchronous, unix_seconds};

pub(crate) fn get(
    connection: &mut Connection,
    kind: &str,
    key: &str,
) -> Result<Option<String>, StoreError> {
    durable(connection, |transaction| {
        transaction
            .query_row(
                "SELECT value FROM view_state WHERE kind=?1 AND key=?2",
                params![kind, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite_error)
    })
}

pub(crate) fn set(
    connection: &mut Connection,
    kind: &str,
    key: &str,
    value: &str,
) -> Result<(), StoreError> {
    let growth = u64::try_from(
        kind.len()
            .saturating_add(key.len())
            .saturating_add(value.len()),
    )
    .map_err(|_| StoreError::DiskFull)?;
    durable(connection, |transaction| {
        admit_growth(transaction, growth)?;
        transaction
            .execute(
                "INSERT INTO view_state(kind,key,value,updated_at) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(kind,key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                params![kind, key, value, unix_seconds()],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    })
}

fn durable<T>(
    connection: &mut Connection,
    operation: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    set_synchronous(connection, "FULL")?;
    let result = (|| {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;
        let unresolved: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM quarantine WHERE durable_unresolved=1)",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if unresolved {
            return Err(StoreError::RecoveryRequired);
        }
        let value = operation(&transaction)?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(value)
    })();
    let restored = set_synchronous(connection, "NORMAL");
    match (result, restored) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}
