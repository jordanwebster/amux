//! Runtime Link establishment from persisted trust-store reachabilities.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tonic::transport::Endpoint;
use tracing::Instrument;

use crate::HostId;
use crate::connection::ConnectionManager;
use crate::identity::DeviceIdentity;
use crate::routing::{
    Host, Link, RoutingConnectorCtx, RoutingCore, generate_server_link,
    spawn_connector_to_channel_with_establishment,
};
use crate::transport::{
    channel_from_single_io, configure_tonic_endpoint_keepalive, spawn_ssh_relay,
    trusted_device_channel,
};
use crate::trust::{Reachability, SharedTrustStore};
use crate::tunnel::TunnelPool;

const DIRECT_LINK_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct ReachabilityLinkConnector {
    mode: ReachabilityLinkConnectorMode,
}

#[derive(Clone)]
enum ReachabilityLinkConnectorMode {
    Enabled(Arc<ReachabilityLinkConnectorInner>),
    #[allow(dead_code)]
    Disabled,
}

struct ReachabilityLinkConnectorInner {
    context: ReachabilityLinkContext,
    pair_time_tasks: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Clone)]
struct ReachabilityLinkContext {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    local_host: Host,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    connections: Arc<ConnectionManager>,
}

#[derive(Clone)]
struct ReachabilityLinkAttempt {
    peer: HostId,
    reachability: Reachability,
    ordinal: usize,
}

impl ReachabilityLinkConnector {
    pub(crate) fn new(
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
        local_host: Host,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
        connections: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            mode: ReachabilityLinkConnectorMode::Enabled(Arc::new(
                ReachabilityLinkConnectorInner {
                    context: ReachabilityLinkContext {
                        identity,
                        trust_store,
                        local_host,
                        routing,
                        tunnels,
                        connections,
                    },
                    pair_time_tasks: Mutex::new(Vec::new()),
                },
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            mode: ReachabilityLinkConnectorMode::Disabled,
        }
    }

    pub(crate) fn spawn_startup_links(
        &self,
        host_name: &str,
        randomise_link_name: bool,
    ) -> Vec<JoinHandle<()>> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return Vec::new();
        };
        let attempts = match snapshot_direct_attempts(&inner.context.trust_store) {
            Ok(attempts) => attempts,
            Err(()) => {
                tracing::warn!("failed to read trust store for reachability Link startup");
                return Vec::new();
            }
        };
        attempts
            .into_iter()
            .filter_map(|attempt| self.spawn_attempt(host_name, randomise_link_name, attempt))
            .collect()
    }

    pub(crate) fn spawn_pair_time_link(
        &self,
        host_name: &str,
        randomise_link_name: bool,
        peer: HostId,
        reachability: Reachability,
    ) {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return;
        };
        if matches!(reachability, Reachability::Cloud) {
            return;
        };
        let Some(task) = self.spawn_attempt(
            host_name,
            randomise_link_name,
            ReachabilityLinkAttempt {
                peer,
                reachability,
                ordinal: 0,
            },
        ) else {
            return;
        };
        inner.retain_pair_time_task(task);
    }

    fn spawn_attempt(
        &self,
        host_name: &str,
        randomise_link_name: bool,
        attempt: ReachabilityLinkAttempt,
    ) -> Option<JoinHandle<()>> {
        let ReachabilityLinkConnectorMode::Enabled(inner) = &self.mode else {
            return None;
        };
        let context = inner.context.clone();
        let proposed_link = proposed_link_name(
            host_name,
            attempt.peer,
            &attempt.reachability,
            attempt.ordinal,
            randomise_link_name,
        );
        let span = tracing::info_span!(
            "reachability_link",
            peer = %attempt.peer,
            reachability = ?attempt.reachability,
            proposed_link = %proposed_link,
        );
        Some(tokio::spawn(
            async move {
                establish_reachability_link(context, attempt, proposed_link).await;
            }
            .instrument(span),
        ))
    }
}

impl ReachabilityLinkConnectorInner {
    fn retain_pair_time_task(&self, task: JoinHandle<()>) {
        let Ok(mut tasks) = self.pair_time_tasks.lock() else {
            task.abort();
            tracing::warn!("failed to retain pair-time reachability Link task");
            return;
        };
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
}

impl Drop for ReachabilityLinkConnectorInner {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.pair_time_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

fn snapshot_direct_attempts(
    trust_store: &SharedTrustStore,
) -> Result<Vec<ReachabilityLinkAttempt>, ()> {
    let store = trust_store.read().map_err(|_| ())?;
    let mut attempts = Vec::new();
    for (peer, entry) in store.entries() {
        for (ordinal, reachability) in entry
            .reachabilities
            .iter()
            .filter(|reachability| !matches!(reachability, Reachability::Cloud))
            .cloned()
            .enumerate()
        {
            attempts.push(ReachabilityLinkAttempt {
                peer,
                reachability,
                ordinal,
            });
        }
    }
    Ok(attempts)
}

async fn establish_reachability_link(
    context: ReachabilityLinkContext,
    attempt: ReachabilityLinkAttempt,
    proposed_link: Link,
) {
    let channel = match attempt.reachability.clone() {
        Reachability::Cloud => return,
        Reachability::DirectTcp { addr } => trusted_device_channel(
            addr,
            context.identity.clone(),
            context.trust_store.clone(),
            attempt.peer,
        )
        .map_err(|error| error.to_string()),
        Reachability::Ssh { target } => spawn_ssh_relay(&target)
            .map(|io| {
                channel_from_single_io(
                    configure_tonic_endpoint_keepalive(Endpoint::from_static("http://ssh-relay")),
                    "SSH relay transport",
                    io,
                )
            })
            .map_err(|error| error.to_string()),
    };
    let channel = match channel {
        Ok(channel) => channel,
        Err(error) => {
            context
                .connections
                .record_reachability_error(attempt.peer, error.clone())
                .await;
            tracing::warn!(error = %error, "failed to prepare reachability Link transport");
            return;
        }
    };

    let connector_ctx = RoutingConnectorCtx::new(
        context.local_host.clone(),
        context.routing.clone(),
        context.tunnels.clone(),
        proposed_link,
        context.connections.route_runtime(),
    )
    .with_expected_peer(attempt.peer);
    let (connector_task, established_rx) =
        spawn_connector_to_channel_with_establishment(connector_ctx, channel);
    let _abort_connector_on_drop = AbortTaskOnDrop(connector_task.abort_handle());

    match tokio::time::timeout(DIRECT_LINK_ESTABLISHMENT_TIMEOUT, established_rx).await {
        Ok(Ok(Ok(host))) => {
            context
                .connections
                .clear_reachability_error(attempt.peer)
                .await;
            tracing::info!(peer_name = %host.name, "reachability Link established");
        }
        Ok(Ok(Err(status))) => {
            context
                .connections
                .record_reachability_error(attempt.peer, status.to_string())
                .await;
            tracing::warn!(error = %status, "reachability Link establishment failed");
            return;
        }
        Ok(Err(_)) => {
            context
                .connections
                .record_reachability_error(
                    attempt.peer,
                    "reachability Link task ended before establishment",
                )
                .await;
            tracing::warn!("reachability Link task ended before establishment");
            return;
        }
        Err(_) => {
            context
                .connections
                .record_reachability_error(
                    attempt.peer,
                    "reachability Link establishment timed out",
                )
                .await;
            tracing::warn!("reachability Link establishment timed out");
            return;
        }
    }

    match connector_task.await {
        Ok(Ok(())) => tracing::info!("reachability Link closed cleanly"),
        Ok(Err(status)) => tracing::warn!(error = %status, "reachability Link closed with error"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(error = %error, "reachability Link task panicked"),
    }
}

fn proposed_link_name(
    host_name: &str,
    peer: HostId,
    reachability: &Reachability,
    ordinal: usize,
    randomise_link_name: bool,
) -> Link {
    let kind = match reachability {
        Reachability::Cloud => "cloud",
        Reachability::DirectTcp { .. } => "tcp",
        Reachability::Ssh { .. } => "ssh",
    };
    let peer_prefix = peer
        .as_simple()
        .to_string()
        .chars()
        .take(8)
        .collect::<String>();
    generate_server_link(
        &format!("{kind}-{peer_prefix}-{ordinal}-{host_name}"),
        randomise_link_name,
    )
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, RwLock};

    use chrono::Utc;

    use super::*;
    use crate::trust::{TrustEntry, TrustStore};

    #[test]
    fn snapshot_direct_attempts_includes_direct_tcp_and_ssh_only() {
        let peer = HostId::from_u128(2);
        let addr: SocketAddr = "127.0.0.1:4321".parse().unwrap();
        let mut store = TrustStore::default();
        store.insert_for_test(
            peer,
            TrustEntry {
                pubkey: vec![7; 32],
                name: "peer".to_string(),
                paired_at: Utc::now(),
                reachabilities: vec![
                    Reachability::Cloud,
                    Reachability::DirectTcp { addr },
                    Reachability::Ssh {
                        target: "workstation".to_string(),
                    },
                ],
            },
        );
        let attempts = snapshot_direct_attempts(&Arc::new(RwLock::new(store))).unwrap();

        assert_eq!(attempts.len(), 2);
        assert!(matches!(
            attempts[0].reachability,
            Reachability::DirectTcp { addr: attempt_addr } if attempt_addr == addr
        ));
        assert!(matches!(
            &attempts[1].reachability,
            Reachability::Ssh { target } if target == "workstation"
        ));
    }

    #[test]
    fn proposed_link_name_keeps_reachability_peer_and_ordinal_prefix() {
        let link = proposed_link_name(
            "this.host.name.is.sanitized",
            HostId::from_u128(0x1234),
            &Reachability::Ssh {
                target: "workstation".to_string(),
            },
            3,
            false,
        );

        assert!(link.as_str().starts_with("ssh-00000000-3-this-host"));
    }
}
