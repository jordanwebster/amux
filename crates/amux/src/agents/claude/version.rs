use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock as StdRwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::watch;

const CLAUDE_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

struct ClaudeVersionCacheInner {
    probe_complete: OnceLock<watch::Receiver<bool>>,
    version: StdRwLock<Option<String>>,
}

/// Daemon-owned Claude version discovery shared by every local session.
#[derive(Clone)]
pub(crate) struct ClaudeVersionCache {
    inner: Arc<ClaudeVersionCacheInner>,
}

impl Default for ClaudeVersionCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(ClaudeVersionCacheInner {
                probe_complete: OnceLock::new(),
                version: StdRwLock::new(None),
            }),
        }
    }
}

impl ClaudeVersionCache {
    /// Run the process probe once, or await the daemon's in-flight first probe.
    pub(crate) async fn probe_once(&self) {
        self.probe_once_with(Path::new("claude")).await;
    }

    async fn probe_once_with(&self, command: &Path) {
        let mut complete = self
            .inner
            .probe_complete
            .get_or_init(|| {
                let (complete_tx, complete_rx) = watch::channel(false);
                let cache = self.clone();
                let command = command.to_path_buf();
                tokio::spawn(async move {
                    let probed = probe_claude_version(&command).await;
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

    pub(crate) fn current(&self) -> Option<String> {
        self.inner
            .version
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub(super) fn observe_transcript_row(&self, row: &Value) {
        let Some(observed) = row.get("version").and_then(Value::as_str) else {
            return;
        };
        let mut version = self
            .inner
            .version
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if version.as_deref() != Some(observed) {
            *version = Some(observed.to_string());
        }
    }
}

async fn probe_claude_version(command: &Path) -> Option<String> {
    let mut command = Command::new(command);
    command.arg("--version").kill_on_drop(true);
    let output = match tokio::time::timeout(CLAUDE_VERSION_PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not run claude version probe; using PTY delivery");
            return None;
        }
        Err(_) => {
            tracing::warn!("claude version probe timed out; using PTY delivery");
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(status = %output.status, "claude version probe failed; using PTY delivery");
        return None;
    }
    let version = match String::from_utf8(output.stdout) {
        Ok(version) => version.trim().to_string(),
        Err(error) => {
            tracing::warn!(%error, "claude version output was not UTF-8; using PTY delivery");
            return None;
        }
    };
    if version.is_empty() {
        tracing::warn!("claude version probe returned no version; using PTY delivery");
        None
    } else {
        Some(version)
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
    async fn claude_version_probe_runs_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("claude");
        let count = dir.path().join("probe-count");
        write_command(
            &command,
            &format!(
                "printf x >> '{}'; printf '%s\\n' '2.1.223 (Claude Code)'",
                count.display()
            ),
        );
        let cache = ClaudeVersionCache::default();

        cache.probe_once_with(&command).await;
        write_command(&command, "printf '%s\\n' '2.1.224 (Claude Code)'");
        cache.probe_once_with(&command).await;

        assert_eq!(cache.current().as_deref(), Some("2.1.223 (Claude Code)"));
        assert_eq!(std::fs::read_to_string(count).unwrap(), "x");
    }

    #[tokio::test]
    async fn failed_claude_version_probe_is_cached_as_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-claude");
        let available = dir.path().join("claude");
        let cache = ClaudeVersionCache::default();

        cache.probe_once_with(&missing).await;
        write_command(&available, "printf '%s\\n' '2.1.224 (Claude Code)'");
        cache.probe_once_with(&available).await;

        assert_eq!(cache.current(), None);
    }

    #[tokio::test]
    async fn canceled_first_waiter_does_not_restart_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("claude");
        let count = dir.path().join("probe-count");
        write_command(
            &command,
            &format!(
                "sleep 0.2; printf x >> '{}'; printf '%s\\n' '2.1.224 (Claude Code)'",
                count.display()
            ),
        );
        let cache = ClaudeVersionCache::default();
        let first_cache = cache.clone();
        let first_command = command.clone();
        let first = tokio::spawn(async move {
            first_cache.probe_once_with(&first_command).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        first.abort();

        cache.probe_once_with(&command).await;

        assert_eq!(cache.current().as_deref(), Some("2.1.224 (Claude Code)"));
        assert_eq!(std::fs::read_to_string(count).unwrap(), "x");
    }
}
