use std::path::{Path, PathBuf};

use amux_artifacts::{ArtifactId, ArtifactKind, ArtifactMeta, Owner, StoreError};
use serde::{Deserialize, Serialize};

use super::Agent;
use crate::ProtocolError;

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
