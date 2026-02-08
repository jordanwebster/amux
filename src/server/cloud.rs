use super::connection::{connection_loop_with_refresh, ConnectionContext};
use super::ServerState;
use crate::cloud::{CloudConnection, CloudError};
use crate::config::Config;
use crate::error::AmuxError;
use crate::message::Message;
use crate::state::State;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

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
            log!("cloud: cloud mode not enabled, skipping connection");
            return;
        }

        let mut backoff = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(300);

        loop {
            log!("cloud: attempting connection");
            match run_cloud_connection(&config, state.clone(), event_tx.clone()).await {
                Ok(()) => {
                    log!("cloud: connection closed cleanly");
                    backoff = Duration::from_secs(1);
                }
                Err(CloudConnectionError::NonRetriable(msg)) => {
                    log!("cloud: non-retriable error, stopping: {}", msg);
                    return;
                }
                Err(CloudConnectionError::Retriable(msg)) => {
                    log!("cloud: retriable error: {}", msg);
                }
            }

            let should_reconnect = State::load(&config.state_path)
                .map(|s| s.cloud.use_cloud_mode == Some(true))
                .unwrap_or(false);

            if !should_reconnect {
                log!("cloud: cloud mode disabled, stopping reconnection");
                return;
            }

            log!("cloud: reconnecting in {:?}", backoff);
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
}

/// Run a single cloud connection attempt.
///
/// Returns Ok(()) on clean disconnect, or an error indicating whether to retry.
async fn run_cloud_connection(
    config: &Config,
    state: Arc<RwLock<ServerState>>,
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
        Err(e) => {
            return Err(CloudConnectionError::Retriable(format!(
                "Connection failed: {}",
                e
            )));
        }
    };

    let (mut transport, token_refresh) = conn.into_parts();
    let link_name = token_refresh.link_name.clone();

    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
    {
        let mut state = state.write().await;
        state.routes.insert(link_name.clone(), outgoing_tx);
    }
    log!("cloud: route established as {}", link_name);

    let ctx = ConnectionContext {
        state: state.clone(),
        event_tx,
        link_name: link_name.clone(),
    };

    let result =
        connection_loop_with_refresh(&mut transport, outgoing_rx, ctx, Some(token_refresh)).await;

    {
        let mut state = state.write().await;
        state.routes.remove(&link_name);
    }
    log!("cloud: route {} removed", link_name);

    match result {
        Ok(()) => Ok(()),
        Err(AmuxError::InvalidCredentials) => Err(CloudConnectionError::NonRetriable(
            "Invalid credentials".to_string(),
        )),
        Err(e) => Err(CloudConnectionError::Retriable(e.to_string())),
    }
}
