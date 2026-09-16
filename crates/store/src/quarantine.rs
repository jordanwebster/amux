use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fold::StoreError;
use serde::{Deserialize, Serialize};

const REQUEST_NAME: &str = "request.json";
const MANIFEST_NAME: &str = "manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuarantineRequest {
    database: String,
    durable_state: DurableState,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DurableState {
    PresentOrUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuarantineManifest {
    id: String,
    database: String,
    files: Vec<String>,
    durable_unresolved: bool,
    moved: bool,
    complete: bool,
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
            durable_unresolved: matches!(request.durable_state, DurableState::PresentOrUnknown),
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
        if !manifest.moved {
            manifest.moved = true;
            write_manifest(&manifest_path, &manifest)?;
            sync_directory(manifest_path.parent().ok_or(StoreError::Io)?)?;
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

pub(crate) fn request(database: &Path) -> Result<(), StoreError> {
    let root = quarantine_root(database)?;
    fs::create_dir_all(&root).map_err(map_io)?;
    let database = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(StoreError::Io)?;
    let request = QuarantineRequest {
        database: database.to_owned(),
        durable_state: DurableState::PresentOrUnknown,
    };
    atomic_write(
        &root.join(REQUEST_NAME),
        &serde_json::to_vec_pretty(&request).map_err(|_| StoreError::Io)?,
    )?;
    sync_directory(&root)
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

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(map_io)
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
