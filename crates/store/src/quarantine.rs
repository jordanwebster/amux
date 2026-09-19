use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fold::StoreError;
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::db::{map_sqlite_error, set_synchronous};
use crate::families::{REGISTRY, Regime};

const REQUEST_NAME: &str = "request.json";
const MANIFEST_NAME: &str = "manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuarantineRequest {
    database: String,
    durable_state: QuarantineDurableState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "families")]
pub enum QuarantineDurableState {
    Present(Vec<String>),
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuarantineManifest {
    id: String,
    database: String,
    files: Vec<String>,
    #[serde(default)]
    moved_files: Vec<String>,
    durable_state: QuarantineDurableState,
    durable_unresolved: bool,
    moved: bool,
    complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineRecord {
    pub id: String,
    pub manifest: PathBuf,
    pub database: String,
    pub named_files: Vec<String>,
    pub durable_state: QuarantineDurableState,
    pub moved_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuarantineReport {
    pub quarantines: Vec<QuarantineRecord>,
}

impl QuarantineReport {
    pub fn is_empty(&self) -> bool {
        self.quarantines.is_empty()
    }

    pub fn len(&self) -> usize {
        self.quarantines.len()
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.quarantines
            .iter()
            .map(|quarantine| quarantine.id.clone())
            .collect()
    }
}

impl fmt::Display for QuarantineReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, quarantine) in self.quarantines.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }
            writeln!(formatter, "Quarantine {}:", quarantine.id)?;
            writeln!(formatter, "  manifest: {}", quarantine.manifest.display())?;
            writeln!(formatter, "  database: {}", quarantine.database)?;
            writeln!(
                formatter,
                "  files named by manifest: {}",
                quarantine.named_files.join(", ")
            )?;
            match &quarantine.durable_state {
                QuarantineDurableState::Present(families) => writeln!(
                    formatter,
                    "  durable families: present ({})",
                    families.join(", ")
                )?,
                QuarantineDurableState::Unknown => {
                    writeln!(formatter, "  durable families: could not be determined")?
                }
            }
            writeln!(formatter, "  moved files:")?;
            for path in &quarantine.moved_files {
                writeln!(formatter, "    {}", path.display())?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingQuarantine {
    pub id: String,
    pub manifest_json: String,
    pub durable_unresolved: bool,
    manifest_path: PathBuf,
}

pub(crate) fn quarantine_root(database: &Path) -> Result<PathBuf, StoreError> {
    let parent = database.parent().ok_or(StoreError::Io)?;
    Ok(parent.join("quarantine"))
}

pub(crate) fn has_pending(database: &Path) -> Result<bool, StoreError> {
    let root = quarantine_root(database)?;
    if root.join(REQUEST_NAME).exists() {
        return Ok(true);
    }
    if !root.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(root).map_err(map_io)? {
        let entry = entry.map_err(map_io)?;
        let manifest_path = entry.path().join(MANIFEST_NAME);
        if !manifest_path.exists() {
            continue;
        }
        let manifest = read_manifest(&manifest_path)?;
        if !manifest.complete {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn prepare_pending(database: &Path) -> Result<Vec<PendingQuarantine>, StoreError> {
    let root = quarantine_root(database)?;
    fs::create_dir_all(&root).map_err(map_io)?;
    let request_path = root.join(REQUEST_NAME);

    let mut manifests = incomplete_manifests(&root)?;
    if manifests.is_empty() && request_path.exists() {
        let request: QuarantineRequest =
            serde_json::from_slice(&fs::read(&request_path).map_err(map_io)?)
                .map_err(|_| StoreError::Io)?;
        let id = unique_id();
        let directory = root.join(&id);
        fs::create_dir(&directory).map_err(map_io)?;
        let files = database_files(database)?;
        let manifest = QuarantineManifest {
            id,
            database: request.database,
            files,
            moved_files: Vec::new(),
            durable_state: request.durable_state,
            durable_unresolved: true,
            moved: false,
            complete: false,
        };
        let manifest_path = directory.join(MANIFEST_NAME);
        write_manifest(&manifest_path, &manifest)?;
        sync_directory(&directory)?;
        sync_directory(&root)?;
        manifests.push((manifest_path, manifest));
    }

    let mut pending = Vec::new();
    for (manifest_path, mut manifest) in manifests {
        move_manifest_files(database, &manifest_path, &manifest)?;
        let destination = manifest_path.parent().ok_or(StoreError::Io)?;
        let moved_files = manifest
            .files
            .iter()
            .filter(|name| destination.join(name).exists())
            .cloned()
            .collect::<Vec<_>>();
        if !manifest.moved || manifest.moved_files != moved_files {
            manifest.moved = true;
            manifest.moved_files = moved_files;
            write_manifest(&manifest_path, &manifest)?;
            sync_directory(destination)?;
        }
        let mut completed_manifest = manifest.clone();
        completed_manifest.complete = true;
        pending.push(PendingQuarantine {
            id: manifest.id.clone(),
            manifest_json: serde_json::to_string_pretty(&completed_manifest)
                .map_err(|_| StoreError::Io)?,
            durable_unresolved: manifest.durable_unresolved,
            manifest_path,
        });
    }

    if request_path.exists() {
        fs::remove_file(&request_path).map_err(map_io)?;
        sync_directory(&root)?;
    }
    Ok(pending)
}

pub(crate) fn finish_pending(pending: &[PendingQuarantine]) -> Result<(), StoreError> {
    for pending in pending {
        let mut manifest = read_manifest(&pending.manifest_path)?;
        manifest.complete = true;
        write_manifest(&pending.manifest_path, &manifest)?;
        sync_directory(pending.manifest_path.parent().ok_or(StoreError::Io)?)?;
    }
    Ok(())
}

pub(crate) fn known_durable_state() -> QuarantineDurableState {
    QuarantineDurableState::Present(
        REGISTRY
            .families()
            .iter()
            .filter(|family| matches!(family.regime, Regime::Durable { .. }))
            .map(|family| family.name.to_owned())
            .collect(),
    )
}

pub(crate) fn request(
    database: &Path,
    durable_state: QuarantineDurableState,
) -> Result<(), StoreError> {
    let root = quarantine_root(database)?;
    fs::create_dir_all(&root).map_err(map_io)?;
    let database = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(StoreError::Io)?;
    let request = QuarantineRequest {
        database: database.to_owned(),
        durable_state,
    };
    atomic_write(
        &root.join(REQUEST_NAME),
        &serde_json::to_vec_pretty(&request).map_err(|_| StoreError::Io)?,
    )?;
    sync_directory(&root)
}

pub(crate) fn inspect(
    connection: &Connection,
    database: &Path,
) -> Result<QuarantineReport, StoreError> {
    let root = quarantine_root(database)?;
    let mut statement = connection
        .prepare("SELECT id, manifest FROM quarantine WHERE durable_unresolved=1 ORDER BY id")
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;

    let mut quarantines = Vec::with_capacity(rows.len());
    for (id, encoded) in rows {
        let manifest: QuarantineManifest =
            serde_json::from_str(&encoded).map_err(|_| StoreError::Corrupt)?;
        if manifest.id != id || !manifest.moved || !manifest.complete {
            return Err(StoreError::Corrupt);
        }
        let directory = root.join(&manifest.id);
        for name in &manifest.files {
            validate_file_name(name)?;
        }
        let mut moved_files = Vec::with_capacity(manifest.moved_files.len());
        for name in &manifest.moved_files {
            validate_file_name(name)?;
            if !manifest.files.contains(name) {
                return Err(StoreError::Corrupt);
            }
            moved_files.push(directory.join(name));
        }
        quarantines.push(QuarantineRecord {
            id: manifest.id,
            manifest: directory.join(MANIFEST_NAME),
            database: manifest.database,
            named_files: manifest.files,
            durable_state: manifest.durable_state,
            moved_files,
        });
    }
    Ok(QuarantineReport { quarantines })
}

fn validate_file_name(name: &str) -> Result<(), StoreError> {
    let path = Path::new(name);
    if path.file_name().and_then(|value| value.to_str()) != Some(name) {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

pub(crate) fn resolve(
    connection: &mut Connection,
    expected_ids: &[String],
) -> Result<(), StoreError> {
    if expected_ids.is_empty() {
        return Err(StoreError::Invalid);
    }
    set_synchronous(connection, "FULL")?;
    let result = (|| {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;
        let mut statement = transaction
            .prepare("SELECT id FROM quarantine WHERE durable_unresolved=1 ORDER BY id")
            .map_err(map_sqlite_error)?;
        let current = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        drop(statement);

        let mut expected = expected_ids.to_vec();
        expected.sort();
        if current != expected {
            return Err(StoreError::Invalid);
        }
        transaction
            .execute(
                "UPDATE quarantine SET durable_unresolved=0 WHERE durable_unresolved=1",
                [],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)
    })();
    let restored = set_synchronous(connection, "NORMAL");
    match (result, restored) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn incomplete_manifests(root: &Path) -> Result<Vec<(PathBuf, QuarantineManifest)>, StoreError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut manifests = Vec::new();
    for entry in fs::read_dir(root).map_err(map_io)? {
        let entry = entry.map_err(map_io)?;
        let path = entry.path().join(MANIFEST_NAME);
        if !path.exists() {
            continue;
        }
        let manifest = read_manifest(&path)?;
        if !manifest.complete {
            manifests.push((path, manifest));
        }
    }
    manifests.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(manifests)
}

fn move_manifest_files(
    database: &Path,
    manifest_path: &Path,
    manifest: &QuarantineManifest,
) -> Result<(), StoreError> {
    let parent = database.parent().ok_or(StoreError::Io)?;
    let destination = manifest_path.parent().ok_or(StoreError::Io)?;
    for name in &manifest.files {
        let source = parent.join(name);
        let target = destination.join(name);
        match (source.exists(), target.exists()) {
            (true, false) => fs::rename(&source, &target).map_err(map_io)?,
            (false, true) | (false, false) => {}
            (true, true) => return Err(StoreError::Io),
        }
    }
    sync_directory(parent)?;
    sync_directory(destination)
}

fn database_files(database: &Path) -> Result<Vec<String>, StoreError> {
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(StoreError::Io)?;
    Ok(vec![
        name.to_owned(),
        format!("{name}-wal"),
        format!("{name}-shm"),
    ])
}

fn read_manifest(path: &Path) -> Result<QuarantineManifest, StoreError> {
    serde_json::from_slice(&fs::read(path).map_err(map_io)?).map_err(|_| StoreError::Io)
}

fn write_manifest(path: &Path, manifest: &QuarantineManifest) -> Result<(), StoreError> {
    atomic_write(
        path,
        &serde_json::to_vec_pretty(manifest).map_err(|_| StoreError::Io)?,
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(map_io)?;
    file.write_all(bytes).map_err(map_io)?;
    file.sync_all().map_err(map_io)?;
    fs::rename(&temporary, path).map_err(map_io)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(map_io)
}

/// Windows cannot open a directory as a file to flush it, and NTFS journals
/// the rename itself, so there is nothing further to make durable.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

fn map_io(error: std::io::Error) -> StoreError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => StoreError::Permission,
        _ => StoreError::Io,
    }
}
