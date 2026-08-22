use std::time::SystemTime;

use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::{
    AuthUrl, ClientId, DeviceAuthorizationUrl, RefreshToken, RequestTokenError, Scope,
    StandardDeviceAuthorizationResponse, TokenResponse, TokenUrl,
};
use thiserror::Error;

use crate::auth::AccessToken;

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("OAuth configuration error: {0}")]
    Config(String),
    #[error("OAuth request error: {0}")]
    Request(String),
    #[error("Refresh token expired or revoked - run 'amux init' to re-authenticate")]
    RefreshTokenExpired,
    #[error("No refresh token returned")]
    NoRefreshToken,
}

/// The HTTP client handed to oauth2. Redirects are refused: following one
/// would forward the bearer credentials to whatever host the redirect names.
fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| OAuthError::Config(e.to_string()))
}

/// Run the OAuth 2.0 device authorization flow.
pub async fn run_device_flow(cloud_url: &str) -> Result<String, OAuthError> {
    let auth_url = AuthUrl::new(format!("{cloud_url}/connect/authorize"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let token_url = TokenUrl::new(format!("{cloud_url}/connect/token"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let device_auth_url =
        DeviceAuthorizationUrl::new(format!("{cloud_url}/connect/deviceauthorization"))
            .map_err(|e| OAuthError::Config(e.to_string()))?;

    let client = BasicClient::new(ClientId::new("cli".to_string()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_device_authorization_url(device_auth_url);
    let http = http_client()?;

    let details: StandardDeviceAuthorizationResponse = client
        .exchange_device_code()
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("offline_access".to_string()))
        .add_scope(Scope::new("api".to_string()))
        .request_async(&http)
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
        .request_async(&http, tokio::time::sleep, None)
        .await
        .map_err(|e| OAuthError::Request(e.to_string()))?;

    token_response
        .refresh_token()
        .map(|token| token.secret().clone())
        .ok_or(OAuthError::NoRefreshToken)
}

/// Get a new access token using a refresh token.
pub async fn refresh_access_token(
    cloud_url: &str,
    refresh_token: &str,
) -> Result<(AccessToken, Option<String>), OAuthError> {
    let token_url = TokenUrl::new(format!("{cloud_url}/connect/token"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;
    let auth_url = AuthUrl::new(format!("{cloud_url}/connect/authorize"))
        .map_err(|e| OAuthError::Config(e.to_string()))?;

    let client = BasicClient::new(ClientId::new("cli".to_string()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url);
    let http = http_client()?;

    let token_response = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
        .request_async(&http)
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
