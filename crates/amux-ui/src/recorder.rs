//! Msg recorder: a ring buffer of the last N serialized Msgs plus a
//! checkpoint Model, dumpable as a self-contained JSONL replay bundle.
//!
//! When the ring evicts a Msg it is folded into the checkpoint with the same
//! pure `update`, so `checkpoint + retained msgs` always reproduces the live
//! Model exactly. Dumps can contain prompts, code, and paths: they are
//! written 0600 under the amux data dir, retained bounded, and never
//! uploaded — local-only, shared deliberately.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::effect::DumpReason;
use crate::model::Model;
use crate::msg::Msg;
use crate::update::update;

/// Default ring capacity (Msgs retained verbatim behind the checkpoint).
pub const DEFAULT_RECORDER_CAPACITY: usize = 10_000;
/// Bounded on-disk retention: newest dumps kept, older deleted.
pub const DUMP_RETAINED_FILES: usize = 20;
/// Bumped whenever the dump framing (not the Msg schema) changes.
pub const DUMP_FORMAT_VERSION: u32 = 1;

/// First line of a dump file; the remaining lines are one serialized Msg
/// each.
#[derive(Debug, Serialize, Deserialize)]
pub struct DumpHeader {
    pub format_version: u32,
    /// Reducer build that recorded the Msgs (replay needs the same build for
    /// the determinism guarantee to hold).
    pub build: String,
    pub reason: DumpReason,
    pub checkpoint: Model,
}

pub struct Recorder {
    capacity: usize,
    checkpoint: Model,
    entries: VecDeque<String>,
}

impl Recorder {
    pub fn new(capacity: usize, initial: &Model) -> Self {
        Self {
            capacity: capacity.max(1),
            checkpoint: initial.clone(),
            entries: VecDeque::new(),
        }
    }

    /// Record a Msg (already coalesced — the recorded Msg is the batch).
    pub fn record(&mut self, msg: &Msg) {
        let line = match serde_json::to_string(msg) {
            Ok(line) => line,
            Err(error) => {
                // A Msg that cannot serialize is a bug in the Msg schema;
                // recording must not take down the client.
                tracing::error!(%error, "failed to serialize Msg for the recorder");
                return;
            }
        };
        self.entries.push_back(line);
        while self.entries.len() > self.capacity {
            let evicted = self.entries.pop_front().expect("non-empty ring");
            match serde_json::from_str::<Msg>(&evicted) {
                Ok(msg) => {
                    // Replay never executes effects; neither does checkpoint
                    // advancement.
                    let _ = update(&mut self.checkpoint, msg);
                }
                Err(error) => {
                    tracing::error!(%error, "recorder evicted an unparseable Msg");
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mirror the shell's sticky invariant-warning fact into replay state.
    /// This transition is monotonic and independent of Msg ordering, so the
    /// checkpoint can carry it without crossing the pure reducer boundary.
    pub fn note_invariant_violation(&mut self) {
        self.checkpoint.note_invariant_violation();
    }

    /// Write a self-contained replay bundle into `dir` and prune old dumps.
    /// `stamp` must be unique per dump (the shell derives it from wall time
    /// and pid; the recorder itself stays clock-free).
    pub fn write_dump(
        &self,
        dir: &Path,
        reason: DumpReason,
        build: &str,
        stamp: &str,
    ) -> io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("ui-dump-{stamp}.jsonl"));
        let header = DumpHeader {
            format_version: DUMP_FORMAT_VERSION,
            build: build.to_string(),
            reason,
            checkpoint: self.checkpoint.clone(),
        };
        let mut file = create_private_file(&path)?;
        serde_json::to_writer(&mut file, &header).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        for line in &self.entries {
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
        prune_old_dumps(dir);
        Ok(path)
    }
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn prune_old_dumps(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ui-dump-") && name.ends_with(".jsonl"))
        })
        .collect();
    // Stamps are lexicographically ordered (zero-padded millis), so name
    // order is age order.
    dumps.sort();
    while dumps.len() > DUMP_RETAINED_FILES {
        let oldest = dumps.remove(0);
        if let Err(error) = std::fs::remove_file(&oldest) {
            tracing::warn!(path = %oldest.display(), %error, "failed to prune old ui dump");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("failed to read dump: {0}")]
    Io(#[from] io::Error),
    #[error("dump is empty")]
    Empty,
    #[error("failed to parse dump header: {0}")]
    Header(serde_json::Error),
    #[error("failed to parse Msg on line {line}: {error}")]
    Msg {
        line: usize,
        error: serde_json::Error,
    },
    #[error("unsupported dump format version {0}")]
    FormatVersion(u32),
}

/// Fold a dump back into the Model it recorded. Effects are never executed.
pub fn replay(path: &Path) -> Result<Model, ReplayError> {
    let contents = std::fs::read_to_string(path)?;
    let mut lines = contents.lines();
    let header_line = lines.next().ok_or(ReplayError::Empty)?;
    let header: DumpHeader = serde_json::from_str(header_line).map_err(ReplayError::Header)?;
    if header.format_version != DUMP_FORMAT_VERSION {
        return Err(ReplayError::FormatVersion(header.format_version));
    }
    let mut model = header.checkpoint;
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Msg = serde_json::from_str(line).map_err(|error| ReplayError::Msg {
            line: index + 2,
            error,
        })?;
        let _ = update(&mut model, msg);
    }
    Ok(model)
}
