//! Cloud API client for amux.
//!
//! Fetches cloud routing connection details from the cloud API.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use thiserror::Error;

use crate::auth::{AccessToken, AuthError, CredentialProvider};
use crate::config::Config;
use crate::setup;

#[derive(Debug, Error)]
pub(crate) enum CloudError {
    #[error("Not authenticated - run 'amux init' to authenticate")]
    NotAuthenticated,
    #[error("Cloud mode is disabled")]
    CloudDisabled,
    #[error("Connection failed: {0}")]
    Connection(String),
    #[error("Authentication failed: {0}")]
    Auth(String),
}

/// Response from the cloud `/api/connect` endpoint.
#[derive(Debug, Deserialize)]
struct ApiConnectResult {
    host: String,
    port: u16,
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct CloudRoutingConnectionDetails {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) token: String,
    pub(crate) expires_at: DateTime<Utc>,
}

impl From<ApiConnectResult> for CloudRoutingConnectionDetails {
    fn from(result: ApiConnectResult) -> Self {
        Self {
            host: result.host,
            port: result.port,
            token: result.token,
            expires_at: result.expires_at,
        }
    }
}

/// Fetch connection details from the cloud API using the consumer-owned token.
///
/// Shared by both initial connection and connection-JWT refresh. The
/// `CredentialProvider` owns access-token refresh and persistence; this layer
/// only presents its bearer to `/api/connect`.
async fn refresh_and_fetch_connection(
    config: &Config,
    credentials: &dyn CredentialProvider,
) -> std::result::Result<ApiConnectResult, CloudError> {
    let access_token = credentials
        .access_token()
        .await
        .map_err(cloud_error_from_auth)?;
    match fetch_connection(&config.cloud_url, credentials, &access_token).await {
        Ok(connection) => Ok(connection),
        Err(CloudError::Auth(_)) => {
            let access_token = credentials
                .access_token()
                .await
                .map_err(cloud_error_from_auth)?;
            fetch_connection(&config.cloud_url, credentials, &access_token).await
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn fetch_routing_connection_details(
    config: &Config,
    credentials: &dyn CredentialProvider,
) -> std::result::Result<CloudRoutingConnectionDetails, CloudError> {
    if !setup::cloud_enabled(config) {
        return Err(CloudError::CloudDisabled);
    }
    refresh_and_fetch_connection(config, credentials)
        .await
        .map(Into::into)
}

async fn fetch_connection(
    cloud_url: &str,
    credentials: &dyn CredentialProvider,
    access_token: &AccessToken,
) -> std::result::Result<ApiConnectResult, CloudError> {
    let response = reqwest::Client::new()
        .get(format!("{cloud_url}/api/connect"))
        .bearer_auth(&access_token.bearer)
        .send()
        .await
        .map_err(|error| CloudError::Connection(error.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        credentials.invalidate(access_token);
        return Err(CloudError::Auth("invalid credentials".to_string()));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(CloudError::Connection(format!(
            "API returned {status}: {body}"
        )));
    }

    response
        .json()
        .await
        .map_err(|error| CloudError::Connection(error.to_string()))
}

fn cloud_error_from_auth(error: AuthError) -> CloudError {
    match error {
        AuthError::Unauthenticated => CloudError::NotAuthenticated,
        AuthError::Provider(message) => CloudError::Connection(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_remain_retriable_cloud_connection_errors() {
        assert!(matches!(
            cloud_error_from_auth(AuthError::Provider("temporary failure".to_string())),
            CloudError::Connection(_)
        ));
        assert!(matches!(
            cloud_error_from_auth(AuthError::Unauthenticated),
            CloudError::NotAuthenticated
        ));
    }
}
