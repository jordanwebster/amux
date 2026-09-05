//! CLI-owned cloud authentication.
//!
//! The library only asks for access tokens through `CredentialProvider`; this
//! module owns the terminal-specific device flow and refresh-token persistence.
//! Refresh tokens live in `auth.yaml`, a CLI-owned sibling of amux's
//! `state.yaml`, so library state never contains consumer credentials.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, SystemTime};

use amux::{AccessToken, AuthError, CredentialProvider, OAuthError, refresh_access_token};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

const CACHE_REFRESH_MARGIN: StdDuration = StdDuration::from_secs(60);

pub struct DeviceFlowProvider {
    auth_path: PathBuf,
    cloud_url: String,
    cached_access_token: Mutex<Option<AccessToken>>,
    refresh_lock: AsyncMutex<()>,
}

impl DeviceFlowProvider {
    pub fn new(auth_path: PathBuf, cloud_url: String) -> Self {
        Self {
            auth_path,
            cloud_url,
            cached_access_token: Mutex::new(None),
            refresh_lock: AsyncMutex::new(()),
        }
    }

    /// True if a refresh token is on disk.
    pub fn is_authenticated(&self) -> bool {
        load_refresh_token(&self.auth_path).ok().flatten().is_some()
    }
}

#[async_trait::async_trait]
impl CredentialProvider for DeviceFlowProvider {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        if let Some(token) = self.current_cached_token() {
            return Ok(token);
        }

        // Serialize refresh so queued callers observe a successful rotated
        // token in the cache instead of spending the same refresh token again.
        let _refresh_guard = self.refresh_lock.lock().await;
        if let Some(token) = self.current_cached_token() {
            return Ok(token);
        }

        let refresh_token = load_refresh_token(&self.auth_path)
            .map_err(|error| AuthError::Provider(error.to_string()))?
            .ok_or(AuthError::Unauthenticated)?;

        let (access_token, new_refresh_token) =
            refresh_access_token(&self.cloud_url, &refresh_token)
                .await
                .map_err(auth_error_from_oauth)?;

        if let Some(new_refresh_token) = new_refresh_token {
            set_refresh_token(&self.auth_path, Some(new_refresh_token))
                .map_err(|error| AuthError::Provider(error.to_string()))?;
        }

        *self
            .cached_access_token
            .lock()
            .expect("access-token cache mutex poisoned") = Some(access_token.clone());

        Ok(access_token)
    }

    fn invalidate(&self, token: &AccessToken) {
        let mut cached = self
            .cached_access_token
            .lock()
            .expect("access-token cache mutex poisoned");
        if cached
            .as_ref()
            .is_some_and(|cached| cached.bearer == token.bearer)
        {
            *cached = None;
        }
    }
}

impl DeviceFlowProvider {
    fn current_cached_token(&self) -> Option<AccessToken> {
        let cached = self
            .cached_access_token
            .lock()
            .expect("access-token cache mutex poisoned");
        cached
            .as_ref()
            .filter(|token| token_is_current(token))
            .cloned()
    }
}

fn token_is_current(token: &AccessToken) -> bool {
    token
        .expires_at
        .is_some_and(|expires_at| expires_at > SystemTime::now() + CACHE_REFRESH_MARGIN)
}

fn auth_error_from_oauth(error: OAuthError) -> AuthError {
    match error {
        OAuthError::RefreshTokenExpired => AuthError::Unauthenticated,
        other => AuthError::Provider(other.to_string()),
    }
}

#[derive(Debug, Error)]
pub enum AuthStateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse state file: {0}")]
    Parse(#[from] serde_yaml::Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

pub(crate) fn auth_file_path(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("auth.yaml")
}

fn load_refresh_token(path: &Path) -> Result<Option<String>, AuthStateError> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)?;

    if contents.trim().is_empty() {
        return Ok(None);
    }

    let file: AuthFile = serde_yaml::from_str(&contents)?;
    Ok(file.refresh_token)
}

fn set_refresh_token(path: &Path, token: Option<String>) -> Result<(), AuthStateError> {
    let auth_file = AuthFile {
        refresh_token: token,
    };
    atomic_write_auth_file(path, &auth_file)
}

fn atomic_write_auth_file(path: &Path, auth_file: &AuthFile) -> Result<(), AuthStateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = temp_auth_path(path);
    let yaml = serde_yaml::to_string(auth_file)?;

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(&tmp_path)?;
    file.write_all(yaml.as_bytes())?;
    file.sync_all()?;
    drop(file);

    fs::rename(tmp_path, path)?;
    Ok(())
}

fn temp_auth_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.yaml");
    path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn refresh_token_roundtrips_through_auth_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");

        set_refresh_token(&path, Some("refresh".to_string())).unwrap();

        assert_eq!(
            load_refresh_token(&path).unwrap().as_deref(),
            Some("refresh")
        );
        let yaml = fs::read_to_string(&path).unwrap();
        assert!(yaml.contains("refresh_token"));

        set_refresh_token(&path, None).unwrap();

        assert_eq!(load_refresh_token(&path).unwrap(), None);
    }

    #[test]
    fn refresh_token_replaces_existing_auth_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");

        set_refresh_token(&path, Some("first-refresh".to_string())).unwrap();
        set_refresh_token(&path, Some("second-refresh".to_string())).unwrap();

        assert_eq!(
            load_refresh_token(&path).unwrap().as_deref(),
            Some("second-refresh")
        );
    }

    #[test]
    fn auth_file_rejects_unknown_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");
        fs::write(&path, "refresh_token: refresh\nlegacy: true\n").unwrap();

        let error = load_refresh_token(&path).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn provider_reports_authentication_from_state_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");
        let provider = DeviceFlowProvider::new(path.clone(), "https://example.com".to_string());

        assert!(!provider.is_authenticated());
        set_refresh_token(&path, Some("refresh".to_string())).unwrap();
        assert!(provider.is_authenticated());
    }

    #[test]
    fn invalidating_matching_access_token_preserves_refresh_token() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");
        set_refresh_token(&path, Some("refresh".to_string())).unwrap();
        let provider = DeviceFlowProvider::new(path.clone(), "https://example.com".to_string());
        let token = AccessToken {
            bearer: "access".to_string(),
            expires_at: Some(SystemTime::now() + StdDuration::from_secs(3600)),
        };
        *provider
            .cached_access_token
            .lock()
            .expect("access-token cache mutex poisoned") = Some(token.clone());

        provider.invalidate(&token);

        assert!(provider.cached_access_token.lock().unwrap().is_none());
        assert_eq!(
            load_refresh_token(&path).unwrap().as_deref(),
            Some("refresh")
        );
    }

    #[test]
    fn invalidating_different_access_token_keeps_cached_token() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");
        let provider = DeviceFlowProvider::new(path, "https://example.com".to_string());
        let token = AccessToken {
            bearer: "access".to_string(),
            expires_at: Some(SystemTime::now() + StdDuration::from_secs(3600)),
        };
        *provider
            .cached_access_token
            .lock()
            .expect("access-token cache mutex poisoned") = Some(token.clone());

        provider.invalidate(&AccessToken {
            bearer: "other".to_string(),
            expires_at: token.expires_at,
        });

        assert_eq!(
            provider
                .cached_access_token
                .lock()
                .unwrap()
                .as_ref()
                .map(|token| token.bearer.as_str()),
            Some("access")
        );
    }

    #[test]
    fn oauth_refresh_error_classification_preserves_retriable_provider_errors() {
        assert!(matches!(
            auth_error_from_oauth(OAuthError::RefreshTokenExpired),
            AuthError::Unauthenticated
        ));
        assert!(matches!(
            auth_error_from_oauth(OAuthError::Request("bad gateway".to_string())),
            AuthError::Provider(message) if message.contains("bad gateway")
        ));
    }

    #[test]
    fn auth_file_path_is_state_sibling() {
        assert_eq!(
            auth_file_path(Path::new("/tmp/amux/state.yaml")),
            PathBuf::from("/tmp/amux/auth.yaml")
        );
    }
}
