//! Msg recorder: a ring buffer of the last N serialized Msgs plus a
//! checkpoint Model, snapshotable as a self-contained JSONL replay bundle.
//!
//! When the ring evicts a Msg it is folded into the checkpoint with the same
//! pure `update`, so `checkpoint + retained msgs` always reproduces the live
//! Model exactly. Snapshots can contain prompts, code, and paths: they are
//! written 0600 under the amux data dir, retained bounded, and never
//! uploaded — local-only, shared deliberately.

use std::collections::VecDeque;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::Model;
use crate::msg::Msg;
use crate::update::update;

/// Default ring capacity (Msgs retained verbatim behind the checkpoint).
pub const DEFAULT_RECORDER_CAPACITY: usize = 10_000;
/// Bumped whenever the recorder snapshot framing changes.
pub const MSGS_SCHEMA_VERSION: u32 = 1;

pub struct Recorder {
    capacity: usize,
    checkpoint: Model,
    entries: VecDeque<String>,
}

/// A frozen recorder window ready to be embedded in a report bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderSnapshot {
    pub checkpoint: Model,
    pub msgs: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct RecorderSnapshotHeader<'a> {
    pub format_version: u32,
    pub checkpoint: &'a Model,
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

    /// Freeze the checkpoint and retained Msg lines as one replayable window.
    pub fn snapshot(&self) -> RecorderSnapshot {
        RecorderSnapshot {
            checkpoint: self.checkpoint.clone(),
            msgs: self.entries.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("failed to read recorder snapshot: {0}")]
    Io(#[from] io::Error),
    #[error("recorder snapshot is empty")]
    Empty,
    #[error("failed to parse recorder snapshot header: {0}")]
    Header(serde_json::Error),
    #[error("failed to parse Msg on line {line}: {error}")]
    Msg {
        line: usize,
        error: serde_json::Error,
    },
    #[error("unsupported recorder snapshot format version {0}")]
    FormatVersion(u32),
}

#[derive(Deserialize)]
struct StoredRecorderSnapshotHeader {
    format_version: u32,
    checkpoint: Model,
}

/// Fold a report's `msgs.jsonl` into the Model it recorded. Effects are never
/// executed.
pub fn replay_msgs(path: &Path) -> Result<Model, ReplayError> {
    let contents = std::fs::read_to_string(path)?;
    let mut lines = contents.lines();
    let header_line = lines.next().ok_or(ReplayError::Empty)?;
    let header: StoredRecorderSnapshotHeader =
        serde_json::from_str(header_line).map_err(ReplayError::Header)?;
    if header.format_version != MSGS_SCHEMA_VERSION {
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
