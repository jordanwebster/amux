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

use amux::{AccessToken, AuthError, CredentialProvider};
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::devicecode::StandardDeviceAuthorizationResponse;
use oauth2::reqwest::async_http_client;
use oauth2::{
    AuthUrl, ClientId, DeviceAuthorizationUrl, RefreshToken, RequestTokenError, Scope,
    TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const CACHE_REFRESH_MARGIN: StdDuration = StdDuration::from_secs(60);

pub struct DeviceFlowProvider {
    auth_path: PathBuf,
    cloud_url: String,
    cached_access_token: Mutex<Option<AccessToken>>,
}

impl DeviceFlowProvider {
    pub fn new(auth_path: PathBuf, cloud_url: String) -> Self {
        Self {
            auth_path,
            cloud_url,
            cached_access_token: Mutex::new(None),
        }
    }

    /// Run device flow interactively and persist the resulting refresh token.
    pub async fn run_device_flow(&self) -> Result<(), DeviceFlowError> {
        let refresh_token = device_flow(&self.cloud_url).await?;
        set_refresh_token(&self.auth_path, Some(refresh_token))?;
        Ok(())
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
pub enum DeviceFlowError {
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    #[error(transparent)]
    State(#[from] AuthStateError),
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("OAuth configuration error: {0}")]
    Config(String),
    #[error("OAuth request error: {0}")]
    Request(String),
    #[error("Refresh token expired or revoked — run 'amux init' to re-authenticate")]
    RefreshTokenExpired,
    #[error("No refresh token returned")]
    NoRefreshToken,
}

#[derive(Debug, Error)]
pub enum AuthStateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse state file: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Run the OAuth 2.0 device authorization flow.
async fn device_flow(cloud_url: &str) -> Result<String, OAuthError> {
    let auth_url = AuthUrl::new(format!("{cloud_url}/connect/authorize"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let token_url = TokenUrl::new(format!("{cloud_url}/connect/token"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let device_auth_url =
        DeviceAuthorizationUrl::new(format!("{cloud_url}/connect/deviceauthorization"))
            .map_err(|e| OAuthError::Config(e.to_string()))?;

    let client = BasicClient::new(
        ClientId::new("cli".to_string()),
        None,
        auth_url,
        Some(token_url),
    )
    .set_device_authorization_url(device_auth_url);

    let device_auth_request = client
        .exchange_device_code()
        .map_err(|e| OAuthError::Config(e.to_string()))?;

    let details: StandardDeviceAuthorizationResponse = device_auth_request
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("offline_access".to_string()))
        .add_scope(Scope::new("api".to_string()))
        .request_async(async_http_client)
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;

    println!("\nTo authenticate, visit:");
    println!("  {}", details.verification_uri().as_str());
    if let Some(complete_uri) = details.verification_uri_complete() {
        println!("\nOr open this URL directly:");
        println!("  {}", complete_uri.secret());
    }
    println!("\nAnd enter code: {}", details.user_code().secret());
    println!("\nWaiting for authentication...");

    let token_response = client
        .exchange_device_access_token(&details)
        .request_async(async_http_client, tokio::time::sleep, None)
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;

    token_response
        .refresh_token()
        .map(|token| token.secret().clone())
        .ok_or(OAuthError::NoRefreshToken)
}

/// Get a new access token using a refresh token.
async fn refresh_access_token(
    cloud_url: &str,
    refresh_token: &str,
) -> Result<(AccessToken, Option<String>), OAuthError> {
    let token_url = TokenUrl::new(format!("{cloud_url}/connect/token"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let auth_url = AuthUrl::new(format!("{cloud_url}/connect/authorize"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;

    let client = BasicClient::new(
        ClientId::new("cli".to_string()),
        None,
        auth_url,
        Some(token_url),
    );

    let token_response = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
        .request_async(async_http_client)
        .await
        .map_err(|e| match &e {
            RequestTokenError::ServerResponse(resp) => match resp.error() {
                BasicErrorResponseType::InvalidGrant => OAuthError::RefreshTokenExpired,
                other => {
                    let desc = resp
                        .error_description()
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default();
                    OAuthError::Request(format!("{}{desc}", other.as_ref()))
                }
            },
            _ => OAuthError::Request(e.to_string()),
        })?;

    let access_token = AccessToken {
        bearer: token_response.access_token().secret().clone(),
        expires_at: token_response
            .expires_in()
            .map(|duration| SystemTime::now() + duration),
    };
    let new_refresh = token_response
        .refresh_token()
        .map(|token| token.secret().clone());

    Ok((access_token, new_refresh))
}

#[derive(Debug, Default, Serialize, Deserialize)]
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

pub(crate) fn clear_refresh_token(auth_path: &Path) -> Result<(), AuthStateError> {
    set_refresh_token(auth_path, None)
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

        clear_refresh_token(&path).unwrap();

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
    fn provider_reports_authentication_from_state_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.yaml");
        let provider = DeviceFlowProvider::new(path.clone(), "https://example.com".to_string());

        assert!(!provider.is_authenticated());
        set_refresh_token(&path, Some("refresh".to_string())).unwrap();
        assert!(provider.is_authenticated());
    }

    #[test]
    fn auth_file_path_is_state_sibling() {
        assert_eq!(
            auth_file_path(Path::new("/tmp/amux/state.yaml")),
            PathBuf::from("/tmp/amux/auth.yaml")
        );
    }
}
