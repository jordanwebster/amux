//! Cloud connection manager for amux.
//!
//! Manages the lifecycle of outbound connections to cloud servers, including:
//! - Initial TLS connection with token authentication
//! - Automatic token refresh before expiry
//! - Message forwarding between local server and cloud

use crate::config::Config;
use crate::error::AmuxError;
use crate::message::{DirectMessage, Message, PROTOCOL_VERSION, ProtocolError};
use crate::oauth;
use crate::route::generate_server_link;
use crate::state::State;
use crate::transport::{TcpTransport, Transport, tls_connect};
use chrono::{DateTime, Duration, Utc};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("Not authenticated - run 'amux init' to authenticate")]
    NotAuthenticated,
    #[error("Cloud mode is disabled")]
    CloudDisabled,
    #[error("Connection failed: {0}")]
    Connection(String),
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("Cloud server host changed - reconnection required")]
    HostChanged,
    #[error("OAuth error: {0}")]
    OAuth(#[from] oauth::OAuthError),
    #[error("Transport error: {0}")]
    Transport(#[from] AmuxError),
    #[error("State error: {0}")]
    State(#[from] crate::state::StateError),
    #[error("amux upgrade required (protocol v{server_version}, client v{client_version})")]
    VersionMismatch {
        server_version: u32,
        client_version: u32,
    },
}

/// Cloud connection state
pub struct CloudConnection {
    config: Config,
    transport: TcpTransport<TlsStream<TcpStream>>,
    link_name: String,
    current_host: String,
    current_port: u16,
    token_expires_at: DateTime<Utc>,
}

impl CloudConnection {
    /// Establish a new cloud connection.
    ///
    /// This will:
    /// 1. Load refresh token from state
    /// 2. Exchange it for an access token
    /// 3. Get connection details from cloud API
    /// 4. Connect via TLS and send Connect message with JWT
    pub async fn connect(config: &Config) -> std::result::Result<Self, CloudError> {
        let state = State::load(&config.state_path)?;

        // Check if cloud mode is enabled
        if state.cloud.use_cloud_mode != Some(true) {
            return Err(CloudError::CloudDisabled);
        }

        // Get refresh token
        let refresh_token = state
            .cloud
            .refresh_token
            .ok_or(CloudError::NotAuthenticated)?;

        // Exchange for access token
        let (access_token, new_refresh) =
            oauth::refresh_access_token(&config.cloud_url, &refresh_token).await?;

        // Update refresh token if rotated
        if let Some(new_token) = new_refresh {
            State::update(&config.state_path, |s| {
                s.cloud.refresh_token = Some(new_token);
            })?;
        }

        // Get connection details
        let conn = oauth::get_connection(&config.cloud_url, &access_token).await?;

        // Connect via TLS
        tracing::info!(host = %conn.host, port = conn.port, "connecting to cloud");
        let mut transport = tls_connect(&conn.host, conn.port)
            .await
            .map_err(|e| CloudError::Connection(e.to_string()))?;

        // Generate link name for this server
        let link_name = generate_server_link(&config.host_name, config.randomise_link_name);

        // Send Connect with token
        transport
            .write_message(&Message::Direct(DirectMessage::Connect {
                link_name: link_name.clone(),
                token: Some(conn.token),
                version: PROTOCOL_VERSION,
            }))
            .await?;

        // Wait for response
        let response = transport.read_message().await?;
        match response {
            Message::Direct(DirectMessage::ConnectResult { error: None }) => {
                tracing::info!(host = %conn.host, link = %link_name, "cloud connected");
            }
            Message::Direct(DirectMessage::ConnectResult {
                error: Some(ProtocolError::InvalidCredentials),
            }) => {
                // TODO: clear refresh token once connection flow is stable
                return Err(CloudError::Auth(
                    "Invalid credentials - please run 'amux init' to re-authenticate".to_string(),
                ));
            }
            Message::Direct(DirectMessage::ConnectResult {
                error:
                    Some(ProtocolError::VersionMismatch {
                        server_version,
                        client_version,
                    }),
            }) => {
                return Err(CloudError::VersionMismatch {
                    server_version,
                    client_version,
                });
            }
            Message::Direct(DirectMessage::ConnectResult { error }) => {
                return Err(CloudError::Connection(format!(
                    "Connect failed: {:?}",
                    error
                )));
            }
            _ => {
                return Err(CloudError::Connection(
                    "Unexpected response to Connect".to_string(),
                ));
            }
        }

        Ok(Self {
            config: config.clone(),
            transport,
            link_name,
            current_host: conn.host,
            current_port: conn.port,
            token_expires_at: conn.expires_at,
        })
    }

    /// Extract the underlying transport and token refresh state.
    /// This consumes the CloudConnection, allowing the transport to be
    /// used directly by the server's peer loop with token refresh.
    pub fn into_parts(self) -> (TcpTransport<TlsStream<TcpStream>>, TokenRefreshState) {
        let refresh_state = TokenRefreshState {
            config: self.config.clone(),
            link_name: self.link_name.clone(),
            current_host: self.current_host,
            current_port: self.current_port,
            token_expires_at: self.token_expires_at,
            pending_expires_at: None,
        };
        (self.transport, refresh_state)
    }
}

/// Token refresh state for cloud connections.
///
/// This is passed to connection_loop to enable automatic token refresh
/// on cloud connections. For non-cloud connections, None is passed.
pub struct TokenRefreshState {
    config: Config,
    pub link_name: String,
    current_host: String,
    current_port: u16,
    token_expires_at: DateTime<Utc>,
    /// Pending expires_at from the most recent send_connect, applied on success
    pending_expires_at: Option<DateTime<Utc>>,
}

impl TokenRefreshState {
    /// Calculate when the token refresh should occur (as tokio Instant).
    /// Refresh happens 5 minutes before expiry.
    pub fn refresh_deadline(&self) -> tokio::time::Instant {
        let refresh_at = self.token_expires_at - Duration::minutes(5);
        let now = Utc::now();

        if refresh_at <= now {
            // Already past refresh time - refresh immediately
            tokio::time::Instant::now()
        } else {
            let duration = (refresh_at - now).to_std().unwrap_or_default();
            tokio::time::Instant::now() + duration
        }
    }

    /// Refresh the OAuth token and send a Connect message through the outgoing channel.
    /// The ConnectResult will be intercepted by connection_loop.
    pub async fn send_connect(
        &mut self,
        tx: &mpsc::Sender<Message>,
    ) -> std::result::Result<(), CloudError> {
        let state = State::load(&self.config.state_path)?;

        let refresh_token = state
            .cloud
            .refresh_token
            .ok_or(CloudError::NotAuthenticated)?;

        // Get new access token
        let (access_token, new_refresh) =
            oauth::refresh_access_token(&self.config.cloud_url, &refresh_token).await?;

        // Update refresh token if rotated
        if let Some(new_token) = new_refresh {
            State::update(&self.config.state_path, |s| {
                s.cloud.refresh_token = Some(new_token);
            })?;
        }

        // Get new connection details
        let conn = oauth::get_connection(&self.config.cloud_url, &access_token).await?;

        // Check if host changed - requires full reconnection
        if conn.host != self.current_host || conn.port != self.current_port {
            return Err(CloudError::HostChanged);
        }

        // Store pending expires_at for use in handle_response
        self.pending_expires_at = Some(conn.expires_at);

        // Send new Connect with fresh token through the outgoing channel
        tx.send(Message::Direct(DirectMessage::Connect {
            link_name: self.link_name.clone(),
            token: Some(conn.token),
            version: PROTOCOL_VERSION,
        }))
        .await
        .map_err(|_| {
            CloudError::Connection("Outgoing channel closed during token refresh".to_string())
        })?;

        Ok(())
    }

    /// Handle a ConnectResult received after send_connect.
    /// Updates token_expires_at on success using the expires_at stored during send_connect.
    pub fn handle_response(&mut self, msg: &Message) -> std::result::Result<(), CloudError> {
        match msg {
            Message::Direct(DirectMessage::ConnectResult { error: None }) => {
                tracing::debug!("token refreshed");
                if let Some(expires_at) = self.pending_expires_at.take() {
                    self.token_expires_at = expires_at;
                }
                Ok(())
            }
            Message::Direct(DirectMessage::ConnectResult {
                error: Some(ProtocolError::InvalidCredentials),
            }) => Err(CloudError::Auth("Token refresh failed".to_string())),
            Message::Direct(DirectMessage::ConnectResult {
                error:
                    Some(ProtocolError::VersionMismatch {
                        server_version,
                        client_version,
                    }),
            }) => Err(CloudError::VersionMismatch {
                server_version: *server_version,
                client_version: *client_version,
            }),
            Message::Direct(DirectMessage::ConnectResult { error }) => Err(CloudError::Connection(
                format!("Token refresh failed: {:?}", error),
            )),
            _ => Err(CloudError::Connection(
                "Unexpected response to token refresh".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_error_display() {
        let err = CloudError::NotAuthenticated;
        assert!(err.to_string().contains("amux init"));

        let err = CloudError::CloudDisabled;
        assert!(err.to_string().contains("disabled"));
    }
}
