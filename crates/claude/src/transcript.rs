//! Claude transcript JSONL row classification and tailing.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::{mpsc, watch};

macro_rules! row_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name(pub Value);
    };
}

row_type!(UserRow);
row_type!(AssistantRow);
row_type!(SystemRow);
row_type!(AttachmentRow);
row_type!(SessionStateRow);
row_type!(FileHistoryRow);

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptRow {
    User(UserRow),
    Assistant(AssistantRow),
    System(SystemRow),
    Attachment(AttachmentRow),
    SessionState(SessionStateRow),
    FileHistory(FileHistoryRow),
    Unknown(Value),
}

impl TranscriptRow {
    pub fn parse(value: Value) -> Self {
        match value.get("type").and_then(Value::as_str) {
            Some("user") => Self::User(UserRow(value)),
            Some("assistant") => Self::Assistant(AssistantRow(value)),
            Some("system") => Self::System(SystemRow(value)),
            Some("attachment") => Self::Attachment(AttachmentRow(value)),
            Some("session_state") | Some("session-state") => {
                Self::SessionState(SessionStateRow(value))
            }
            Some("file-history-snapshot") | Some("file_history") => {
                Self::FileHistory(FileHistoryRow(value))
            }
            _ => Self::Unknown(value),
        }
    }

    pub fn as_value(&self) -> &Value {
        match self {
            Self::User(UserRow(value))
            | Self::Assistant(AssistantRow(value))
            | Self::System(SystemRow(value))
            | Self::Attachment(AttachmentRow(value))
            | Self::SessionState(SessionStateRow(value))
            | Self::FileHistory(FileHistoryRow(value))
            | Self::Unknown(value) => value,
        }
    }

    pub fn into_value(self) -> Value {
        match self {
            Self::User(UserRow(value))
            | Self::Assistant(AssistantRow(value))
            | Self::System(SystemRow(value))
            | Self::Attachment(AttachmentRow(value))
            | Self::SessionState(SessionStateRow(value))
            | Self::FileHistory(FileHistoryRow(value))
            | Self::Unknown(value) => value,
        }
    }
}

/// A tailer owns one row stream and can be relinked to a replacement transcript.
pub struct TranscriptTailer {
    path_tx: watch::Sender<PathBuf>,
    rows: Mutex<Option<mpsc::Receiver<TranscriptRow>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TranscriptTailer {
    pub fn follow(path: PathBuf) -> Self {
        let (path_tx, path_rx) = watch::channel(path);
        let (row_tx, row_rx) = mpsc::channel(256);
        let task = tokio::spawn(async move {
            tail_paths(path_rx, row_tx).await;
        });
        Self {
            path_tx,
            rows: Mutex::new(Some(row_rx)),
            task,
        }
    }

    pub fn relink(&mut self, path: PathBuf) {
        self.path_tx.send_replace(path);
    }

    pub fn rows(&self) -> mpsc::Receiver<TranscriptRow> {
        self.rows
            .lock()
            .expect("transcript rows mutex poisoned")
            .take()
            .expect("transcript row stream already taken")
    }
}

impl Drop for TranscriptTailer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn tail_paths(mut paths: watch::Receiver<PathBuf>, rows: mpsc::Sender<TranscriptRow>) {
    loop {
        let path = paths.borrow_and_update().clone();
        match tail_one(&path, &mut paths, &rows).await {
            TailOutcome::Relink => continue,
            TailOutcome::Closed => break,
        }
    }
}

enum TailOutcome {
    Relink,
    Closed,
}

async fn tail_one(
    path: &PathBuf,
    paths: &mut watch::Receiver<PathBuf>,
    rows: &mpsc::Sender<TranscriptRow>,
) -> TailOutcome {
    let file = loop {
        match tokio::fs::File::open(path).await {
            Ok(file) => break file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::select! {
                    changed = paths.changed() => return if changed.is_ok() { TailOutcome::Relink } else { TailOutcome::Closed },
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
            Err(_) => return TailOutcome::Closed,
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut eof_observed = false;
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                if !eof_observed {
                    eof_observed = true;
                    tokio::select! {
                        changed = paths.changed() => return if changed.is_ok() { TailOutcome::Relink } else { TailOutcome::Closed },
                        _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                    }
                }
                let ready =
                    TranscriptRow::Unknown(serde_json::json!({"type":"amux.transcript_ready"}));
                if rows.send(ready).await.is_err() {
                    return TailOutcome::Closed;
                }
                break;
            }
            Ok(_) => {
                eof_observed = false;
                if send_line(&line, rows).await.is_err() {
                    return TailOutcome::Closed;
                }
            }
            Err(_) => return TailOutcome::Closed,
        }
    }
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                tokio::select! {
                    changed = paths.changed() => return if changed.is_ok() { TailOutcome::Relink } else { TailOutcome::Closed },
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        let Ok(position) = reader.stream_position().await else { return TailOutcome::Closed; };
                        let Ok(metadata) = tokio::fs::metadata(path).await else { continue; };
                        if metadata.len() < position && reader.seek(std::io::SeekFrom::Start(0)).await.is_err() {
                            return TailOutcome::Closed;
                        }
                    }
                }
            }
            Ok(_) => {
                if send_line(&line, rows).await.is_err() {
                    return TailOutcome::Closed;
                }
            }
            Err(_) => return TailOutcome::Closed,
        }
    }
}

async fn send_line(
    line: &str,
    rows: &mpsc::Sender<TranscriptRow>,
) -> Result<(), mpsc::error::SendError<TranscriptRow>> {
    let trimmed = line.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        rows.send(TranscriptRow::parse(value)).await?;
    }
    Ok(())
}

/// An ingest owns relinking and exposes the crate row stream.
pub struct TranscriptIngest {
    tailer: TranscriptTailer,
}

impl TranscriptIngest {
    pub fn follow(path: PathBuf) -> Self {
        Self {
            tailer: TranscriptTailer::follow(path),
        }
    }

    pub fn relink(&mut self, path: PathBuf) {
        self.tailer.relink(path);
    }

    pub fn rows(&self) -> mpsc::Receiver<TranscriptRow> {
        self.tailer.rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_rows_and_preserves_unknown_rows() {
        assert!(matches!(
            TranscriptRow::parse(serde_json::json!({"type":"assistant","extra":1})),
            TranscriptRow::Assistant(_)
        ));
        let unknown = TranscriptRow::parse(serde_json::json!({"type":"progress","extra":1}));
        assert_eq!(unknown.as_value()["extra"], 1);
    }

    #[tokio::test]
    async fn tailer_reads_existing_rows_and_follows_relink() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jsonl");
        let second = dir.path().join("second.jsonl");
        tokio::fs::write(&first, "{\"type\":\"user\",\"uuid\":\"one\"}\n")
            .await
            .unwrap();
        tokio::fs::write(&second, "{\"type\":\"assistant\",\"uuid\":\"two\"}\n")
            .await
            .unwrap();
        let mut tailer = TranscriptTailer::follow(first);
        let mut rows = tailer.rows();
        assert_eq!(rows.recv().await.unwrap().as_value()["uuid"], "one");
        assert_eq!(
            rows.recv().await.unwrap().as_value()["type"],
            "amux.transcript_ready"
        );
        tailer.relink(second);
        assert_eq!(rows.recv().await.unwrap().as_value()["uuid"], "two");
    }

    #[tokio::test]
    async fn tailer_waits_for_a_transcript_created_after_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("later.jsonl");
        let tailer = TranscriptTailer::follow(path.clone());
        let mut rows = tailer.rows();
        drop(tokio::fs::File::create(&path).await.unwrap());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rows.recv())
                .await
                .is_err(),
            "a newly created transcript is not ready before its initial write"
        );
        tokio::fs::write(&path, "{\"type\":\"system\",\"subtype\":\"ready\"}\n")
            .await
            .unwrap();
        let row = tokio::time::timeout(Duration::from_secs(2), rows.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(row, TranscriptRow::System(_)));
    }
}
