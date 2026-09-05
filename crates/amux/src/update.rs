//! Update checking: fetches the release manifest and compares versions.

use serde::Deserialize;

mod markers;
pub use markers::MarkerFileReporter;

/// Select persistent per-profile status on desktop or callbacks owned by a host.
#[derive(Clone, Default)]
pub enum StatusReporters {
    #[default]
    None,
    MarkerFiles,
    Host {
        update: Option<std::sync::Arc<dyn UpdateReporter>>,
        subscription: Option<std::sync::Arc<dyn crate::SubscriptionReporter>>,
    },
}

pub(crate) struct ResolvedReporters {
    pub update: Option<std::sync::Arc<dyn UpdateReporter>>,
    pub subscription: Option<std::sync::Arc<dyn crate::SubscriptionReporter>>,
}

impl StatusReporters {
    pub(crate) fn resolve(&self, state_path: &std::path::Path) -> ResolvedReporters {
        match self {
            Self::None => ResolvedReporters {
                update: None,
                subscription: None,
            },
            Self::Host {
                update,
                subscription,
            } => ResolvedReporters {
                update: update.clone(),
                subscription: subscription.clone(),
            },
            Self::MarkerFiles => {
                let reporter = std::sync::Arc::new(MarkerFileReporter::from_state_path(state_path));
                ResolvedReporters {
                    update: Some(reporter.clone()),
                    subscription: Some(reporter),
                }
            }
        }
    }
}

/// Result of an update check.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub update_version: String,
}

#[derive(Debug, Clone)]
pub enum UpdateStatus {
    /// `Some(info)` reports an available update; `None` clears that status.
    Available(Option<UpdateInfo>),
    /// `Some(min)` reports a required minimum version; `None` clears it.
    Required(Option<String>),
}

pub trait UpdateReporter: Send + Sync + 'static {
    fn report(&self, status: UpdateStatus);
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
}

async fn fetch_manifest(url: &str) -> Result<Manifest, reqwest::Error> {
    let resp = reqwest::get(url).await?.error_for_status()?;
    resp.json().await
}

/// Check the remote manifest for a newer version. Returns `Some(UpdateInfo)` if
/// the manifest version is strictly greater than `current_version`.
pub async fn check_for_update(cloud_url: &str, current_version: &str) -> Option<UpdateInfo> {
    let manifest_url = format!("{}/manifest.json", cloud_url.trim_end_matches('/'));

    let manifest = match fetch_manifest(&manifest_url).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(error = %e, "update check failed");
            return None;
        }
    };

    let current = semver::Version::parse(current_version).ok()?;
    let latest = semver::Version::parse(&manifest.version).ok()?;

    if latest > current {
        Some(UpdateInfo {
            current_version: current_version.to_string(),
            update_version: manifest.version,
        })
    } else {
        None
    }
}
