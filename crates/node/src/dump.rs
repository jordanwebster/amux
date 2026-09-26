//! Dumps: one bundle with everything the host side knows about a profile's
//! agents, redacted before it is written.
//!
//! The daemon interprets nothing, with this one stated exception: only the
//! per-kind code in `interpret` can decode a body, so the dump path calls
//! its redactor on every record it writes. Each agent process redacts its
//! own part (the facts ring, its checkpoints and the specs) before it
//! leaves the process.
//!
//! A bundle is a directory under the installation's `reports/`, assembled
//! beside it under a dotted name and renamed into place once complete:
//!
//! ```text
//! reports/dump-<unix ms>-<id>/
//!   manifest.json            what was dumped, when, why, by which daemon, and
//!                            every file or part that could not be gathered
//!   daemon.log               the tail of the daemon's own log, redacted as text
//!   agents/<agent_id>/
//!     row.pb                 the inventory row, a wire.Agent
//!     store.pb               the store slice: one wire.Step holding the
//!                            snapshot and the newest rows, oldest first, with
//!                            their orders and revisions
//!     journal/<segment>      the last two journal segments, whole frames only,
//!                            each frame redacted (own agents)
//!     part/...               the agent process's part, as it sent it:
//!                            facts/<segment>, facts/<segment>.checkpoint,
//!                            spec.<n>, and dump-errors when it had any. An
//!                            agent with no process has only its specs here,
//!                            redacted by the daemon
//! ```
//!
//! Everything protobuf is encoded without a length prefix except journal
//! segments, which keep the journal's own framing. [`pack`] turns a bundle
//! into one `wire.DumpPart` whose files carry these relative paths, for a
//! client on another machine.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use interpret::RedactTarget;
use prost::Message as _;
use store::{AgentRow, Store as _};
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::{
    CtlFrame, DumpFile, DumpInput, DumpPart, DumpRequest, Input, Kind, Step, ctl_frame, input,
};

use crate::blobs::hex;
use crate::runtime::{ProfileRuntime, to_wire};

/// The rows of each agent a dump carries: the newest, which is where a
/// problem being reported lives.
pub const DUMP_ROWS: u32 = 1_000;
/// The journal segments a dump carries: the daemon keeps the last two
/// ingested ones for exactly this.
pub const DUMP_SEGMENTS: usize = 2;
/// How much of the daemon's log a dump carries, from its end.
pub const DUMP_LOG_BYTES: u64 = 4 << 20;
/// How long an agent process has to send its part.
const PART_PATIENCE: Duration = Duration::from_secs(10);

pub const MANIFEST: &str = "manifest.json";
pub const DAEMON_LOG: &str = "daemon.log";

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("no agent {0}")]
    NoAgent(String),
    #[error("writing the bundle: {0}")]
    Write(#[from] io::Error),
    #[error("the store: {0}")]
    Store(#[from] store::StoreError),
}

impl DumpError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent(_) => wire::ErrorCode::NotFound,
            Self::Write(_) | Self::Store(_) => wire::ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

impl ProfileRuntime {
    /// Writes one bundle for the request's agents (every agent of the
    /// profile when it names none) and returns its directory. What cannot
    /// be gathered is named in the manifest, never included unredacted.
    pub async fn dump(&self, request: DumpRequest) -> Result<PathBuf, DumpError> {
        let dump_id = Uuid::new_v4();
        let created_at_ms = self.clock_now();
        let name = format!(
            "dump-{created_at_ms}-{}",
            &dump_id.simple().to_string()[..8]
        );
        let rows = self.dump_rows(&request.agent_ids).await?;

        std::fs::create_dir_all(&self.reports)?;
        let partial = self.reports.join(format!(".{name}"));
        let _ = std::fs::remove_dir_all(&partial);
        std::fs::create_dir_all(&partial)?;
        let mut errors = Vec::new();
        let mut agents = Vec::new();
        for row in &rows {
            let (entry, mut failed) = self.dump_agent(row, dump_id.as_bytes(), &partial).await?;
            agents.push(entry);
            errors.append(&mut failed);
        }
        match self.dump_log(&partial) {
            Ok(Some(bytes)) => log_cut_note(&mut errors, bytes),
            Ok(None) => {}
            Err(error) => errors.push(format!("{DAEMON_LOG}: {error}")),
        }
        let manifest = serde_json::json!({
            "dump_id": dump_id.to_string(),
            "created_at_ms": created_at_ms,
            "reason": interpret::redact_text(&request.reason),
            "automatic": request.automatic,
            "profile": self.profile().to_string(),
            "host": self.host().to_string(),
            "generation": self.generation(),
            "daemon_version": crate::VERSION,
            "agents": agents,
            "errors": errors,
        });
        std::fs::write(
            partial.join(MANIFEST),
            serde_json::to_vec_pretty(&manifest).expect("JSON writes"),
        )?;
        let bundle = self.reports.join(&name);
        std::fs::rename(&partial, &bundle)?;
        Ok(bundle)
    }

    /// The rows a dump covers, own and replica.
    async fn dump_rows(&self, ids: &[Vec<u8>]) -> Result<Vec<AgentRow>, DumpError> {
        let rows = self.store.lock().await.agents()?;
        if ids.is_empty() {
            return Ok(rows);
        }
        ids.iter()
            .map(|id| {
                rows.iter()
                    .find(|row| &row.agent.agent == id)
                    .cloned()
                    .ok_or_else(|| {
                        DumpError::NoAgent(
                            Uuid::from_slice(id).map_or_else(|_| hex(id), |id| id.to_string()),
                        )
                    })
            })
            .collect()
    }

    /// One agent's directory in the bundle. Returns its manifest entry and
    /// what could not be gathered.
    async fn dump_agent(
        &self,
        row: &AgentRow,
        dump_id: &[u8],
        bundle: &Path,
    ) -> Result<(serde_json::Value, Vec<String>), DumpError> {
        let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
        let kind = wire::kind_from_tag(&row.kind).unwrap_or(Kind::Unspecified);
        let own = row.agent.host == self.host().as_bytes();
        let out = bundle.join("agents").join(id.to_string());
        std::fs::create_dir_all(&out)?;
        let mut errors = Vec::new();
        let mut note = |what: &str, error: &dyn std::fmt::Display| {
            errors.push(format!("{id} {what}: {error}"));
        };

        std::fs::write(
            out.join("row.pb"),
            redacted(kind, RedactTarget::Agent(to_wire(row).encode_to_vec())),
        )?;
        let cut = self.store.lock().await.cut(&row.agent, DUMP_ROWS)?;
        let slice = Step {
            items: cut.held,
            snapshot: cut.snapshot,
            ..Step::default()
        };
        std::fs::write(
            out.join("store.pb"),
            redacted(kind, RedactTarget::Step(slice.encode_to_vec())),
        )?;

        let mut part_files = Vec::new();
        if own {
            let dir = self.agent_dir(id);
            match journal_tail(&dir.join(agent_dir::JOURNAL), kind) {
                Ok(segments) => {
                    for (name, bytes) in segments {
                        write_under(&out.join("journal"), &name, &bytes)?;
                    }
                }
                Err(error) => note("journal", &error),
            }
            match self.agent_part(id, dump_id).await {
                Some(part) => {
                    for file in part.files {
                        match safe_relative(&file.name) {
                            Some(name) => {
                                write_under(&out.join("part"), &name, &file.contents)?;
                                part_files.push(file.name);
                            }
                            None => note("part", &format!("a file named {:?}", file.name)),
                        }
                    }
                }
                None => {
                    note(
                        "part",
                        &"no process answered, so its facts ring was not gathered",
                    );
                    match specs(&dir) {
                        Ok(specs) => {
                            for (name, bytes) in specs {
                                let bytes = redacted(kind, RedactTarget::Spec(bytes));
                                write_under(&out.join("part"), &name, &bytes)?;
                                part_files.push(name);
                            }
                        }
                        Err(error) => note("specs", &error),
                    }
                }
            }
        }
        let entry = serde_json::json!({
            "agent_id": id.to_string(),
            "host_id": Uuid::from_slice(&row.agent.host).unwrap_or_default().to_string(),
            "kind": row.kind,
            "own": own,
            "lifecycle": row.lifecycle,
            "incarnation": row.incarnation,
            "rows": slice.items.len(),
            "part": part_files,
        });
        Ok((entry, errors))
    }

    /// Asks the agent's process for its part over its control connection.
    /// None when it has no connection or does not answer in time.
    async fn agent_part(&self, id: Uuid, dump_id: &[u8]) -> Option<DumpPart> {
        let handle = self.handle(id)?;
        let (tx, rx) = oneshot::channel();
        handle.expect_dump(dump_id.to_vec(), tx);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Input(Input {
                input_id: dump_id.to_vec(),
                of: Some(input::Of::Dump(DumpInput {
                    dump_id: dump_id.to_vec(),
                })),
            })),
        };
        let patience = crate::runtime::ms(self.launch().ctl_write_ms);
        let sent = matches!(
            handle.write_ctl(&frame, patience).await,
            crate::runtime::Sent::Written
        );
        let part = if sent {
            tokio::time::timeout(PART_PATIENCE, rx).await.ok()?.ok()
        } else {
            None
        };
        handle.forget_dump(dump_id);
        part
    }

    /// The tail of the daemon's log, redacted. Returns how many bytes of
    /// the log were left out, when the log was cut.
    fn dump_log(&self, bundle: &Path) -> io::Result<Option<u64>> {
        let Some(path) = &self.daemon_log else {
            return Ok(None);
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let skipped = (bytes.len() as u64).saturating_sub(DUMP_LOG_BYTES);
        let mut tail = &bytes[skipped as usize..];
        // Start at a line, not in the middle of one.
        if skipped > 0
            && let Some(newline) = tail.iter().position(|byte| *byte == b'\n')
        {
            tail = &tail[newline + 1..];
        }
        let text = interpret::redact_text(&String::from_utf8_lossy(tail));
        std::fs::write(bundle.join(DAEMON_LOG), text)?;
        Ok((skipped > 0).then_some(skipped))
    }
}

fn log_cut_note(errors: &mut Vec<String>, skipped: u64) {
    errors.push(format!(
        "{DAEMON_LOG}: the first {skipped} bytes of the log were left out"
    ));
}

/// One target through the kind's redactor, as the bytes it came back as.
fn redacted(kind: Kind, target: RedactTarget) -> Vec<u8> {
    match interpret::redact(kind, target) {
        RedactTarget::ItemBody(bytes)
        | RedactTarget::SnapshotBody(bytes)
        | RedactTarget::Input(bytes)
        | RedactTarget::Checkpoint(bytes)
        | RedactTarget::Spec(bytes)
        | RedactTarget::Step(bytes)
        | RedactTarget::Agent(bytes) => bytes,
        RedactTarget::Fact(fact) => fact.payload,
    }
}

/// The last journal segments, each re-framed with every whole frame
/// redacted. A torn frame at the end, a write still under way, is left out.
fn journal_tail(dir: &Path, kind: Kind) -> io::Result<Vec<(String, Vec<u8>)>> {
    let starts = journal::segments(dir)?;
    let mut out = Vec::new();
    for &start in starts.iter().rev().take(DUMP_SEGMENTS).rev() {
        let bytes = std::fs::read(journal::segment_path(dir, start))?;
        let mut framed = Vec::new();
        let mut at = 0;
        while let journal::Frame::Whole(step, len) = journal::decode_frame(&bytes[at..]) {
            let step = redacted(kind, RedactTarget::Step(step.encode_to_vec()));
            let step = Step::decode(step.as_slice()).unwrap_or_default();
            if step.encoded_len() > 0 {
                framed.extend(journal::encode_frame(&step));
            }
            at += len;
        }
        out.push((journal::segment_name(start), framed));
    }
    Ok(out)
}

/// Every `spec.<n>` in an agent directory, by number.
fn specs(dir: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut specs = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(n) = name
            .strip_prefix("spec.")
            .and_then(|n| n.parse::<u32>().ok())
        {
            specs.insert(n, (name.clone(), std::fs::read(dir.join(&name))?));
        }
    }
    Ok(specs.into_values().collect())
}

/// A name an agent sent, if it stays inside the directory it is written
/// under.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let normal = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (normal && !name.is_empty()).then(|| path.to_owned())
}

fn write_under(dir: &Path, name: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// A bundle as one message: every file under `bundle`, named by its path
/// relative to it with `/` separators, in path order.
pub fn pack(bundle: &Path) -> io::Result<DumpPart> {
    let mut files = Vec::new();
    let mut stack = vec![bundle.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .strip_prefix(bundle)
                .map_err(io::Error::other)?
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push(DumpFile {
                name,
                contents: std::fs::read(&path)?,
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let dump_id = bundle
        .file_name()
        .map(|name| name.to_string_lossy().into_owned().into_bytes())
        .unwrap_or_default();
    Ok(DumpPart { dump_id, files })
}
