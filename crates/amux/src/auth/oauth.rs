//! OAuth 2.0 device flow implementation for amux cloud authentication.
//!
//! Uses the OAuth 2.0 Device Authorization Grant (RFC 8628) to authenticate
//! users without requiring a browser on the same machine as the CLI.

use chrono::{DateTime, Utc};
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::devicecode::StandardDeviceAuthorizationResponse;
use oauth2::reqwest::async_http_client;
use oauth2::{
    AuthUrl, ClientId, DeviceAuthorizationUrl, RefreshToken, RequestTokenError, Scope,
    TokenResponse, TokenUrl,
};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum OAuthError {
    #[error("OAuth configuration error: {0}")]
    Config(String),
    #[error("OAuth request error: {0}")]
    Request(String),
    #[error("Refresh token expired or revoked — run 'amux init' to re-authenticate")]
    RefreshTokenExpired,
    #[error("No refresh token returned")]
    NoRefreshToken,
    #[error("Unauthorized - token may be expired or revoked")]
    Unauthorized,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

/// Response from /api/connect endpoint
#[derive(Debug, Deserialize)]
pub(crate) struct ConnectResult {
    /// Cloud server hostname to connect to
    pub(crate) host: String,
    /// Cloud server port
    pub(crate) port: u16,
    /// JWT token for this connection
    pub(crate) token: String,
    /// When the token expires
    pub(crate) expires_at: DateTime<Utc>,
}

/// Run the OAuth 2.0 device authorization flow.
///
/// Returns a refresh token on success.
pub(crate) async fn device_flow(cloud_url: &str) -> Result<String, OAuthError> {
    let auth_url = AuthUrl::new(format!("{}/connect/authorize", cloud_url))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let token_url = TokenUrl::new(format!("{}/connect/token", cloud_url))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let device_auth_url =
        DeviceAuthorizationUrl::new(format!("{}/connect/deviceauthorization", cloud_url))
            .map_err(|e| OAuthError::Config(e.to_string()))?;

    let client = BasicClient::new(
        ClientId::new("cli".to_string()),
        None, // No client secret for public clients
        auth_url,
        Some(token_url),
    )
    .set_device_authorization_url(device_auth_url);

    // Request device code
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

    // Display instructions to user
    println!("\nTo authenticate, visit:");
    println!("  {}", details.verification_uri().as_str());
    if let Some(complete_uri) = details.verification_uri_complete() {
        println!("\nOr open this URL directly:");
        // VerificationUriComplete wraps the URL as a secret
        println!("  {}", complete_uri.secret());
    }
    println!("\nAnd enter code: {}", details.user_code().secret());
    println!("\nWaiting for authentication...");

    // Poll for token
    let token_response = client
        .exchange_device_access_token(&details)
        .request_async(async_http_client, tokio::time::sleep, None)
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;

    // Return refresh token
    token_response
        .refresh_token()
        .map(|t| t.secret().clone())
        .ok_or(OAuthError::NoRefreshToken)
}

/// Get a new access token using a refresh token.
///
/// Returns (access_token, new_refresh_token) where new_refresh_token is Some
/// if the server rotated the refresh token.
pub(crate) async fn refresh_access_token(
    cloud_url: &str,
    refresh_token: &str,
) -> Result<(String, Option<String>), OAuthError> {
    let token_url = TokenUrl::new(format!("{}/connect/token", cloud_url))
        .map_err(|e| OAuthError::Config(e.to_string()))?;

    // Auth URL is required but not used for refresh token exchange
    let auth_url = AuthUrl::new(format!("{}/connect/authorize", cloud_url))
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
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default();
                    OAuthError::Request(format!("{}{desc}", other.as_ref()))
                }
            },
            _ => OAuthError::Request(e.to_string()),
        })?;

    let access_token = token_response.access_token().secret().clone();
    let new_refresh = token_response.refresh_token().map(|t| t.secret().clone());

    Ok((access_token, new_refresh))
}

/// Fetch cloud connection details using an access token.
///
/// Calls /api/connect to get the specific server and JWT token for this connection.
pub(crate) async fn fetch_connection(
    cloud_url: &str,
    access_token: &str,
) -> Result<ConnectResult, OAuthError> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/connect", cloud_url))
        .bearer_auth(access_token)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(OAuthError::Unauthorized);
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(OAuthError::Request(format!(
            "API returned {}: {}",
            status, body
        )));
    }

    response.json().await.map_err(OAuthError::from)
}
