//! Claude Code version probing and shared observation cache.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use semver::Version;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::watch;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeVersion(pub Version);

impl FromStr for ClaudeVersion {
    type Err = semver::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .split_whitespace()
            .next()
            .unwrap_or(value)
            .parse()
            .map(Self)
    }
}

impl std::fmt::Display for ClaudeVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("could not run Claude version probe: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude version probe timed out")]
    Timeout,
    #[error("Claude version probe failed with {0}")]
    Status(std::process::ExitStatus),
    #[error("Claude version output was not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("Claude version output was empty")]
    Empty,
    #[error("invalid Claude version `{value}`: {source}")]
    Invalid {
        value: String,
        source: semver::Error,
    },
}

pub async fn probe_version(binary: &Path) -> Result<ClaudeVersion, VersionError> {
    let mut command = Command::new(binary);
    command.arg("--version").kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| VersionError::Timeout)??;
    if !output.status.success() {
        return Err(VersionError::Status(output.status));
    }
    let raw = String::from_utf8(output.stdout)?.trim().to_string();
    if raw.is_empty() {
        return Err(VersionError::Empty);
    }
    raw.parse()
        .map_err(|source| VersionError::Invalid { value: raw, source })
}

struct VersionCacheInner {
    probe_complete: OnceLock<watch::Receiver<bool>>,
    version: RwLock<Option<ClaudeVersion>>,
}

/// One process-wide semantic version observation shared by hosted sessions.
#[derive(Clone)]
pub struct VersionCache {
    inner: Arc<VersionCacheInner>,
}

impl Default for VersionCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(VersionCacheInner {
                probe_complete: OnceLock::new(),
                version: RwLock::new(None),
            }),
        }
    }
}

impl VersionCache {
    pub async fn probe_once(&self) {
        self.probe_once_with(Path::new("claude")).await;
    }

    pub async fn probe_once_with(&self, binary: &Path) {
        let mut complete = self
            .inner
            .probe_complete
            .get_or_init(|| {
                let (complete_tx, complete_rx) = watch::channel(false);
                let cache = self.clone();
                let binary = binary.to_path_buf();
                tokio::spawn(async move {
                    let probed = probe_version(&binary).await.ok();
                    let mut version = cache
                        .inner
                        .version
                        .write()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if version.is_none() {
                        *version = probed;
                    }
                    complete_tx.send_replace(true);
                });
                complete_rx
            })
            .clone();
        if !*complete.borrow() {
            let _ = complete.changed().await;
        }
    }

    pub fn current(&self) -> Option<ClaudeVersion> {
        self.inner
            .version
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn observe_transcript_row(&self, row: &Value) {
        let Some(observed) = row
            .get("version")
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
        else {
            return;
        };
        let mut version = self
            .inner
            .version
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if version.as_ref() != Some(&observed) {
            *version = Some(observed);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn write_command(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[tokio::test]
    async fn probe_parses_semantic_version_from_cli_banner() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("fake-claude.sh");
        write_command(&command, "printf '%s\\n' '2.1.251 (Claude Code)'");
        assert_eq!(
            probe_version(&command).await.unwrap().to_string(),
            "2.1.251"
        );
    }

    #[tokio::test]
    async fn cache_probes_once_and_accepts_newer_transcript_fact() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("fake-claude.sh");
        let count = dir.path().join("count");
        write_command(
            &command,
            &format!(
                "printf x >> '{}'; printf '%s\\n' '2.1.250 (Claude Code)'",
                count.display()
            ),
        );
        let cache = VersionCache::default();
        cache.probe_once_with(&command).await;
        cache.probe_once_with(&command).await;
        assert_eq!(std::fs::read_to_string(count).unwrap(), "x");
        cache.observe_transcript_row(&serde_json::json!({"version":"2.1.251"}));
        assert_eq!(cache.current().unwrap().to_string(), "2.1.251");
    }
}
