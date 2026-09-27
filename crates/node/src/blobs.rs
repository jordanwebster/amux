//! Blobs: content-addressed bytes whose only home is their agent's
//! directory, `agents/<id>/blobs/<sha256>` for an own agent and
//! `replicas/<host>/agents/<id>/blobs/<sha256>` for a peer's. There is no
//! table: name, mime and size ride on the attachment that references a
//! blob, and a blob lives and dies with its directory.
//!
//! PutBlob writes a person's attachment into the agent's directory before
//! the input that references it is handed over. GetBlob reads a file for a
//! client. Diff runs git in the agent's working directory and writes the
//! patch as a blob of the requesting agent, so it lives as long as that
//! agent whether or not a review is ever attached.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest as _, Sha256};
use store::{AgentRow, Store as _, StoreError};
use uuid::Uuid;
use wire::{
    BlobRef, DiffBase, DiffRequest, ErrorCode, GetBlobRequest, GetBlobResponse, PutBlobRequest,
    diff_base,
};

use crate::install::{AGENTS, REPLICAS};
use crate::runtime::ProfileRuntime;

/// The mime type of a patch written by Diff.
pub const PATCH_MIME: &str = "text/x-diff";

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("a blob is written only into an agent this host runs")]
    NotOwn,
    #[error("a blob hash is 32 bytes, not {0}")]
    BadHash(usize),
    #[error("the agent holds no blob {0}")]
    NoBlob(String),
    #[error("a diff needs a base")]
    NoBase,
    /// Writing failed, most often because the disk is full. Nothing is
    /// left behind.
    #[error("writing the blob: {0}")]
    Write(io::Error),
    #[error("reading the blob: {0}")]
    Read(io::Error),
    #[error("{0}")]
    Git(String),
    #[error("the bytes a peer sent are not the blob {0}")]
    Mismatch(String),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl BlobError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoBlob(_) => ErrorCode::NotFound,
            Self::NotOwn | Self::Git(_) => ErrorCode::FailedPrecondition,
            Self::BadHash(_) | Self::NoBase => ErrorCode::InvalidArgument,
            Self::Write(error) if is_full(error) => ErrorCode::ResourceExhausted,
            Self::Write(_) | Self::Read(_) | Self::Mismatch(_) | Self::Store(_) => {
                ErrorCode::Internal
            }
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

fn is_full(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    )
}

impl ProfileRuntime {
    /// Writes a person's attachment into the named own agent's directory
    /// and returns its reference. The same bytes twice are one file.
    pub async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, BlobError> {
        let row = self.blob_owner(&request.agent_id).await?;
        if row.agent.host != self.host().as_bytes() {
            return Err(BlobError::NotOwn);
        }
        let id = Uuid::from_slice(&row.agent.agent).map_err(|_| BlobError::NoAgent)?;
        let hash = Sha256::digest(&request.bytes).to_vec();
        let dir = self.agent_dir(id).join(agent_dir::BLOBS);
        let bytes = request.bytes;
        let size = bytes.len() as u64;
        let written = hash.clone();
        tokio::task::spawn_blocking(move || write_blob(&dir, &written, &bytes))
            .await
            .map_err(|error| BlobError::Write(io::Error::other(error)))?
            .map_err(BlobError::Write)?;
        Ok(BlobRef {
            size,
            hash,
            name: request.name,
            mime: request.mime,
        })
    }

    /// Reads a blob of an own agent or of a replica this runtime holds. A
    /// replica file not fetched yet is not found here.
    pub async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, BlobError> {
        if request.hash.len() != 32 {
            return Err(BlobError::BadHash(request.hash.len()));
        }
        let row = self.blob_owner(&request.agent_id).await?;
        let path = self.blob_path(&row, &request.hash);
        if row.agent.host != self.host().as_bytes()
            && let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut()
        {
            blobs.touch(&path, self.clock_now());
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(BlobError::NoBlob(hex(&request.hash)));
            }
            Err(error) => return Err(BlobError::Read(error)),
        };
        Ok(GetBlobResponse {
            blob: Some(BlobRef {
                hash: request.hash,
                size: bytes.len() as u64,
                ..BlobRef::default()
            }),
            bytes,
        })
    }

    /// Keeps bytes of a peer's agent, read from its origin, under that
    /// replica's directory, once they prove to be what `hash` names.
    pub async fn keep_replica_blob(
        &self,
        agent_id: &[u8],
        hash: &[u8],
        bytes: &[u8],
    ) -> Result<(), BlobError> {
        if Sha256::digest(bytes)[..] != *hash {
            return Err(BlobError::Mismatch(hex(hash)));
        }
        let row = self.blob_owner(agent_id).await?;
        if row.agent.host == self.host().as_bytes() {
            return Ok(());
        }
        let path = self.blob_path(&row, hash);
        let dir = path
            .parent()
            .expect("a blob path has a directory")
            .to_owned();
        let (written, owned) = (hash.to_vec(), bytes.to_vec());
        tokio::task::spawn_blocking(move || write_blob(&dir, &written, &owned))
            .await
            .map_err(|error| BlobError::Write(io::Error::other(error)))?
            .map_err(BlobError::Write)?;
        if let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut() {
            blobs.insert(path, bytes.len() as u64, self.clock_now());
        }
        Ok(())
    }

    /// Diffs the agent's working directory against `base` and writes the
    /// patch as the agent's blob. The working tree includes untracked
    /// files; a branch base diffs its merge base with HEAD. Index lines
    /// carry full object ids, which is each file's identity.
    pub async fn diff(&self, request: DiffRequest) -> Result<wire::Diff, BlobError> {
        let row = self.blob_owner(&request.agent_id).await?;
        if row.agent.host != self.host().as_bytes() {
            return Err(BlobError::NotOwn);
        }
        let base = request.base.ok_or(BlobError::NoBase)?;
        let cwd = PathBuf::from(&row.cwd);
        let head = one_line(git(&cwd, None, &["rev-parse", "--verify", "HEAD^{commit}"]).await?);
        let (patch, merge_base, name) = match &base.base {
            Some(diff_base::Base::Branch(branch)) => {
                let merge_base = one_line(git(&cwd, None, &["merge-base", branch, "HEAD"]).await?);
                let patch = git(&cwd, None, &diff_args(&[&merge_base, "HEAD"])).await?;
                (patch, Some(merge_base), format!("{branch}.diff"))
            }
            Some(diff_base::Base::WorkingTree(_)) => (
                working_tree(&cwd).await?,
                None,
                "working-tree.diff".to_owned(),
            ),
            None => return Err(BlobError::NoBase),
        };
        let patch = self
            .put_blob(PutBlobRequest {
                agent_id: request.agent_id,
                name,
                mime: PATCH_MIME.to_owned(),
                bytes: patch,
            })
            .await?;
        Ok(wire::Diff {
            patch: Some(patch),
            base: Some(DiffBase { base: base.base }),
            head,
            merge_base,
        })
    }

    /// The row of the agent a blob call names, own or replica.
    async fn blob_owner(&self, agent_id: &[u8]) -> Result<AgentRow, BlobError> {
        let store = self.store.lock().await;
        if let Ok(id) = Uuid::from_slice(agent_id)
            && let Some(row) = store.agent(&self.key(id))?
        {
            return Ok(row);
        }
        store
            .agents()?
            .into_iter()
            .find(|row| row.agent.agent == agent_id)
            .ok_or(BlobError::NoAgent)
    }

    fn blob_path(&self, row: &AgentRow, hash: &[u8]) -> PathBuf {
        let agent = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
        let dir = if row.agent.host == self.host().as_bytes() {
            self.agent_dir(agent)
        } else {
            let host = Uuid::from_slice(&row.agent.host).unwrap_or_default();
            self.dir()
                .join(REPLICAS)
                .join(host.to_string())
                .join(AGENTS)
                .join(agent.to_string())
        };
        dir.join(agent_dir::BLOBS).join(hex(hash))
    }
}

/// Writes `<dir>/<hex>` by temp-and-rename, so a reader sees the whole file
/// or none; the name is the content's hash, so two writers are safe. A
/// failed write removes its temporary file.
fn write_blob(dir: &Path, hash: &[u8], bytes: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let name = hex(hash);
    let path = dir.join(&name);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return Ok(());
    }
    let temp = dir.join(format!(".{name}.{}", Uuid::new_v4().simple()));
    let written = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn diff_args<'a>(revisions: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
        "--full-index",
    ];
    args.extend_from_slice(revisions);
    args.push("--");
    args
}

/// HEAD against the working tree, untracked files included: a temporary
/// index holds HEAD plus intent-to-add entries for the untracked files, so
/// the person's own index is never touched.
async fn working_tree(cwd: &Path) -> Result<Vec<u8>, BlobError> {
    let scratch = tempfile_path();
    let result = async {
        git(cwd, Some(&scratch), &["read-tree", "HEAD"]).await?;
        let untracked = git(
            cwd,
            None,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )
        .await?;
        let untracked: Vec<String> = untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect();
        if !untracked.is_empty() {
            let mut args = vec!["--literal-pathspecs", "add", "--intent-to-add", "--"];
            args.extend(untracked.iter().map(String::as_str));
            git(cwd, Some(&scratch), &args).await?;
        }
        git(cwd, Some(&scratch), &diff_args(&["HEAD"])).await
    }
    .await;
    let _ = std::fs::remove_file(&scratch);
    result
}

fn tempfile_path() -> PathBuf {
    std::env::temp_dir().join(format!("amux-diff-index-{}", Uuid::new_v4().simple()))
}

async fn git(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, BlobError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
        .output()
        .await
        .map_err(|error| BlobError::Git(format!("running git: {error}")))?;
    if !output.status.success() {
        return Err(BlobError::Git(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn one_line(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_owned()
}
