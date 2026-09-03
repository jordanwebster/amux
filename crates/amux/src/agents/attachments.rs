use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use amux_artifacts::{ArtifactId, ArtifactKind, ArtifactMeta, Clock, Owner, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::Agent;
use crate::protocol::{ProtocolError, wire};

/// Metadata for an artifact without its owner-only lifetime fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl From<ArtifactMeta> for ArtifactRef {
    fn from(meta: ArtifactMeta) -> Self {
        Self {
            id: meta.id,
            kind: meta.kind,
            name: meta.name,
            mime: meta.mime,
            size: meta.size,
        }
    }
}

/// All authoritative artifact stores loaded by one daemon.
pub(crate) struct ArtifactOwners {
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    owners: RwLock<HashMap<Uuid, Arc<Owner>>>,
}

impl ArtifactOwners {
    /// Opens every artifact owner already present below the daemon's data directory.
    pub(crate) fn open(data_dir: PathBuf, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        let mut owners = HashMap::new();
        let agents_dir = data_dir.join("agents");
        match fs::read_dir(&agents_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() {
                        continue;
                    }
                    let Some(agent_id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| Uuid::parse_str(name).ok())
                    else {
                        continue;
                    };
                    let root = entry.path().join("artifacts");
                    if root.is_dir() {
                        owners.insert(agent_id, Arc::new(Owner::open(root, clock.clone())?));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            data_dir,
            clock,
            owners: RwLock::new(owners),
        })
    }

    /// Returns the loaded owner for an agent, opening it on first touch.
    pub(crate) fn owner(&self, agent_id: Uuid) -> Result<Arc<Owner>, ProtocolError> {
        if let Some(owner) = self
            .owners
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&agent_id)
            .cloned()
        {
            return Ok(owner);
        }

        let mut owners = self
            .owners
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(owner) = owners.get(&agent_id) {
            return Ok(owner.clone());
        }
        let owner = Arc::new(
            Owner::open(self.owner_root(agent_id), self.clock.clone()).map_err(store_error)?,
        );
        owners.insert(agent_id, owner.clone());
        Ok(owner)
    }

    /// Drops an agent's loaded owner and removes its complete artifact root.
    pub(crate) fn delete_agent(&self, agent_id: Uuid) -> Result<(), ProtocolError> {
        let owner = self
            .owners
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&agent_id);
        if let Some(owner) = owner
            && let Ok(owner) = Arc::try_unwrap(owner)
        {
            return owner.delete_all().map_err(store_error);
        }

        match fs::remove_dir_all(self.owner_root(agent_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(store_error(error.into())),
        }
    }

    /// Sweeps only owners already loaded into memory.
    pub(crate) fn sweep_loaded(&self, ttl: Duration) -> Result<Vec<ArtifactId>, ProtocolError> {
        let owners: Vec<_> = self
            .owners
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect();
        let mut swept = Vec::new();
        for owner in owners {
            swept.extend(owner.sweep(ttl).map_err(store_error)?);
        }
        Ok(swept)
    }

    fn owner_root(&self, agent_id: Uuid) -> PathBuf {
        self.data_dir
            .join("agents")
            .join(agent_id.to_string())
            .join("artifacts")
    }

    #[cfg(test)]
    pub(crate) fn loaded_count(&self) -> usize {
        self.owners
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

/// Runs the daemon's periodic sweep without rescanning the artifact tree.
pub(crate) fn spawn_artifact_sweeper(owners: Arc<ArtifactOwners>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
            match owners.sweep_loaded(amux_artifacts::EPHEMERAL_TTL) {
                Ok(swept) if !swept.is_empty() => {
                    tracing::info!(count = swept.len(), "swept ephemeral artifacts");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to sweep ephemeral artifacts");
                }
            }
        }
    })
}

/// Builds the synthetic stream row that introduces attachment metadata.
pub fn attachments_row(input_id: Option<&[u8]>, refs: &[ArtifactRef]) -> Value {
    json!({
        "type": "amux.attachments",
        "input_id": input_id.map(hex_bytes),
        "refs": refs,
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

pub(crate) fn artifact_ref_to_wire(artifact: &ArtifactRef) -> wire::ArtifactRef {
    wire::ArtifactRef {
        id: artifact.id.to_string(),
        kind: artifact_kind_to_wire(artifact.kind) as i32,
        name: artifact.name.clone(),
        mime: artifact.mime.clone(),
        size: artifact.size,
    }
}

pub(crate) fn artifact_ref_from_wire(
    artifact: wire::ArtifactRef,
) -> Result<ArtifactRef, wire::DecodeError> {
    Ok(ArtifactRef {
        id: ArtifactId::from_str(&artifact.id).map_err(|error| {
            wire::DecodeError::Invalid(format!("ArtifactRef.id is invalid: {error}"))
        })?,
        kind: artifact_kind_from_wire(artifact.kind)?,
        name: artifact.name,
        mime: artifact.mime,
        size: artifact.size,
    })
}

pub(crate) fn artifact_kind_to_wire(kind: ArtifactKind) -> wire::ArtifactKind {
    match kind {
        ArtifactKind::Image => wire::ArtifactKind::Image,
        ArtifactKind::File => wire::ArtifactKind::File,
        ArtifactKind::Diff => wire::ArtifactKind::Diff,
    }
}

pub(crate) fn artifact_kind_from_wire(kind: i32) -> Result<ArtifactKind, wire::DecodeError> {
    match wire::ArtifactKind::try_from(kind) {
        Ok(wire::ArtifactKind::Image) => Ok(ArtifactKind::Image),
        Ok(wire::ArtifactKind::File) => Ok(ArtifactKind::File),
        Ok(wire::ArtifactKind::Diff) => Ok(ArtifactKind::Diff),
        Ok(wire::ArtifactKind::Unspecified) | Err(_) => Err(wire::DecodeError::Invalid(format!(
            "invalid ArtifactKind value {kind}"
        ))),
    }
}

/// The repository state a diff compares.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffBase {
    WorkingTree,
    Branch { base: String },
}

/// The immutable repository identity captured with a diff.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BaseIdentity {
    pub base: DiffBase,
    pub head: String,
    pub merge_base: Option<String>,
    pub blobs: Vec<(String, String)>,
}

/// Addition and removal totals for one changed path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// A frozen patch and the repository identity it was computed from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffResponse {
    pub artifact: ArtifactRef,
    pub patch: String,
    pub identity: BaseIdentity,
    pub files: Vec<DiffFile>,
}

pub(crate) fn diff_base_to_wire(base: &DiffBase) -> wire::DiffBase {
    wire::DiffBase {
        base: Some(match base {
            DiffBase::WorkingTree => wire::diff_base::Base::WorkingTree(wire::Empty {}),
            DiffBase::Branch { base } => wire::diff_base::Base::Branch(base.clone()),
        }),
    }
}

pub(crate) fn diff_base_from_wire(base: wire::DiffBase) -> Result<DiffBase, wire::DecodeError> {
    match base.base {
        Some(wire::diff_base::Base::WorkingTree(_)) => Ok(DiffBase::WorkingTree),
        Some(wire::diff_base::Base::Branch(base)) if !base.is_empty() => {
            Ok(DiffBase::Branch { base })
        }
        Some(wire::diff_base::Base::Branch(_)) => Err(wire::DecodeError::Invalid(
            "DiffBase.branch must not be empty".to_string(),
        )),
        None => Err(wire::DecodeError::Invalid(
            "DiffBase.base is required".to_string(),
        )),
    }
}

pub(crate) fn diff_response_to_wire(response: &DiffResponse) -> wire::DiffResponse {
    wire::DiffResponse {
        artifact: Some(artifact_ref_to_wire(&response.artifact)),
        patch: response.patch.clone(),
        identity: Some(wire::BaseIdentity {
            base: Some(diff_base_to_wire(&response.identity.base)),
            head: response.identity.head.clone(),
            merge_base: response.identity.merge_base.clone(),
            blobs: response
                .identity
                .blobs
                .iter()
                .map(|(path, blob)| wire::PathBlob {
                    path: path.clone(),
                    blob: blob.clone(),
                })
                .collect(),
        }),
        files: response
            .files
            .iter()
            .map(|file| wire::DiffFile {
                path: file.path.clone(),
                added: file.added,
                removed: file.removed,
            })
            .collect(),
    }
}

pub(crate) fn diff_response_from_wire(
    response: wire::DiffResponse,
) -> Result<DiffResponse, wire::DecodeError> {
    let artifact = response.artifact.ok_or_else(|| {
        wire::DecodeError::Invalid("DiffResponse.artifact is required".to_string())
    })?;
    let identity = response.identity.ok_or_else(|| {
        wire::DecodeError::Invalid("DiffResponse.identity is required".to_string())
    })?;
    let base = identity
        .base
        .ok_or_else(|| wire::DecodeError::Invalid("BaseIdentity.base is required".to_string()))?;
    Ok(DiffResponse {
        artifact: artifact_ref_from_wire(artifact)?,
        patch: response.patch,
        identity: BaseIdentity {
            base: diff_base_from_wire(base)?,
            head: identity.head,
            merge_base: identity.merge_base,
            blobs: identity
                .blobs
                .into_iter()
                .map(|blob| (blob.path, blob.blob))
                .collect(),
        },
        files: response
            .files
            .into_iter()
            .map(|file| DiffFile {
                path: file.path,
                added: file.added,
                removed: file.removed,
            })
            .collect(),
    })
}

/// Computes and stores a frozen diff for any agent with a Git working directory.
pub(crate) async fn compute_diff(
    owner: &Owner,
    agent: &Agent,
    base: DiffBase,
) -> Result<DiffResponse, ProtocolError> {
    let head = git_stdout(
        &agent.working_dir,
        None,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolve HEAD",
    )
    .await?;
    let head = one_line(&head, "resolve HEAD")?;

    let (patch, files, merge_base, artifact_name) = match &base {
        DiffBase::WorkingTree => {
            let index = TemporaryIndex::new();
            git_stdout(
                &agent.working_dir,
                Some(index.path()),
                &["read-tree", "HEAD"],
                "prepare working-tree diff",
            )
            .await?;

            let untracked = git_stdout(
                &agent.working_dir,
                None,
                &["ls-files", "--others", "--exclude-standard", "-z"],
                "list untracked files",
            )
            .await?;
            let untracked = nul_paths(&untracked)?;
            if !untracked.is_empty() {
                let mut args = vec!["--literal-pathspecs", "add", "--intent-to-add", "--"];
                args.extend(untracked.iter().map(String::as_str));
                git_stdout(
                    &agent.working_dir,
                    Some(index.path()),
                    &args,
                    "prepare untracked files",
                )
                .await?;
            }

            let patch = diff_output(&agent.working_dir, Some(index.path()), &["HEAD"]).await?;
            let numstat = diff_numstat(&agent.working_dir, Some(index.path()), &["HEAD"]).await?;
            (
                patch,
                parse_numstat(&numstat)?,
                None,
                "working-tree.diff".to_string(),
            )
        }
        DiffBase::Branch { base } => {
            let merge_base = git_stdout(
                &agent.working_dir,
                None,
                &["merge-base", base, "HEAD"],
                "resolve branch merge base",
            )
            .await?;
            let merge_base = one_line(&merge_base, "resolve branch merge base")?;
            let patch = diff_output(&agent.working_dir, None, &[&merge_base, "HEAD"]).await?;
            let numstat = diff_numstat(&agent.working_dir, None, &[&merge_base, "HEAD"]).await?;
            (
                patch,
                parse_numstat(&numstat)?,
                Some(merge_base),
                format!("{base}.diff"),
            )
        }
    };

    let blobs = match &base {
        DiffBase::WorkingTree => working_tree_blobs(&agent.working_dir, &files).await?,
        DiffBase::Branch { .. } => head_blobs(&agent.working_dir, &files).await?,
    };
    let patch = String::from_utf8_lossy(&patch).into_owned();
    let artifact = owner
        .put(
            ArtifactKind::Diff,
            &artifact_name,
            "text/x-diff",
            patch.as_bytes(),
        )
        .map_err(store_error)?
        .into();

    Ok(DiffResponse {
        artifact,
        patch,
        identity: BaseIdentity {
            base,
            head,
            merge_base,
            blobs,
        },
        files,
    })
}

async fn diff_output(
    working_dir: &Path,
    index: Option<&Path>,
    revisions: &[&str],
) -> Result<Vec<u8>, ProtocolError> {
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
    ];
    args.extend_from_slice(revisions);
    args.push("--");
    git_stdout(working_dir, index, &args, "compute diff").await
}

async fn diff_numstat(
    working_dir: &Path,
    index: Option<&Path>,
    revisions: &[&str],
) -> Result<Vec<u8>, ProtocolError> {
    let mut args = vec!["diff", "--numstat", "-z", "--no-renames"];
    args.extend_from_slice(revisions);
    args.push("--");
    git_stdout(working_dir, index, &args, "compute diff magnitudes").await
}

async fn working_tree_blobs(
    working_dir: &Path,
    files: &[DiffFile],
) -> Result<Vec<(String, String)>, ProtocolError> {
    let mut blobs = Vec::new();
    for file in files {
        let path = working_dir.join(&file.path);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(diff_unavailable(format!(
                    "inspect `{}` for diff identity: {error}",
                    file.path
                )));
            }
        }
        let output = git_stdout(
            working_dir,
            None,
            &["hash-object", "--no-filters", "--", &file.path],
            "hash working-tree file",
        )
        .await?;
        blobs.push((
            file.path.clone(),
            one_line(&output, "hash working-tree file")?,
        ));
    }
    Ok(blobs)
}

async fn head_blobs(
    working_dir: &Path,
    files: &[DiffFile],
) -> Result<Vec<(String, String)>, ProtocolError> {
    let mut blobs = Vec::new();
    for file in files {
        let output = git_output(
            working_dir,
            None,
            &[
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "HEAD",
                "--",
                &file.path,
            ],
        )
        .await?;
        if !output.status.success() {
            return Err(git_failure("resolve HEAD blob", &output));
        }
        if output.stdout.is_empty() {
            continue;
        }
        let record = output
            .stdout
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        let header = record
            .split(|byte| *byte == b'\t')
            .next()
            .unwrap_or_default();
        let hash = header
            .split(|byte| *byte == b' ')
            .nth(2)
            .ok_or_else(|| diff_unavailable("Git returned a malformed tree entry"))?;
        let hash = std::str::from_utf8(hash)
            .map_err(|_| diff_unavailable("Git returned a non-UTF-8 blob id"))?;
        blobs.push((file.path.clone(), hash.to_string()));
    }
    Ok(blobs)
}

fn parse_numstat(output: &[u8]) -> Result<Vec<DiffFile>, ProtocolError> {
    output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let mut fields = record.splitn(3, |byte| *byte == b'\t');
            let added = parse_magnitude(fields.next(), "added")?;
            let removed = parse_magnitude(fields.next(), "removed")?;
            let path = fields
                .next()
                .ok_or_else(|| diff_unavailable("Git returned malformed numstat output"))?;
            let path = std::str::from_utf8(path)
                .map_err(|_| diff_unavailable("A changed path is not valid UTF-8"))?;
            Ok(DiffFile {
                path: path.to_string(),
                added,
                removed,
            })
        })
        .collect()
}

fn parse_magnitude(value: Option<&[u8]>, field: &str) -> Result<u32, ProtocolError> {
    let value = value.ok_or_else(|| diff_unavailable("Git returned malformed numstat output"))?;
    if value == b"-" {
        return Ok(0);
    }
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| diff_unavailable(format!("Git returned an invalid {field} magnitude")))
}

fn nul_paths(output: &[u8]) -> Result<Vec<String>, ProtocolError> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|_| diff_unavailable("An untracked path is not valid UTF-8"))
        })
        .collect()
}

async fn git_stdout(
    working_dir: &Path,
    index: Option<&Path>,
    args: &[&str],
    operation: &str,
) -> Result<Vec<u8>, ProtocolError> {
    let output = git_output(working_dir, index, args).await?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(git_failure(operation, &output))
    }
}

async fn git_output(
    working_dir: &Path,
    index: Option<&Path>,
    args: &[&str],
) -> Result<std::process::Output, ProtocolError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(working_dir)
        .env("GIT_PAGER", "cat");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
        .output()
        .await
        .map_err(|error| diff_unavailable(format!("run Git: {error}")))
}

fn one_line(output: &[u8], operation: &str) -> Result<String, ProtocolError> {
    let value = std::str::from_utf8(output)
        .map_err(|_| diff_unavailable(format!("{operation}: Git returned non-UTF-8 output")))?
        .trim();
    if value.is_empty() {
        Err(diff_unavailable(format!(
            "{operation}: Git returned no value"
        )))
    } else {
        Ok(value.to_string())
    }
}

fn git_failure(operation: &str, output: &std::process::Output) -> ProtocolError {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        diff_unavailable(format!("{operation}: Git exited with {}", output.status))
    } else {
        diff_unavailable(format!("{operation}: {detail}"))
    }
}

fn diff_unavailable(message: impl Into<String>) -> ProtocolError {
    ProtocolError::DiffUnavailable {
        message: message.into(),
    }
}

pub(crate) fn store_error(error: StoreError) -> ProtocolError {
    match error {
        StoreError::TooLarge { size, max } => ProtocolError::AttachmentTooLarge { size, max },
        StoreError::Missing { id } => ProtocolError::AttachmentMissing { id: id.to_string() },
        StoreError::Corrupt { id } => ProtocolError::ArtifactCorrupt { id: id.to_string() },
        StoreError::Fetch(error) => ProtocolError::ServerError {
            message: error.to_string(),
        },
        StoreError::Io(error) => ProtocolError::ServerError {
            message: error.to_string(),
        },
    }
}

struct TemporaryIndex {
    path: PathBuf,
}

impl TemporaryIndex {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "amux-diff-index-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            )),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let lock = PathBuf::from(format!("{}.lock", self.path.display()));
        let _ = std::fs::remove_file(lock);
    }
}

#[cfg(test)]
mod owners {
    use std::sync::Mutex;

    use amux_artifacts::{EPHEMERAL_TTL, id_of};
    use chrono::{DateTime, TimeDelta, Utc};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self(Mutex::new(now))
        }

        fn set(&self, now: DateTime<Utc>) {
            *self.0.lock().unwrap() = now;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn root(data_dir: &Path, agent_id: Uuid) -> PathBuf {
        data_dir
            .join("agents")
            .join(agent_id.to_string())
            .join("artifacts")
    }

    #[test]
    fn startup_loads_existing_owners_and_sweeps_only_their_indexes() {
        let data_dir = TempDir::new().unwrap();
        let started_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let clock = Arc::new(TestClock::new(started_at));
        let agent_ids = [Uuid::new_v4(), Uuid::new_v4()];
        for (index, agent_id) in agent_ids.iter().copied().enumerate() {
            let owner = Owner::open(root(data_dir.path(), agent_id), clock.clone()).unwrap();
            owner
                .put(
                    ArtifactKind::File,
                    &format!("file-{index}.txt"),
                    "text/plain",
                    format!("indexed-{index}").as_bytes(),
                )
                .unwrap();
        }

        clock.set(started_at + TimeDelta::hours(2));
        let owners = ArtifactOwners::open(data_dir.path().to_path_buf(), clock).unwrap();
        assert_eq!(owners.loaded_count(), 2);

        let unindexed_bytes = b"created after startup";
        let unindexed_id = id_of(unindexed_bytes);
        let unindexed_path = root(data_dir.path(), agent_ids[0])
            .join("blobs")
            .join(unindexed_id.as_str().strip_prefix("sha256:").unwrap());
        fs::write(&unindexed_path, unindexed_bytes).unwrap();

        let swept = owners.sweep_loaded(EPHEMERAL_TTL).unwrap();
        assert_eq!(swept.len(), 2);
        assert!(unindexed_path.exists());
        assert_eq!(owners.loaded_count(), 2);
    }

    #[test]
    fn first_touch_opens_an_owner_and_delete_removes_its_root() {
        let data_dir = TempDir::new().unwrap();
        let owners = ArtifactOwners::open(
            data_dir.path().to_path_buf(),
            Arc::new(amux_artifacts::SystemClock),
        )
        .unwrap();
        let agent_id = Uuid::new_v4();

        let owner = owners.owner(agent_id).unwrap();
        owner
            .put(ArtifactKind::File, "notes.txt", "text/plain", b"notes")
            .unwrap();
        drop(owner);
        assert_eq!(owners.loaded_count(), 1);
        assert!(root(data_dir.path(), agent_id).is_dir());

        owners.delete_agent(agent_id).unwrap();

        assert_eq!(owners.loaded_count(), 0);
        assert!(!root(data_dir.path(), agent_id).exists());
    }

    #[test]
    fn attachments_row_has_stable_shape_and_hex_input_id() {
        let artifact = ArtifactRef {
            id: id_of(b"image"),
            kind: ArtifactKind::Image,
            name: "screen.png".to_string(),
            mime: "image/png".to_string(),
            size: 5,
        };

        assert_eq!(
            attachments_row(Some(&[0x00, 0xaf, 0x10]), &[artifact.clone()]),
            json!({
                "type": "amux.attachments",
                "input_id": "00af10",
                "refs": [{
                    "id": artifact.id,
                    "kind": "image",
                    "name": "screen.png",
                    "mime": "image/png",
                    "size": 5
                }]
            })
        );
        assert_eq!(
            attachments_row(None, &[]),
            json!({"type": "amux.attachments", "input_id": null, "refs": []})
        );
    }
}

#[cfg(test)]
mod diff {
    use std::fs;
    use std::process::Command;
    use std::sync::Arc;

    use amux_artifacts::SystemClock;
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::AgentKind;

    fn git(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn test_agent(working_dir: &Path) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("diff-test".to_string()),
            command: "test-agent".to_string(),
            working_dir: working_dir.to_path_buf(),
            kind: AgentKind::TestAgent,
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        }
    }

    fn open_owner(root: &Path) -> Owner {
        Owner::open(root.to_path_buf(), Arc::new(SystemClock)).unwrap()
    }

    fn init_repository() -> (TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        git(directory.path(), &["init", "-q"]);
        git(directory.path(), &["config", "user.name", "amux test"]);
        git(
            directory.path(),
            &["config", "user.email", "amux@example.invalid"],
        );
        fs::write(directory.path().join("modified.txt"), "before\n").unwrap();
        fs::write(directory.path().join("deleted.txt"), "delete me\n").unwrap();
        git(directory.path(), &["add", "."]);
        git(directory.path(), &["commit", "-qm", "base"]);
        let base = git(directory.path(), &["rev-parse", "HEAD"]);
        (directory, base)
    }

    fn make_changes(directory: &Path) {
        fs::write(directory.join("modified.txt"), "after\nsecond\n").unwrap();
        fs::remove_file(directory.join("deleted.txt")).unwrap();
        fs::write(directory.join("untracked.txt"), "new\nsecond\n").unwrap();
    }

    fn expected_files() -> Vec<DiffFile> {
        vec![
            DiffFile {
                path: "deleted.txt".to_string(),
                added: 0,
                removed: 1,
            },
            DiffFile {
                path: "modified.txt".to_string(),
                added: 2,
                removed: 1,
            },
            DiffFile {
                path: "untracked.txt".to_string(),
                added: 2,
                removed: 0,
            },
        ]
    }

    fn expected_working_blobs(directory: &Path) -> Vec<(String, String)> {
        ["modified.txt", "untracked.txt"]
            .into_iter()
            .map(|path| {
                (
                    path.to_string(),
                    git(directory, &["hash-object", "--no-filters", "--", path]),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn working_tree_includes_modified_deleted_and_untracked_files() {
        let (directory, base) = init_repository();
        make_changes(directory.path());
        let owner_dir = tempfile::tempdir().unwrap();
        let owner = open_owner(&owner_dir.path().join("artifacts"));

        let response = compute_diff(&owner, &test_agent(directory.path()), DiffBase::WorkingTree)
            .await
            .unwrap();

        assert_eq!(response.files, expected_files());
        assert_eq!(response.identity.head, base);
        assert_eq!(response.identity.merge_base, None);
        assert_eq!(
            response.identity.blobs,
            expected_working_blobs(directory.path())
        );
        assert!(
            response
                .patch
                .contains("diff --git a/untracked.txt b/untracked.txt")
        );
        assert_eq!(response.artifact.kind, ArtifactKind::Diff);
        assert_eq!(response.artifact.name, "working-tree.diff");
        let meta = owner.meta(&response.artifact.id).unwrap();
        assert_eq!(meta.pinned_at, None);
        assert_eq!(
            owner.get(&response.artifact.id).unwrap().1,
            response.patch.as_bytes()
        );
    }

    #[tokio::test]
    async fn branch_uses_merge_base_and_head_with_new_side_blob_hashes() {
        let (directory, base) = init_repository();
        make_changes(directory.path());
        git(directory.path(), &["add", "-A"]);
        git(directory.path(), &["commit", "-qm", "branch changes"]);
        let head = git(directory.path(), &["rev-parse", "HEAD"]);
        let owner_dir = tempfile::tempdir().unwrap();
        let owner = open_owner(&owner_dir.path().join("artifacts"));

        let response = compute_diff(
            &owner,
            &test_agent(directory.path()),
            DiffBase::Branch { base: base.clone() },
        )
        .await
        .unwrap();

        assert_eq!(response.files, expected_files());
        assert_eq!(response.identity.head, head);
        assert_eq!(response.identity.merge_base, Some(base.clone()));
        assert_eq!(
            response.identity.blobs,
            expected_working_blobs(directory.path())
        );
        assert_eq!(response.artifact.kind, ArtifactKind::Diff);
        assert_eq!(response.artifact.name, format!("{base}.diff"));
        assert_eq!(owner.meta(&response.artifact.id).unwrap().pinned_at, None);
    }

    #[tokio::test]
    async fn non_git_directory_returns_diff_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let owner_dir = tempfile::tempdir().unwrap();
        let owner = open_owner(&owner_dir.path().join("artifacts"));

        let error = compute_diff(&owner, &test_agent(directory.path()), DiffBase::WorkingTree)
            .await
            .unwrap_err();

        assert!(matches!(error, ProtocolError::DiffUnavailable { .. }));
    }
}
