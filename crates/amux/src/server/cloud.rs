//! Cloud relay connection with automatic reconnection.
//!
//! Manages the outbound TLS connection from a local server to a cloud relay.
//! Handles exponential backoff on retriable errors, stops on auth failures, and
//! exits the process on protocol version mismatch (after notifying attached terminals).

use std::sync::Arc;
use std::time::Duration;

use prost::Message as _;
use tokio::sync::{RwLock, mpsc};
use tracing::Instrument;
use uuid::Uuid;

use super::connection::{
    ConnectionContext, ConnectionError, HeartbeatRole, HeartbeatSetup, RunConnection,
    run_connection,
};
use super::runtime::notify_local_clients;
use super::{
    LOCAL_USER_ID, ServerState, ServerUserState, ensure_user_state, local_host,
    validate_remote_host,
};
use crate::agent::SessionEvent;
use crate::auth::cloud::{CloudConnection, CloudError};
use crate::config::Config;
use crate::protocol::handshake::RoutingRole;
use crate::protocol::message::{FrameBody, Message, PeerFrame, RequestFrame, ShutdownReason};
use crate::protocol::{method, wire};
use crate::rpc::RpcPeerStreamOutboundStart;
use crate::setup;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const RELATIVE_JITTER_RATIO: f64 = 0.25;
const ABSOLUTE_JITTER_MAX: Duration = Duration::from_secs(5);
const BACKOFF_RESET_AFTER_ESTABLISHED: Duration = Duration::from_secs(30);

/// Establish and maintain a cloud connection with automatic reconnection.
///
/// This function spawns a background task that:
/// 1. Checks if cloud mode is enabled in state
/// 2. Connects to the cloud server with exponential backoff and jitter on retriable errors
/// 3. Stops retrying on non-retriable errors (auth failures)
pub(super) fn establish_cloud_connection(
    config: Config,
    state: Arc<RwLock<ServerState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) {
    let cloud_span = tracing::info_span!("cloud", url = %config.cloud_url);
    tokio::spawn(
        async move {
            if !setup::cloud_enabled(&config) {
                tracing::info!("cloud mode not enabled");
                return;
            }

            // Get the default user state for cloud connections
            let user_state = ensure_user_state(&state, LOCAL_USER_ID).await;

            let mut backoff = INITIAL_BACKOFF;

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
                        backoff = INITIAL_BACKOFF;
                    }
                    Err(CloudConnectionError::ProtocolMismatch {
                        server_version,
                        client_version,
                    }) => {
                        tracing::error!(server_version, client_version, "cloud protocol mismatch");
                        notify_local_clients(&user_state, ShutdownReason::UpdateRequired).await;
                        // Give writer tasks time to flush notifications to transports.
                        // try_send only places messages in channel buffers; without this
                        // yield, process::exit kills the runtime before writers can drain
                        // them, so terminals see "connection reset" not "amux update required".
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        std::process::exit(1);
                    }
                    Err(CloudConnectionError::UpdateRequired {
                        minimum_version,
                        client_version,
                    }) => {
                        tracing::error!(
                            minimum_version = %minimum_version,
                            client_version = %client_version,
                            "cloud requires newer client version"
                        );
                        crate::update::write_update_required(
                            &config.state_path,
                            &minimum_version,
                        );
                        return;
                    }
                    Err(CloudConnectionError::NonRetriable(msg)) => {
                        tracing::error!(error = %msg, "cloud non-retriable error, stopping");
                        return;
                    }
                    Err(CloudConnectionError::Retriable { msg, reset_backoff }) => {
                        if reset_backoff {
                            backoff = INITIAL_BACKOFF;
                        }
                        tracing::warn!(error = %msg, "cloud connection error, will retry");
                    }
                }

                if !setup::cloud_enabled(&config) {
                    tracing::info!("cloud mode disabled, stopping reconnection");
                    return;
                }

                let retry_delay = jittered_backoff(backoff);
                tracing::info!(base_backoff = ?backoff, retry_delay = ?retry_delay, "reconnecting to cloud");
                tokio::time::sleep(retry_delay).await;
                backoff = next_backoff(backoff);
            }
        }
        .instrument(cloud_span),
    );
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable { msg: String, reset_backoff: bool },
    /// Error that should stop reconnection attempts (auth failure)
    NonRetriable(String),
    /// Protocol version mismatch — notify terminals and exit
    ProtocolMismatch {
        server_version: u32,
        client_version: u32,
    },
    /// Client binary version is below the server's minimum requirement.
    UpdateRequired {
        minimum_version: String,
        client_version: String,
    },
}

/// Run a single cloud connection attempt.
///
/// Returns Ok(()) on clean disconnect, or an error indicating whether to retry.
async fn run_cloud_connection(
    config: &Config,
    state: Arc<RwLock<ServerState>>,
    user_state: Arc<RwLock<ServerUserState>>,
    event_tx: mpsc::Sender<SessionEvent>,
) -> std::result::Result<(), CloudConnectionError> {
    let host = {
        let state = state.read().await;
        local_host(state.host_id, &state.config.host_name)
    };
    let local_host_id = host.id;
    let conn = match CloudConnection::connect(config, host).await {
        Ok(conn) => conn,
        Err(CloudError::NotAuthenticated)
        | Err(CloudError::Auth(_))
        | Err(CloudError::OAuth(crate::auth::oauth::OAuthError::RefreshTokenExpired)) => {
            return Err(CloudConnectionError::NonRetriable(
                "Authentication failed — run 'amux init' to re-authenticate".to_string(),
            ));
        }
        Err(CloudError::CloudDisabled) => {
            return Err(CloudConnectionError::NonRetriable(
                "Cloud mode disabled".to_string(),
            ));
        }
        Err(CloudError::ProtocolMismatch {
            server_version,
            client_version,
        }) => {
            return Err(CloudConnectionError::ProtocolMismatch {
                server_version,
                client_version,
            });
        }
        Err(CloudError::UpdateRequired {
            minimum_version,
            client_version,
        }) => {
            return Err(CloudConnectionError::UpdateRequired {
                minimum_version,
                client_version,
            });
        }
        Err(e) => {
            return Err(CloudConnectionError::Retriable {
                msg: format!("Connection failed: {}", e),
                reset_backoff: false,
            });
        }
    };

    // Handshake succeeded: clear any stale update-required marker.
    crate::update::clear_update_required(&config.state_path);
    let remote_routing_role = conn.routing_role();
    let peer_host = match conn.host().cloned() {
        Some(host) => {
            if let Err(message) = validate_remote_host(&host) {
                return Err(CloudConnectionError::Retriable {
                    msg: format!("accepted cloud host identity is invalid: {message}"),
                    reset_backoff: false,
                });
            }
            if host.id == local_host_id {
                return Err(CloudConnectionError::Retriable {
                    msg: "accepted cloud host_id matched local host_id".to_string(),
                    reset_backoff: false,
                });
            }
            Some(host)
        }
        None => {
            return Err(CloudConnectionError::Retriable {
                msg: "accepted cloud connection omitted host identity".to_string(),
                reset_backoff: false,
            });
        }
    };

    let heartbeat = conn.idle_timeout_secs().map(|secs| HeartbeatSetup {
        role: HeartbeatRole::Dialer,
        idle_timeout: Duration::from_secs(secs.into()),
    });
    let (transport, token_refresh) = conn.into_parts();
    let link = token_refresh.link.clone();

    let (route_handle, outgoing_rx, initial_messages) = {
        let mut us = user_state.write().await;
        let (route_handle, outgoing_rx) =
            us.try_reserve_link(link.clone())
                .map_err(|_| CloudConnectionError::Retriable {
                    msg: format!("cloud assigned link `{link}` is already connected"),
                    reset_backoff: false,
                })?;
        us.mark_peer_link(link.clone());
        if remote_routing_role.is_direct_host() {
            let host = peer_host
                .clone()
                .expect("host routing role requires host identity");
            let change = us.apply_direct_peer_host_up(&link, host);
            for event in &change.events {
                super::broadcast_topology_event(&mut us, event, Some(&link));
            }
        }
        let initial_messages: Vec<Message> = remote_routing_role
            .serves_routing_events()
            .then(|| {
                Message::Peer(PeerFrame {
                    call_id: crate::protocol::CallId::from(Uuid::new_v4()),
                    body: FrameBody::Request(RequestFrame {
                        method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                        payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                    }),
                })
            })
            .into_iter()
            .collect();
        (route_handle, outgoing_rx, initial_messages)
    };
    let rpc = user_state
        .read()
        .await
        .rpc_for_link(&link)
        .expect("reserved cloud route should have RPC state");
    for message in &initial_messages {
        if let Message::Peer(PeerFrame { call_id, .. }) = message {
            rpc.register_peer_stream_outbound(RpcPeerStreamOutboundStart {
                call_id: call_id.clone(),
                link: link.clone(),
                method: method::ROUTING_SUBSCRIBE_EVENTS,
            })
            .expect("fresh peer routing call id should not collide");
        }
    }
    let conn_span = tracing::info_span!(
        "connection",
        link = %link,
        transport = "cloud",
        user_id = %LOCAL_USER_ID,
        heartbeat_role = heartbeat.map(|h| h.role.as_str()).unwrap_or("disabled"),
        local_routing_role = RoutingRole::Host.as_str(),
        remote_routing_role = remote_routing_role.as_str(),
    );
    tracing::info!(parent: &conn_span, "cloud route established");

    let rpc = user_state
        .read()
        .await
        .rpc_for_link(&link)
        .expect("reserved cloud route should have RPC state");
    let ctx = ConnectionContext {
        state,
        rpc,
        user_state,
        user_id: LOCAL_USER_ID,
        event_tx,
        link: link.clone(),
        is_local: false,
        heartbeat,
        routing_role: RoutingRole::Host,
    };

    let connected_at = std::time::Instant::now();
    let result = run_connection(RunConnection {
        transport,
        outgoing_rx,
        initial_messages,
        response_tx: route_handle.sender(),
        close_rx: route_handle.close_receiver(),
        ctx,
        token_refresh: Some(token_refresh),
        span: conn_span,
    })
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(ConnectionError::InvalidCredentials) => Err(CloudConnectionError::NonRetriable(
            "Invalid credentials — run 'amux init' to re-authenticate".to_string(),
        )),
        Err(ConnectionError::ProtocolMismatch {
            server_version,
            client_version,
        }) => Err(CloudConnectionError::ProtocolMismatch {
            server_version,
            client_version,
        }),
        Err(ConnectionError::UpdateRequired {
            minimum_version,
            client_version,
        }) => Err(CloudConnectionError::UpdateRequired {
            minimum_version,
            client_version,
        }),
        Err(e) => Err(CloudConnectionError::Retriable {
            msg: e.to_string(),
            reset_backoff: should_reset_backoff_after_connection(connected_at.elapsed()),
        }),
    }
}

fn next_backoff(backoff: Duration) -> Duration {
    std::cmp::min(backoff * 2, MAX_BACKOFF)
}

fn jittered_backoff(base_backoff: Duration) -> Duration {
    jittered_backoff_with_samples(base_backoff, random_unit_interval(), random_unit_interval())
}

fn jittered_backoff_with_samples(
    base_backoff: Duration,
    relative_sample: f64,
    absolute_sample: f64,
) -> Duration {
    debug_assert!((0.0..=1.0).contains(&relative_sample));
    debug_assert!((0.0..=1.0).contains(&absolute_sample));

    let base_secs = base_backoff.as_secs_f64();
    let relative_offset = base_secs * RELATIVE_JITTER_RATIO * ((relative_sample * 2.0) - 1.0);
    let absolute_offset = ABSOLUTE_JITTER_MAX.as_secs_f64() * absolute_sample;
    Duration::from_secs_f64((base_secs + relative_offset + absolute_offset).max(0.0))
}

fn random_unit_interval() -> f64 {
    let uuid = Uuid::new_v4();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&uuid.as_bytes()[..8]);
    let sample = u64::from_le_bytes(bytes);
    sample as f64 / u64::MAX as f64
}

fn should_reset_backoff_after_connection(connection_uptime: Duration) -> bool {
    connection_uptime >= BACKOFF_RESET_AFTER_ESTABLISHED
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ABSOLUTE_JITTER_MAX, BACKOFF_RESET_AFTER_ESTABLISHED, INITIAL_BACKOFF, MAX_BACKOFF,
        jittered_backoff_with_samples, next_backoff, should_reset_backoff_after_connection,
    };

    #[test]
    fn jittered_backoff_applies_relative_and_absolute_jitter() {
        let base = Duration::from_secs(10);

        let min = jittered_backoff_with_samples(base, 0.0, 0.0);
        let mid = jittered_backoff_with_samples(base, 0.5, 0.5);
        let max = jittered_backoff_with_samples(base, 1.0, 1.0);

        assert_eq!(min, Duration::from_millis(7500));
        assert_eq!(mid, base + ABSOLUTE_JITTER_MAX / 2);
        assert_eq!(
            max,
            Duration::from_secs(10) + Duration::from_millis(2500) + ABSOLUTE_JITTER_MAX
        );
    }

    #[test]
    fn jittered_backoff_keeps_small_backoff_positive() {
        let delay = jittered_backoff_with_samples(INITIAL_BACKOFF, 0.0, 0.0);
        assert_eq!(delay, Duration::from_millis(750));
    }

    #[test]
    fn next_backoff_doubles_until_capped() {
        assert_eq!(next_backoff(INITIAL_BACKOFF), Duration::from_secs(2));
        assert_eq!(next_backoff(Duration::from_secs(150)), MAX_BACKOFF);
        assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF);
    }

    #[test]
    fn short_lived_connection_does_not_reset_backoff() {
        assert!(!should_reset_backoff_after_connection(
            BACKOFF_RESET_AFTER_ESTABLISHED - Duration::from_secs(1)
        ));
    }

    #[test]
    fn stable_connection_resets_backoff() {
        assert!(should_reset_backoff_after_connection(
            BACKOFF_RESET_AFTER_ESTABLISHED
        ));
        assert!(should_reset_backoff_after_connection(
            BACKOFF_RESET_AFTER_ESTABLISHED + Duration::from_secs(1)
        ));
    }
}
