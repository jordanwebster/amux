//! Cloud connection manager for amux.
//!
//! Manages the lifecycle of outbound connections to cloud servers, including:
//! - Initial TLS connection with token authentication
//! - Automatic token refresh before expiry
//! - Message forwarding between local server and cloud

use crate::auth::oauth;
use crate::config::Config;
use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::protocol::message::{DirectMessage, Message, ProtocolError};
use crate::protocol::route::generate_server_link;
use crate::state::State;
use crate::transport::{TcpTransport, Transport, TransportError, tls_connect};
use chrono::{DateTime, Duration, Utc};
use std::time::Duration as StdDuration;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;

/// Maximum time to wait for a handshake response from a cloud server.
/// Prevents hanging forever if the remote accepts TCP but never responds.
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(10);

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
    Transport(#[from] TransportError),
    #[error("State error: {0}")]
    State(#[from] crate::state::StateError),
    #[error(
        "protocol mismatch (server protocol v{server_version}, client protocol v{client_version})"
    )]
    ProtocolMismatch {
        server_version: u32,
        client_version: u32,
    },
    #[error("amux upgrade required (minimum v{minimum_version}, you have v{client_version})")]
    UpgradeRequired {
        minimum_version: String,
        client_version: String,
    },
}

/// Check a handshake ConnectResult, mapping protocol errors to CloudError.
fn check_handshake_connect_result(result: &ConnectResult) -> std::result::Result<(), CloudError> {
    match &result.error {
        None => Ok(()),
        Some(ProtocolError::InvalidCredentials) => {
            Err(CloudError::Auth("invalid credentials".to_string()))
        }
        Some(ProtocolError::ProtocolMismatch {
            server_version,
            client_version,
        }) => Err(CloudError::ProtocolMismatch {
            server_version: *server_version,
            client_version: *client_version,
        }),
        Some(ProtocolError::UpgradeRequired {
            minimum_version,
            client_version,
        }) => Err(CloudError::UpgradeRequired {
            minimum_version: minimum_version.clone(),
            client_version: client_version.clone(),
        }),
        Some(e) => Err(CloudError::Connection(format!("server rejected: {e}"))),
    }
}

/// Check a session ReauthResult message, mapping protocol errors to CloudError.
fn check_reauth_result(msg: &Message) -> std::result::Result<(), CloudError> {
    match msg {
        Message::Direct {
            message: DirectMessage::ReauthResult { error: None },
        } => Ok(()),
        Message::Direct {
            message:
                DirectMessage::ReauthResult {
                    error: Some(ProtocolError::InvalidCredentials),
                },
        } => Err(CloudError::Auth("invalid credentials".to_string())),
        Message::Direct {
            message:
                DirectMessage::ReauthResult {
                    error:
                        Some(ProtocolError::ProtocolMismatch {
                            server_version,
                            client_version,
                        }),
                },
        } => Err(CloudError::ProtocolMismatch {
            server_version: *server_version,
            client_version: *client_version,
        }),
        Message::Direct {
            message:
                DirectMessage::ReauthResult {
                    error:
                        Some(ProtocolError::UpgradeRequired {
                            minimum_version,
                            client_version,
                        }),
                },
        } => Err(CloudError::UpgradeRequired {
            minimum_version: minimum_version.clone(),
            client_version: client_version.clone(),
        }),
        Message::Direct {
            message: DirectMessage::ReauthResult { error: Some(e) },
        } => Err(CloudError::Connection(format!("server rejected: {e}"))),
        _ => Err(CloudError::Connection("expected ReauthResult".to_string())),
    }
}

/// Refresh the OAuth token and get connection details from the cloud API.
///
/// Shared by both initial connection and token refresh to avoid duplicating
/// the load-state → extract-token → refresh → rotate → get-connection sequence.
async fn refresh_and_get_connection(
    config: &Config,
) -> std::result::Result<oauth::ConnectResult, CloudError> {
    let state = State::load(&config.state_path)?;
    let refresh_token = state
        .cloud
        .refresh_token
        .ok_or(CloudError::NotAuthenticated)?;
    let (access_token, new_refresh) =
        oauth::refresh_access_token(&config.cloud_url, &refresh_token).await?;
    if let Some(new_token) = new_refresh {
        State::update(&config.state_path, |s| {
            s.cloud.refresh_token = Some(new_token);
        })?;
    }
    Ok(oauth::get_connection(&config.cloud_url, &access_token).await?)
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

        let conn = refresh_and_get_connection(config).await?;

        // Connect via TLS
        tracing::info!(host = %conn.host, port = conn.port, "connecting to cloud");
        let mut transport = tls_connect(&conn.host, conn.port)
            .await
            .map_err(|e| CloudError::Connection(e.to_string()))?;

        // Generate link name for this server
        let link_name = generate_server_link(&config.host_name, config.randomise_link_name);

        // Send Connect handshake with token
        let connect = Connect {
            link_name: link_name.clone(),
            token: Some(conn.token),
            version: PROTOCOL_VERSION,
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        let payload = connect
            .encode()
            .map_err(TransportError::SerializationEncode)?;
        transport.write_frame(&payload).await?;

        // Wait for response with timeout to prevent hanging on unresponsive peers
        let payload = tokio::time::timeout(CONNECT_TIMEOUT, transport.read_frame())
            .await
            .map_err(|_| CloudError::Connection("handshake timed out".to_string()))??;
        let response = ConnectResult::decode(&payload)
            .map_err(|e| CloudError::Connection(format!("invalid ConnectResult: {e}")))?;
        check_handshake_connect_result(&response)?;
        tracing::info!(host = %conn.host, link = %link_name, "cloud connected");

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
            config: self.config,
            link_name: self.link_name,
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
    /// Pending expires_at from the most recent send_reauth, applied on success
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

    /// Refresh the OAuth token and send a Reauth message through the outgoing channel.
    /// The ReauthResult will be intercepted by connection_loop.
    pub async fn send_reauth(
        &mut self,
        tx: &mpsc::Sender<Message>,
    ) -> std::result::Result<(), CloudError> {
        let conn = refresh_and_get_connection(&self.config).await?;

        // Check if host changed - requires full reconnection
        if conn.host != self.current_host || conn.port != self.current_port {
            return Err(CloudError::HostChanged);
        }

        // Store pending expires_at for use in handle_reauth_result
        self.pending_expires_at = Some(conn.expires_at);

        // Send Reauth with fresh token through the outgoing channel
        tx.send(Message::Direct {
            message: DirectMessage::Reauth { token: conn.token },
        })
        .await
        .map_err(|_| {
            CloudError::Connection("Outgoing channel closed during token refresh".to_string())
        })?;

        Ok(())
    }

    /// Handle a ReauthResult received after send_reauth.
    /// Updates token_expires_at on success using the expires_at stored during send_reauth.
    pub fn handle_reauth_result(&mut self, msg: &Message) -> std::result::Result<(), CloudError> {
        check_reauth_result(msg)?;
        tracing::debug!("token refreshed");
        if let Some(expires_at) = self.pending_expires_at.take() {
            self.token_expires_at = expires_at;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_refresh_state(expires_at: DateTime<Utc>) -> TokenRefreshState {
        TokenRefreshState {
            config: Config::default(),
            link_name: "test-link".to_string(),
            current_host: "test-host".to_string(),
            current_port: 9001,
            token_expires_at: expires_at,
            pending_expires_at: None,
        }
    }

    #[test]
    fn handle_reauth_result_success_updates_expiry() {
        let initial_expiry = Utc::now() + Duration::hours(1);
        let new_expiry = Utc::now() + Duration::hours(2);
        let mut state = test_refresh_state(initial_expiry);
        state.pending_expires_at = Some(new_expiry);

        let msg = Message::Direct {
            message: DirectMessage::ReauthResult { error: None },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(result.is_ok());
        assert_eq!(state.token_expires_at, new_expiry);
        assert!(state.pending_expires_at.is_none());
    }

    #[test]
    fn handle_reauth_result_success_without_pending_keeps_expiry() {
        let initial_expiry = Utc::now() + Duration::hours(1);
        let mut state = test_refresh_state(initial_expiry);

        let msg = Message::Direct {
            message: DirectMessage::ReauthResult { error: None },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(result.is_ok());
        assert_eq!(state.token_expires_at, initial_expiry);
    }

    #[test]
    fn handle_reauth_result_invalid_credentials() {
        let mut state = test_refresh_state(Utc::now() + Duration::hours(1));

        let msg = Message::Direct {
            message: DirectMessage::ReauthResult {
                error: Some(ProtocolError::InvalidCredentials),
            },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(matches!(result, Err(CloudError::Auth(_))));
    }

    #[test]
    fn handle_reauth_result_protocol_mismatch() {
        let mut state = test_refresh_state(Utc::now() + Duration::hours(1));

        let msg = Message::Direct {
            message: DirectMessage::ReauthResult {
                error: Some(ProtocolError::ProtocolMismatch {
                    server_version: 4,
                    client_version: 3,
                }),
            },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(matches!(
            result,
            Err(CloudError::ProtocolMismatch {
                server_version: 4,
                client_version: 3,
            })
        ));
    }

    #[test]
    fn handle_reauth_result_generic_server_error() {
        let mut state = test_refresh_state(Utc::now() + Duration::hours(1));

        let msg = Message::Direct {
            message: DirectMessage::ReauthResult {
                error: Some(ProtocolError::ServerError {
                    message: "something broke".to_string(),
                }),
            },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(matches!(result, Err(CloudError::Connection(_))));
    }

    #[test]
    fn handle_reauth_result_non_reauth_result_returns_error() {
        let mut state = test_refresh_state(Utc::now() + Duration::hours(1));

        // Not a ReauthResult at all — should be rejected
        let msg = Message::Command {
            command: crate::protocol::message::Command::Debug {
                verbose: false,
                format: crate::protocol::message::DebugFormat::Yaml,
            },
        };
        let result = state.handle_reauth_result(&msg);

        assert!(matches!(result, Err(CloudError::Connection(_))));
    }

    #[tokio::test]
    async fn refresh_deadline_future_expiry() {
        let state = test_refresh_state(Utc::now() + Duration::minutes(30));
        let deadline = state.refresh_deadline();
        let now = tokio::time::Instant::now();
        // Should be ~25 minutes from now (30 min expiry - 5 min buffer)
        let min_expected = now + std::time::Duration::from_secs(24 * 60);
        let max_expected = now + std::time::Duration::from_secs(26 * 60);
        assert!(
            deadline >= min_expected && deadline <= max_expected,
            "deadline should be ~25 minutes from now, got {:?} relative to now",
            deadline.duration_since(now)
        );
    }

    #[tokio::test]
    async fn refresh_deadline_past_expiry_refreshes_immediately() {
        let state = test_refresh_state(Utc::now() - Duration::minutes(10));
        let deadline = state.refresh_deadline();
        let now = tokio::time::Instant::now();
        // Past expiry should trigger immediate refresh
        assert!(
            deadline <= now + std::time::Duration::from_secs(1),
            "past expiry should return now"
        );
    }
}
