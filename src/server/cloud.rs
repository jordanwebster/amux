//! Cloud relay connection with automatic reconnection.
//!
//! Manages the outbound TLS connection from a local server to a cloud relay.
//! Handles exponential backoff on retriable errors, stops on auth failures, and
//! exits the process on protocol version mismatch (after notifying attached terminals).

use super::connection::{ConnectionContext, run_connection};
use super::routing::send_initial_announcements;
use super::{LOCAL_USER_ID, ServerState, ServerUserState, get_or_create_user_state};
use crate::cloud::{CloudConnection, CloudError};
use crate::config::Config;
use crate::error::AmuxError;
use crate::message::{Command, Message, ShutdownReason};
use crate::state::State;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tracing::Instrument;

/// Establish and maintain a cloud connection with automatic reconnection.
///
/// This function spawns a background task that:
/// 1. Checks if cloud mode is enabled in state
/// 2. Connects to the cloud server with exponential backoff on retriable errors
/// 3. Stops retrying on non-retriable errors (auth failures)
pub(super) fn establish_cloud_connection(
    config: Config,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
) {
    let cloud_span = tracing::info_span!("cloud", url = %config.cloud_url);
    tokio::spawn(
        async move {
            let should_connect = State::load(&config.state_path)
                .map(|s| s.cloud.use_cloud_mode == Some(true))
                .unwrap_or(false);

            if !should_connect {
                tracing::info!("cloud mode not enabled");
                return;
            }

            // Get the default user state for cloud connections
            let user_state = get_or_create_user_state(&state, LOCAL_USER_ID).await;

            let mut backoff = Duration::from_secs(1);
            const MAX_BACKOFF: Duration = Duration::from_secs(300);

            loop {
                tracing::info!("attempting cloud connection");
                match run_cloud_connection(
                    &config,
                    state.clone(),
                    user_state.clone(),
                    event_tx.clone(),
                )
                .await
                {
                    Ok(()) => {
                        tracing::info!("cloud connection closed cleanly");
                        backoff = Duration::from_secs(1);
                    }
                    Err(CloudConnectionError::VersionMismatch {
                        server_version,
                        client_version,
                    }) => {
                        tracing::error!(server_version, client_version, "cloud version mismatch");
                        // Notify all attached terminals to exit cleanly
                        let us = user_state.read().await;
                        for (link, tx) in &us.routes {
                            if !us.peer_links.contains(link) {
                                let _ = tx.try_send(Message::Command(
                                    Command::ShutdownNotification(ShutdownReason::ProtocolMismatch),
                                ));
                            }
                        }
                        drop(us);
                        // Give writer tasks time to flush notifications to transports.
                        // try_send only places messages in channel buffers; without this
                        // yield, process::exit kills the runtime before writers can drain
                        // them, so terminals see "connection reset" not "amux upgrade required".
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        std::process::exit(1);
                    }
                    Err(CloudConnectionError::NonRetriable(msg)) => {
                        tracing::error!(error = %msg, "cloud non-retriable error, stopping");
                        return;
                    }
                    Err(CloudConnectionError::Retriable(msg)) => {
                        tracing::warn!(error = %msg, "cloud connection error, will retry");
                    }
                }

                let should_reconnect = State::load(&config.state_path)
                    .map(|s| s.cloud.use_cloud_mode == Some(true))
                    .unwrap_or(false);

                if !should_reconnect {
                    tracing::info!("cloud mode disabled, stopping reconnection");
                    return;
                }

                tracing::info!(backoff = ?backoff, "reconnecting to cloud");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
            }
        }
        .instrument(cloud_span),
    );
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable(String),
    /// Error that should stop reconnection attempts (auth failure)
    NonRetriable(String),
    /// Protocol version mismatch — notify terminals and exit
    VersionMismatch {
        server_version: u32,
        client_version: u32,
    },
}

/// Run a single cloud connection attempt.
///
/// Returns Ok(()) on clean disconnect, or an error indicating whether to retry.
async fn run_cloud_connection(
    config: &Config,
    state: Arc<RwLock<ServerState>>,
    user_state: Arc<RwLock<ServerUserState>>,
    event_tx: mpsc::Sender<super::SessionEvent>,
) -> std::result::Result<(), CloudConnectionError> {
    let conn = match CloudConnection::connect(config).await {
        Ok(conn) => conn,
        Err(CloudError::NotAuthenticated)
        | Err(CloudError::Auth(_))
        | Err(CloudError::OAuth(crate::oauth::OAuthError::RefreshTokenExpired)) => {
            return Err(CloudConnectionError::NonRetriable(
                "Authentication failed — run 'amux init' to re-authenticate".to_string(),
            ));
        }
        Err(CloudError::CloudDisabled) => {
            return Err(CloudConnectionError::NonRetriable(
                "Cloud mode disabled".to_string(),
            ));
        }
        Err(CloudError::VersionMismatch {
            server_version,
            client_version,
        }) => {
            return Err(CloudConnectionError::VersionMismatch {
                server_version,
                client_version,
            });
        }
        Err(e) => {
            return Err(CloudConnectionError::Retriable(format!(
                "Connection failed: {}",
                e
            )));
        }
    };

    let (transport, token_refresh) = conn.into_parts();
    let link_name = token_refresh.link_name.clone();

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let (host_id, host_name, is_cloud_server) = {
            let s = state.read().await;
            (s.host_id, s.config.host_name.clone(), s.is_cloud_server)
        };
        let mut us = user_state.write().await;
        us.routes.insert(link_name.clone(), outgoing_tx.clone());
        us.peer_links.insert(link_name.clone());
        send_initial_announcements(&us, host_id, &host_name, is_cloud_server, &link_name);
    }
    let conn_span = tracing::info_span!("connection", link = %link_name, transport = "cloud", user_id = %LOCAL_USER_ID);
    tracing::info!(parent: &conn_span, "cloud route established");

    let ctx = ConnectionContext {
        state,
        user_state,
        user_id: LOCAL_USER_ID,
        event_tx,
        link_name: link_name.clone(),
        is_local: false,
        next_request_id: Arc::new(AtomicU64::new(1)),
    };

    let result = run_connection(
        transport,
        outgoing_rx,
        outgoing_tx,
        ctx,
        Some(token_refresh),
        conn_span,
    )
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(AmuxError::InvalidCredentials) => Err(CloudConnectionError::NonRetriable(
            "Invalid credentials — run 'amux init' to re-authenticate".to_string(),
        )),
        Err(AmuxError::VersionMismatch(_)) => Err(CloudConnectionError::VersionMismatch {
            server_version: 0,
            client_version: crate::message::PROTOCOL_VERSION,
        }),
        Err(e) => Err(CloudConnectionError::Retriable(e.to_string())),
    }
}
