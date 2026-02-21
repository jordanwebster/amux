use super::connection::{ConnectionContext, connection_loop, reader_loop, writer_loop};
use super::routing::{handle_peer_disconnect, send_initial_announcements};
use super::{LOCAL_USER_ID, ServerState, ServerUserState, get_or_create_user_state};
use crate::cloud::{CloudConnection, CloudError};
use crate::config::Config;
use crate::error::AmuxError;
use crate::message::{LocalMessage, Message};
use crate::state::State;
use crate::transport::TransportSplit;
use std::sync::Arc;
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
    tokio::spawn(async move {
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
            match run_cloud_connection(&config, state.clone(), user_state.clone(), event_tx.clone())
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
                    let reason = format!(
                        "amux upgrade required (protocol v{}, client v{})",
                        server_version, client_version
                    );
                    tracing::error!(reason = %reason, "cloud version mismatch");
                    // Notify all attached terminals to exit cleanly
                    let us = user_state.read().await;
                    for (link, tx) in &us.routes {
                        if !us.peer_links.contains(link) {
                            let _ = tx.try_send(Message::Local(LocalMessage::ServerShutdown {
                                reason: reason.clone(),
                            }));
                        }
                    }
                    drop(us);
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
    });
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
        Err(CloudError::NotAuthenticated) | Err(CloudError::Auth(_)) => {
            return Err(CloudConnectionError::NonRetriable(
                "Authentication failed - run 'amux init' to re-authenticate".to_string(),
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
    let conn_span = tracing::info_span!("connection", link = %link_name, transport = "cloud");
    tracing::info!(parent: &conn_span, "cloud route established");

    // Split transport into reader/writer halves
    let (reader, writer) = transport.into_split();

    // Spawn reader and writer tasks
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let reader_handle =
        tokio::spawn(reader_loop(reader, incoming_tx).instrument(conn_span.clone()));
    let writer_handle =
        tokio::spawn(writer_loop(writer, outgoing_rx).instrument(conn_span.clone()));

    let ctx = ConnectionContext {
        state: state.clone(),
        user_state: user_state.clone(),
        user_id: LOCAL_USER_ID,
        event_tx,
        link_name: link_name.clone(),
    };

    let result = connection_loop(incoming_rx, outgoing_tx, ctx, Some(token_refresh))
        .instrument(conn_span.clone())
        .await;

    {
        let mut us = user_state.write().await;
        handle_peer_disconnect(&mut us, &link_name);
    }

    // Let writer drain, then abort reader
    let _ = writer_handle.await;
    reader_handle.abort();

    tracing::info!(parent: &conn_span, "cloud route removed");

    match result {
        Ok(()) => Ok(()),
        Err(AmuxError::InvalidCredentials) => Err(CloudConnectionError::NonRetriable(
            "Invalid credentials".to_string(),
        )),
        Err(AmuxError::VersionMismatch(_)) => Err(CloudConnectionError::VersionMismatch {
            server_version: 0,
            client_version: crate::message::PROTOCOL_VERSION,
        }),
        Err(e) => Err(CloudConnectionError::Retriable(e.to_string())),
    }
}
