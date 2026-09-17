//! Msg recorder: a bounded ring of recent serialized Msgs, snapshotable with
//! an on-disk Model checkpoint as a self-contained JSONL report part.
//!
//! The recorder owns no Model. The runtime advances a private rolling
//! checkpoint whenever the next serialized Msg would cross either ring bound,
//! then clears the ring before admitting that Msg. Snapshots can contain
//! prompts, code, and paths: they are written 0600 under the amux data dir,
//! retained bounded, and never uploaded — local-only, shared deliberately.

use std::collections::VecDeque;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ui_state::{Model, Msg, update};

/// Default ring capacity (Msgs retained verbatim as recent context).
pub const DEFAULT_RECORDER_CAPACITY: usize = 10_000;
/// Maximum serialized bytes retained after the on-disk checkpoint.
///
/// A count limit alone is not a memory limit: one store page or stream batch
/// can carry hundreds of entries. Two MiB preserves a useful recent event
/// window without retaining another copy of the live Model.
pub const DEFAULT_RECORDER_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Bumped whenever the recorder snapshot framing changes.
pub const MSGS_SCHEMA_VERSION: u32 = 3;

#[derive(Clone)]
pub struct Recorder {
    capacity: usize,
    max_bytes: usize,
    retained_bytes: usize,
    entries: VecDeque<String>,
    invariant_violation: bool,
}

/// One serialized Msg prepared before the runtime decides whether the
/// checkpoint must advance.
pub(crate) struct PreparedMsg {
    line: String,
}

/// A frozen recorder window ready to be embedded in a report bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderSnapshot {
    pub checkpoint: Model,
    pub msgs: Vec<String>,
    pub invariant_violation: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecorderRetention {
    pub entries: usize,
    pub entry_bytes: usize,
    pub checkpoint_bytes: usize,
}

#[derive(Serialize)]
pub(crate) struct RecorderSnapshotHeader<'a> {
    pub format_version: u32,
    pub checkpoint: &'a Model,
    pub invariant_violation: bool,
}

impl Recorder {
    pub fn new(capacity: usize) -> Self {
        Self::with_limits(capacity, DEFAULT_RECORDER_MAX_BYTES)
    }

    /// Build a recorder with explicit count and serialized-byte ceilings.
    pub fn with_limits(capacity: usize, max_bytes: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            max_bytes: max_bytes.max(1),
            retained_bytes: 0,
            entries: VecDeque::new(),
            invariant_violation: false,
        }
    }

    /// Serialize a Msg before changing either the checkpoint or the ring.
    pub(crate) fn prepare(&self, msg: &Msg) -> Option<PreparedMsg> {
        match serde_json::to_string(msg) {
            Ok(line) => Some(PreparedMsg { line }),
            Err(error) => {
                // A Msg that cannot serialize is a bug in the Msg schema;
                // recording must not take down the client.
                tracing::error!(%error, "failed to serialize Msg for the recorder");
                None
            }
        }
    }

    /// Whether admitting this Msg requires the runtime to checkpoint the
    /// current Model and start a new ring first.
    pub(crate) fn requires_checkpoint(&self, msg: &PreparedMsg) -> bool {
        self.entries.len() >= self.capacity
            || self.retained_bytes.saturating_add(msg.line.capacity()) > self.max_bytes
    }

    pub(crate) fn push(&mut self, msg: PreparedMsg) {
        self.retained_bytes = self.retained_bytes.saturating_add(msg.line.capacity());
        self.entries.push_back(msg.line);
    }

    pub(crate) fn exceeds_limits(&self) -> bool {
        self.entries.len() > self.capacity || self.retained_bytes > self.max_bytes
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.retained_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Preserve the shell's sticky invariant-warning fact in report framing.
    pub fn note_invariant_violation(&mut self) {
        self.invariant_violation = true;
    }

    /// Pair a checkpoint read by the runtime with the Msgs that followed it.
    pub(crate) fn snapshot(&self, checkpoint: Model) -> RecorderSnapshot {
        RecorderSnapshot {
            checkpoint,
            msgs: self.entries.iter().cloned().collect(),
            invariant_violation: self.invariant_violation,
        }
    }

    pub(crate) fn retention(&self) -> RecorderRetention {
        RecorderRetention {
            entries: self.entries.len(),
            entry_bytes: self.retained_bytes,
            checkpoint_bytes: 0,
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
    invariant_violation: bool,
}

/// Fold a report's `msgs.jsonl` into the Model it recorded. Effects are never
/// executed. The invariant-warning fact is report framing rather than an
/// input, so it is applied after every recorded Msg has folded.
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
    if header.invariant_violation {
        model.note_invariant_violation();
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn byte_limit_requests_a_checkpoint_without_retaining_a_model() {
        let mut recorder = Recorder::with_limits(10_000, 256);
        let mut resets = 0;
        for second in 0..100 {
            let msg = Msg::Tick {
                now: Utc.timestamp_opt(1_700_000_000 + second, 0).unwrap(),
            };
            let prepared = recorder.prepare(&msg).unwrap();
            if recorder.requires_checkpoint(&prepared) {
                recorder.clear();
                resets += 1;
            }
            recorder.push(prepared);
        }

        assert!(resets > 1);
        assert!(recorder.retained_bytes <= 256);
        assert!(recorder.len() < 100);
        assert_eq!(recorder.retention().checkpoint_bytes, 0);
    }
}
