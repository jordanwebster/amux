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
    command.arg("--version");
    probe_command(command).await
}

async fn probe_command(mut command: Command) -> Result<ClaudeVersion, VersionError> {
    command.kill_on_drop(true);
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
        let binary = binary.to_path_buf();
        self.probe_once_using(move || async move { probe_version(&binary).await })
            .await;
    }

    async fn probe_once_using<F>(&self, probe: impl FnOnce() -> F)
    where
        F: std::future::Future<Output = Result<ClaudeVersion, VersionError>> + Send + 'static,
    {
        let mut complete = self
            .inner
            .probe_complete
            .get_or_init(|| {
                let (complete_tx, complete_rx) = watch::channel(false);
                let cache = self.clone();
                let probe = probe();
                tokio::spawn(async move {
                    let probed = probe.await.ok();
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

#[cfg(test)]
mod tests {
    use std::future::Future as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn parses_semantic_version_from_cli_banner() {
        assert_eq!(
            "2.1.251 (Claude Code)"
                .parse::<ClaudeVersion>()
                .unwrap()
                .to_string(),
            "2.1.251"
        );
        assert!("not a version".parse::<ClaudeVersion>().is_err());
    }

    #[tokio::test]
    async fn cache_probes_once_and_accepts_newer_transcript_fact() {
        let count = Arc::new(AtomicUsize::new(0));
        let cache = VersionCache::default();
        for _ in 0..2 {
            let count = count.clone();
            cache
                .probe_once_using(move || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok("2.1.250".parse().unwrap())
                })
                .await;
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(cache.current().unwrap().to_string(), "2.1.250");
        cache.observe_transcript_row(&serde_json::json!({"version":"2.1.251"}));
        assert_eq!(cache.current().unwrap().to_string(), "2.1.251");
    }

    #[tokio::test]
    async fn concurrent_probe_callers_wait_without_overwriting_a_transcript_fact() {
        let cache = VersionCache::default();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_cache = cache.clone();
        let first = tokio::spawn(async move {
            first_cache
                .probe_once_using(|| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok("2.1.250".parse().unwrap())
                })
                .await;
        });
        started_rx.await.unwrap();
        let second = cache.probe_once_using(|| async { panic!("probe ran twice") });
        tokio::pin!(second);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        cache.observe_transcript_row(&serde_json::json!({"version":"2.1.251"}));
        release_tx.send(()).unwrap();
        first.await.unwrap();
        second.await;
        assert_eq!(cache.current().unwrap().to_string(), "2.1.251");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_reports_process_output_and_failures() {
        for (script, expected) in [
            ("printf '2.1.251 (Claude Code)\\n'", "2.1.251"),
            ("printf '2.1.251'; exit 17", "status"),
            ("printf ''", "empty"),
            ("printf 'invalid'", "invalid"),
            ("printf '\\377'", "utf8"),
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let output = probe_command(command).await;
            match expected {
                "status" => assert!(
                    matches!(output, Err(VersionError::Status(status)) if status.code() == Some(17))
                ),
                "empty" => assert!(matches!(output, Err(VersionError::Empty))),
                "invalid" => assert!(matches!(output, Err(VersionError::Invalid { .. }))),
                "utf8" => assert!(matches!(output, Err(VersionError::Utf8(_)))),
                version => assert_eq!(output.unwrap().to_string(), version),
            }
        }
    }
}
