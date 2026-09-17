//! Msg recorder: a bounded ring buffer of recent serialized Msgs,
//! snapshotable as a self-contained JSONL report part.
//!
//! Before the ring evicts anything, its initial checkpoint plus retained Msgs
//! remains a foldable recording. After eviction, a normal runtime report
//! captures the live Model once and keeps the ring as recent diagnostic
//! context. This avoids retaining a second, continuously growing Model merely
//! to make a report possible. Snapshots can contain prompts, code, and paths:
//! they are written 0600 under the amux data dir, retained bounded, and never
//! uploaded — local-only, shared deliberately.

use std::collections::VecDeque;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ui_state::{Model, Msg, update};

/// Default ring capacity (Msgs retained verbatim as recent context).
pub const DEFAULT_RECORDER_CAPACITY: usize = 10_000;
/// Maximum serialized bytes retained behind the checkpoint.
///
/// A count limit alone is not a memory limit: one store page or stream batch
/// can carry hundreds of entries. Two MiB preserves a useful recent event
/// window without retaining another copy of the live Model.
pub const DEFAULT_RECORDER_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Bumped whenever the recorder snapshot framing changes.
pub const MSGS_SCHEMA_VERSION: u32 = 2;

pub struct Recorder {
    capacity: usize,
    max_bytes: usize,
    retained_bytes: usize,
    checkpoint: Model,
    entries: VecDeque<String>,
    evicted: bool,
}

/// A frozen recorder window ready to be embedded in a report bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderSnapshot {
    pub checkpoint: Model,
    pub msgs: Vec<String>,
    pub mode: RecorderSnapshotMode,
}

/// How a recorder snapshot reaches the Model it represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecorderSnapshotMode {
    /// Fold every following Msg into the checkpoint.
    Fold,
    /// The checkpoint is already the captured final Model; Msgs are context.
    Captured,
    /// The ring evicted history and no live Model was available to capture.
    RecentOnly,
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
    pub mode: RecorderSnapshotMode,
}

impl Recorder {
    pub fn new(capacity: usize, initial: &Model) -> Self {
        Self::with_limits(capacity, DEFAULT_RECORDER_MAX_BYTES, initial)
    }

    /// Build a recorder with explicit count and serialized-byte ceilings.
    pub fn with_limits(capacity: usize, max_bytes: usize, initial: &Model) -> Self {
        Self {
            capacity: capacity.max(1),
            max_bytes: max_bytes.max(1),
            retained_bytes: 0,
            checkpoint: initial.clone(),
            entries: VecDeque::new(),
            evicted: false,
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
        self.retained_bytes = self.retained_bytes.saturating_add(line.capacity());
        self.entries.push_back(line);
        while self.entries.len() > self.capacity || self.retained_bytes > self.max_bytes {
            let evicted = self.entries.pop_front().expect("non-empty ring");
            self.retained_bytes = self.retained_bytes.saturating_sub(evicted.capacity());
            self.evicted = true;
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
            mode: if self.evicted {
                RecorderSnapshotMode::RecentOnly
            } else {
                RecorderSnapshotMode::Fold
            },
        }
    }

    /// Freeze an exact report snapshot using the runtime's authoritative
    /// Model when the bounded ring no longer contains the full session.
    pub fn snapshot_with_model(&self, live: &Model) -> RecorderSnapshot {
        if !self.evicted {
            return self.snapshot();
        }
        RecorderSnapshot {
            checkpoint: live.clone(),
            msgs: self.entries.iter().cloned().collect(),
            mode: RecorderSnapshotMode::Captured,
        }
    }

    pub(crate) fn retention(&self) -> RecorderRetention {
        RecorderRetention {
            entries: self.entries.len(),
            entry_bytes: self.retained_bytes,
            checkpoint_bytes: serde_json::to_vec(&self.checkpoint).map_or(0, |bytes| bytes.len()),
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
    #[error("recorder snapshot contains recent context but no complete Model")]
    Incomplete,
}

#[derive(Deserialize)]
struct StoredRecorderSnapshotHeader {
    format_version: u32,
    checkpoint: Model,
    mode: RecorderSnapshotMode,
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
    if header.mode == RecorderSnapshotMode::RecentOnly {
        return Err(ReplayError::Incomplete);
    }
    let mut model = header.checkpoint;
    if header.mode == RecorderSnapshotMode::Captured {
        return Ok(model);
    }
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn byte_limit_captures_live_model_without_retaining_a_second_model() {
        let mut live = Model::default();
        let mut recorder = Recorder::with_limits(10_000, 256, &live);
        for second in 0..100 {
            let msg = Msg::Tick {
                now: Utc.timestamp_opt(1_700_000_000 + second, 0).unwrap(),
            };
            recorder.record(&msg);
            let _ = update(&mut live, msg);
        }

        assert!(recorder.retained_bytes <= 256);
        assert!(recorder.len() < 100, "the byte ceiling should evict ticks");
        assert_eq!(recorder.snapshot().mode, RecorderSnapshotMode::RecentOnly);

        let snapshot = recorder.snapshot_with_model(&live);
        assert_eq!(snapshot.mode, RecorderSnapshotMode::Captured);
        assert_eq!(snapshot.checkpoint, live);
        assert!(
            !snapshot.msgs.is_empty(),
            "recent inputs remain inspectable"
        );
        assert!(
            recorder.retention().checkpoint_bytes < serde_json::to_vec(&live).unwrap().len(),
            "the live model is copied only when a report is requested"
        );
    }
}
