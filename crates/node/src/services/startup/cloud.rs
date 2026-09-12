//! Cloud relay connection with automatic reconnection.
//!
//! Manages the outbound TLS connection from a local server to a cloud relay.
//! Handles exponential backoff on retriable errors and stops on auth failures.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::FutureExt;
use model::ProtocolError;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;
use uuid::Uuid;
use wire::protocol_error_from_status_details;

use crate::audit;
use crate::auth::CredentialProvider;
use crate::auth::cloud::{
    CloudError, CloudRoutingConnectionDetails, fetch_routing_connection_details,
};
use crate::config::Config;
use crate::link::{CarrierKind, LinkCarrier, MuxCarrier, MuxRole, QuicCarrier};
use crate::profile::status::{Observed, RelayCarrier, RuntimeStatus};
use crate::routing::{
    Host, LinkConnectorAuth, LinkConnectorCtx, LinkConnectorRefreshReceiver,
    LinkConnectorRefreshRequest, LinkConnectorToken, LinkConnectorTokenRefresher,
    spawn_connector_with_auth_establishment_and_shutdown,
};
use crate::transport::tls_connect_stream;
use crate::user_state::ServerState;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const RELATIVE_JITTER_RATIO: f64 = 0.25;
const ABSOLUTE_JITTER_MAX: Duration = Duration::from_secs(5);
const BACKOFF_RESET_AFTER_ESTABLISHED: Duration = Duration::from_secs(30);
const CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
pub const FREE_TIER_REFRESH_INTERVAL: Duration = Duration::from_secs(180);
pub const UDP_BLOCKED_MEMORY: Duration = Duration::from_secs(3600);
pub(crate) const TCP_FALLBACK_DELAY: Duration = Duration::from_millis(300);

pub struct UdpBlockedMemory {
    duration: Duration,
    blocked_at: Mutex<HashMap<String, tokio::time::Instant>>,
}

pub struct CloudTransport {
    quic_endpoint: quinn::Endpoint,
    udp_blocked: Arc<UdpBlockedMemory>,
    tcp_override: Option<std::net::SocketAddr>,
    free_refresh_interval: Option<Duration>,
}

impl CloudTransport {
    pub fn new(
        quic_endpoint: quinn::Endpoint,
        udp_blocked: Arc<UdpBlockedMemory>,
        tcp_override: Option<std::net::SocketAddr>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        Self {
            quic_endpoint,
            udp_blocked,
            tcp_override,
            free_refresh_interval,
        }
    }
}

impl UdpBlockedMemory {
    pub(crate) fn new(duration: Duration) -> Self {
        Self {
            duration,
            blocked_at: Mutex::new(HashMap::new()),
        }
    }

    fn holds(&self, host: &str) -> bool {
        let mut blocked_at = self
            .blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned");
        let now = tokio::time::Instant::now();
        blocked_at.retain(|_, recorded| now.duration_since(*recorded) < self.duration);
        blocked_at.contains_key(host)
    }

    fn record(&self, host: &str) {
        self.blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned")
            .insert(host.to_string(), tokio::time::Instant::now());
    }

    fn clear(&self, host: &str) {
        self.blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned")
            .remove(host);
    }
}

#[allow(dead_code)]
pub struct CloudLink {
    stop_tx: watch::Sender<bool>,
    refresh_tx: Option<mpsc::Sender<LinkConnectorRefreshRequest>>,
    status: RuntimeStatus,
    task: JoinHandle<()>,
}

struct CloudConnectionContext {
    config: Config,
    state: Arc<RwLock<ServerState>>,
    connector: LinkConnectorCtx,
    status: RuntimeStatus,
    refresh_rx: LinkConnectorRefreshReceiver,
    transport: CloudTransport,
}

#[derive(Clone)]
pub enum TestCloudTransport {
    Auto {
        client_config: quinn::ClientConfig,
        server_name: String,
        quic_addr: std::net::SocketAddr,
    },
    Quic {
        client_config: quinn::ClientConfig,
        server_name: String,
        quic_addr: std::net::SocketAddr,
    },
    Tcp,
}

impl CloudLink {
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(crate) fn request_stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    pub(crate) async fn stop(self) {
        self.request_stop();
        let _ = self.task.await;
    }

    pub async fn refresh_entitlement(&self) -> Result<crate::Tier, CloudError> {
        let refresh_tx = self.refresh_tx.as_ref().ok_or_else(|| {
            CloudError::Connection("cloud link does not support token refresh".into())
        })?;
        let (response, response_rx) = oneshot::channel();
        refresh_tx
            .send(LinkConnectorRefreshRequest { response })
            .await
            .map_err(|_| CloudError::Connection("cloud link is not connected".into()))?;
        let tier = response_rx
            .await
            .map_err(|_| CloudError::Connection("cloud link closed during refresh".into()))?
            .map_err(cloud_error_from_refresh_status)?;
        let carrier = match &*self.status.subscribe().borrow() {
            Observed::Connected { carrier, .. } => *carrier,
            _ => {
                return Err(CloudError::Connection(
                    "cloud link refreshed before it was connected".into(),
                ));
            }
        };
        self.status.report(Observed::Connected { tier, carrier });
        Ok(tier)
    }

    pub(crate) fn testnet_with_auth(
        connector_ctx: LinkConnectorCtx,
        address: std::net::SocketAddr,
        auth: LinkConnectorAuth,
        status: RuntimeStatus,
        quic_endpoint: quinn::Endpoint,
        transport: TestCloudTransport,
        udp_blocked: Arc<UdpBlockedMemory>,
    ) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let tier = auth.tier();
        let refresh_status = status.clone();
        let (refresh_tx, refresh_rx) = mpsc::channel(1);
        status.report(Observed::Connecting);
        let task_status = status.clone();
        let task_stop_tx = stop_tx.clone();
        let task = tokio::spawn(async move {
            let selected =
                dial_test_cloud_carrier(quic_endpoint, address, transport, udp_blocked).await;
            let (carrier, relay_carrier) = match selected {
                Ok(selected) => selected,
                Err(error) => {
                    tracing::warn!(%error, "test cloud connection failed");
                    task_status.report(Observed::Retrying);
                    return;
                }
            };
            let auth = auth.with_refresh_observer(move |tier| {
                refresh_status.report(Observed::Connected {
                    tier,
                    carrier: relay_carrier,
                });
            });
            let (connector_task, established_rx) =
                spawn_connector_with_auth_establishment_and_shutdown(
                    connector_ctx,
                    carrier,
                    auth,
                    stop_rx.clone(),
                    Some(Arc::new(tokio::sync::Mutex::new(refresh_rx))),
                );
            observe_fixture_connector(
                connector_task,
                established_rx,
                task_stop_tx,
                stop_rx,
                task_status,
                tier,
                relay_carrier,
            )
            .await;
        });
        Self {
            stop_tx,
            refresh_tx: Some(refresh_tx),
            status,
            task,
        }
    }
}

async fn observe_fixture_connector(
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    established_rx: oneshot::Receiver<Result<Host, tonic::Status>>,
    stop_tx: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
    status: RuntimeStatus,
    tier: crate::Tier,
    carrier: RelayCarrier,
) {
    let connected_at = std::time::Instant::now();
    let established = await_cloud_establishment(
        &status,
        established_rx,
        connected_at,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        tier,
        carrier,
    )
    .await;
    let stopped = *stop_rx.borrow();
    let establishment_failed = established.is_err();
    let result = match established {
        Ok(()) => match connector_task.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                Err(
                    cloud_connection_error_from_status(&status, error, connected_at.elapsed())
                        .await,
                )
            }
            Err(error) => Err(CloudConnectionError::Retriable {
                msg: error.to_string(),
                reset_backoff: false,
            }),
        },
        Err(error) => {
            stop_tx.send_replace(true);
            let _ = connector_task.await;
            Err(error)
        }
    };
    if stopped || (!establishment_failed && *stop_rx.borrow()) {
        return;
    }
    match result {
        Err(CloudConnectionError::NonRetriable(_)) => {}
        _ => status.report(Observed::Retrying),
    }
}

pub fn establish_cloud_link(
    config: Config,
    state: Arc<RwLock<ServerState>>,
    connector_ctx: LinkConnectorCtx,
    status: RuntimeStatus,
    transport: CloudTransport,
) -> CloudLink {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    let refresh_rx = Arc::new(tokio::sync::Mutex::new(refresh_rx));
    let task_status = status.clone();
    let cloud_span = tracing::info_span!("cloud", url = %config.cloud_url);
    let connection = CloudConnectionContext {
        config,
        state,
        connector: connector_ctx,
        status: task_status,
        refresh_rx,
        transport,
    };
    let task = tokio::spawn(
        async move {

            let mut backoff = INITIAL_BACKOFF;

            loop {
                if *stop_rx.borrow() {
                    return;
                }
                connection.status.report(Observed::Connecting);
                tracing::info!("attempting cloud routing connection");
                match run_cloud_connection(&connection, stop_rx.clone()).await
                {
                    Ok(()) => {
                        tracing::info!("cloud routing connection closed cleanly");
                        backoff = INITIAL_BACKOFF;
                    }
                    Err(CloudConnectionError::NonRetriable(msg)) => {
                        tracing::error!(error = %msg, "cloud non-retriable error, stopping");
                        return;
                    }
                    Err(CloudConnectionError::Retriable { msg, reset_backoff }) => {
                        if reset_backoff {
                            backoff = INITIAL_BACKOFF;
                        }
                        tracing::warn!(error = %msg, "cloud routing connection error, will retry");
                    }
                }

                if *stop_rx.borrow() {
                    return;
                }
                connection.status.report(Observed::Retrying);

                let retry_delay = jittered_backoff(backoff);
                tracing::info!(base_backoff = ?backoff, retry_delay = ?retry_delay, "reconnecting to cloud");
                if sleep_or_stop(retry_delay, &mut stop_rx).await {
                    return;
                }
                backoff = next_backoff(backoff);
            }
        }
        .instrument(cloud_span),
    );
    CloudLink {
        stop_tx,
        refresh_tx: Some(refresh_tx),
        status,
        task,
    }
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable { msg: String, reset_backoff: bool },
    /// Error that should stop reconnection attempts.
    NonRetriable(String),
}

fn cloud_connection_error_from_fetch(
    error: CloudError,
    status: &RuntimeStatus,
) -> CloudConnectionError {
    match error {
        CloudError::NotAuthenticated | CloudError::Auth(_) => {
            status.report(Observed::AuthenticationRequired);
            CloudConnectionError::NonRetriable(
                "Authentication failed — run 'amux init' to re-authenticate".to_string(),
            )
        }
        error @ CloudError::Rejected(_) => {
            status.report(Observed::AuthenticationRequired);
            CloudConnectionError::NonRetriable(error.to_string())
        }
        error @ CloudError::Connection(_) => CloudConnectionError::Retriable {
            msg: format!("Connection failed: {error}"),
            reset_backoff: false,
        },
    }
}

async fn run_cloud_connection(
    ctx: &CloudConnectionContext,
    mut stop_rx: watch::Receiver<bool>,
) -> std::result::Result<(), CloudConnectionError> {
    let prepared = tokio::select! {
        biased;
        _ = wait_for_stop(&mut stop_rx) => return Ok(()),
        prepared = prepare_cloud_connection(&ctx.config, &ctx.state, &ctx.status) => prepared,
    };
    let (credentials, details) = prepared?;

    run_cloud_connection_with_details(ctx, credentials, details, stop_rx).await
}

async fn prepare_cloud_connection(
    config: &Config,
    state: &Arc<RwLock<ServerState>>,
    status: &RuntimeStatus,
) -> std::result::Result<
    (Arc<dyn CredentialProvider>, CloudRoutingConnectionDetails),
    CloudConnectionError,
> {
    let credentials = {
        let state = state.read().await;
        state.credentials.clone().ok_or_else(|| {
            status.report(Observed::AuthenticationRequired);
            CloudConnectionError::NonRetriable(
                "Authentication failed — run 'amux init' to authenticate".to_string(),
            )
        })?
    };

    let details = match fetch_routing_connection_details(config, credentials.as_ref()).await {
        Ok(details) => details,
        Err(error) => {
            if matches!(error, CloudError::NotAuthenticated | CloudError::Auth(_)) {
                audit::auth_jwt_failure("cloud routing credentials were rejected");
            }
            return Err(cloud_connection_error_from_fetch(error, status));
        }
    };

    Ok((credentials, details))
}

type CloudDialResult = Result<Arc<dyn LinkCarrier>, String>;

async fn select_cloud_carrier<Q, T>(
    host: &str,
    udp_blocked: Arc<UdpBlockedMemory>,
    fallback_delay: Duration,
    quic: Q,
    tcp: T,
) -> Result<(Arc<dyn LinkCarrier>, RelayCarrier), String>
where
    Q: Future<Output = CloudDialResult> + Send + 'static,
    T: Future<Output = CloudDialResult> + Send,
{
    if udp_blocked.holds(host) {
        tracing::debug!(host, "remembered UDP-blocked cloud host; dialing TCP only");
        return tcp.await.map(|carrier| (carrier, RelayCarrier::Tcp));
    }

    let mut quic = Box::pin(quic);
    let tcp = async move {
        tokio::time::sleep(fallback_delay).await;
        tcp.await
    };
    tokio::pin!(tcp);

    enum First {
        Quic(CloudDialResult),
        Tcp(CloudDialResult),
    }
    let first = tokio::select! {
        biased;
        result = quic.as_mut() => First::Quic(result),
        result = &mut tcp => First::Tcp(result),
    };

    match first {
        First::Quic(Ok(carrier)) => {
            close_ready_loser(tcp.as_mut().now_or_never());
            udp_blocked.clear(host);
            Ok((carrier, RelayCarrier::Quic))
        }
        First::Tcp(Ok(carrier)) => {
            match quic.as_mut().now_or_never() {
                Some(Ok(late_quic)) => {
                    late_quic.close(wire::pb::LinkCloseReason::UserShutdown);
                    udp_blocked.clear(host);
                }
                Some(Err(error)) => {
                    tracing::debug!(%error, "cloud QUIC dial failed after all candidates");
                    udp_blocked.record(host);
                }
                None => {
                    let host = host.to_string();
                    tokio::spawn(async move {
                        match quic.await {
                            Ok(late_quic) => {
                                late_quic.close(wire::pb::LinkCloseReason::UserShutdown);
                                udp_blocked.clear(&host);
                            }
                            Err(error) => {
                                tracing::debug!(%error, %host, "cloud QUIC probe failed after all candidates");
                                udp_blocked.record(&host);
                            }
                        }
                    });
                }
            }
            Ok((carrier, RelayCarrier::Tcp))
        }
        First::Quic(Err(quic_error)) => {
            tracing::debug!(error = %quic_error, "cloud QUIC dial failed; waiting for TCP");
            match tcp.await {
                Ok(carrier) => {
                    udp_blocked.record(host);
                    Ok((carrier, RelayCarrier::Tcp))
                }
                Err(tcp_error) => Err(format!(
                    "QUIC failed: {quic_error}; TCP failed: {tcp_error}"
                )),
            }
        }
        First::Tcp(Err(tcp_error)) => {
            tracing::debug!(error = %tcp_error, "cloud TCP dial failed; waiting for QUIC");
            match quic.await {
                Ok(carrier) => {
                    udp_blocked.clear(host);
                    Ok((carrier, RelayCarrier::Quic))
                }
                Err(quic_error) => Err(format!(
                    "QUIC failed: {quic_error}; TCP failed: {tcp_error}"
                )),
            }
        }
    }
}

fn close_ready_loser(result: Option<CloudDialResult>) {
    if let Some(Ok(carrier)) = result {
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
    }
}

async fn dial_test_cloud_carrier(
    quic_endpoint: quinn::Endpoint,
    tcp_addr: std::net::SocketAddr,
    transport: TestCloudTransport,
    udp_blocked: Arc<UdpBlockedMemory>,
) -> Result<(Arc<dyn LinkCarrier>, RelayCarrier), String> {
    let tcp = async move {
        TcpStream::connect(tcp_addr)
            .await
            .map(|stream| {
                Arc::new(MuxCarrier::new(
                    stream,
                    MuxRole::Connector,
                    CarrierKind::RelayTcp,
                )) as Arc<dyn LinkCarrier>
            })
            .map_err(|error| error.to_string())
    };

    let (client_config, server_name, quic_addr, automatic) = match transport {
        TestCloudTransport::Auto {
            client_config,
            server_name,
            quic_addr,
        } => (client_config, server_name, quic_addr, true),
        TestCloudTransport::Quic {
            client_config,
            server_name,
            quic_addr,
        } => (client_config, server_name, quic_addr, false),
        TestCloudTransport::Tcp => return tcp.await.map(|carrier| (carrier, RelayCarrier::Tcp)),
    };
    let quic = async move {
        QuicCarrier::connect_relay_candidates_with_config(
            &quic_endpoint,
            [quic_addr],
            &server_name,
            quic_addr.port(),
            client_config,
        )
        .await
        .map(|carrier| Arc::new(carrier) as Arc<dyn LinkCarrier>)
        .map_err(|error| error.to_string())
    };
    if automatic {
        select_cloud_carrier("testnet-relay", udp_blocked, TCP_FALLBACK_DELAY, quic, tcp).await
    } else {
        quic.await.map(|carrier| (carrier, RelayCarrier::Quic))
    }
}

async fn run_cloud_connection_with_details(
    ctx: &CloudConnectionContext,
    credentials: Arc<dyn CredentialProvider>,
    details: CloudRoutingConnectionDetails,
    stop_rx: watch::Receiver<bool>,
) -> std::result::Result<(), CloudConnectionError> {
    tracing::info!(host = %details.host, port = details.port, "connecting to cloud routing");
    let quic_endpoint = ctx.transport.quic_endpoint.clone();
    let quic_host = details.host.clone();
    let quic_port = details.port;
    let quic = async move {
        #[cfg(debug_assertions)]
        if std::env::var_os("AMUX_TEST_RELAY_UDP_BLOCKED").is_some() {
            return Err("relay UDP blocked by test fixture".to_string());
        }
        QuicCarrier::connect_relay(&quic_endpoint, &quic_host, quic_port)
            .await
            .map(|carrier| Arc::new(carrier) as Arc<dyn LinkCarrier>)
            .map_err(|error| error.to_string())
    };
    let tcp_host = details.host.clone();
    let tcp_port = details.port;
    let fixture_transport = ctx.transport.tcp_override;
    let tcp = async move {
        let stream = cloud_routing_stream(&tcp_host, tcp_port, fixture_transport).await;
        stream
            .map(|stream| {
                Arc::new(MuxCarrier::new(
                    stream,
                    MuxRole::Connector,
                    CarrierKind::RelayTcp,
                )) as Arc<dyn LinkCarrier>
            })
            .map_err(|error| error.to_string())
    };
    let (carrier, relay_carrier) = tokio::time::timeout(
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        select_cloud_carrier(
            &details.host,
            ctx.transport.udp_blocked.clone(),
            TCP_FALLBACK_DELAY,
            quic,
            tcp,
        ),
    )
    .await
    .map_err(|_| CloudConnectionError::Retriable {
        msg: "cloud routing transport dial timed out".to_string(),
        reset_backoff: false,
    })?
    .map_err(|error| CloudConnectionError::Retriable {
        msg: format!("Connection failed: {error}"),
        reset_backoff: false,
    })?;
    let connected_at = std::time::Instant::now();
    let tier = details.tier;
    let refresh_status = ctx.status.clone();
    let connector_auth = LinkConnectorAuth::with_free_refresh_interval(
        LinkConnectorToken {
            token: details.token,
            expires_at: SystemTime::from(details.expires_at),
            tier,
        },
        Arc::new(CloudLinkTokenRefresher {
            config: ctx.config.clone(),
            credentials,
            current_host: details.host,
            current_port: details.port,
        }),
        Some(
            ctx.transport
                .free_refresh_interval
                .unwrap_or(FREE_TIER_REFRESH_INTERVAL),
        ),
    )
    .with_refresh_observer(move |tier| {
        refresh_status.report(Observed::Connected {
            tier,
            carrier: relay_carrier,
        });
    });
    let (connector_task, established_rx) = spawn_connector_with_auth_establishment_and_shutdown(
        ctx.connector.clone(),
        carrier,
        connector_auth,
        stop_rx,
        Some(ctx.refresh_rx.clone()),
    );
    let _abort_connector_on_drop = AbortTaskOnDrop(connector_task.abort_handle());
    await_cloud_establishment(
        &ctx.status,
        established_rx,
        connected_at,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        tier,
        relay_carrier,
    )
    .await?;

    let result = connector_task
        .await
        .map_err(|error| CloudConnectionError::Retriable {
            msg: format!("cloud routing task failed: {error}"),
            reset_backoff: should_reset_backoff_after_connection(connected_at.elapsed()),
        })?;

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            Err(
                cloud_connection_error_from_status(&ctx.status, error, connected_at.elapsed())
                    .await,
            )
        }
    }
}

async fn sleep_or_stop(duration: Duration, stop_rx: &mut watch::Receiver<bool>) -> bool {
    if *stop_rx.borrow() {
        return true;
    }

    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    tokio::select! {
        biased;
        _ = wait_for_stop(stop_rx) => true,
        _ = &mut sleep => false,
    }
}

async fn wait_for_stop(stop_rx: &mut watch::Receiver<bool>) {
    loop {
        if *stop_rx.borrow() {
            return;
        }
        match stop_rx.changed().await {
            Ok(()) => {}
            Err(_) => {
                // Losing the stop handle means this task can no longer be
                // stopped cooperatively; it is not itself a stop request.
                std::future::pending::<()>().await;
            }
        }
    }
}

async fn await_cloud_establishment(
    observed: &RuntimeStatus,
    established_rx: oneshot::Receiver<Result<Host, tonic::Status>>,
    connected_at: std::time::Instant,
    timeout: Duration,
    tier: crate::Tier,
    carrier: RelayCarrier,
) -> std::result::Result<(), CloudConnectionError> {
    match tokio::time::timeout(timeout, established_rx).await {
        Ok(Ok(Ok(_))) => {
            observed.report(Observed::Connected { tier, carrier });
            Ok(())
        }
        Ok(Ok(Err(status))) => {
            Err(cloud_connection_error_from_status(observed, status, connected_at.elapsed()).await)
        }
        Ok(Err(_)) => Ok(()),
        Err(_) => Err(CloudConnectionError::Retriable {
            msg: "cloud routing handshake timed out".to_string(),
            reset_backoff: false,
        }),
    }
}

async fn cloud_connection_error_from_status(
    observed: &RuntimeStatus,
    status: tonic::Status,
    connection_uptime: Duration,
) -> CloudConnectionError {
    if let Some(minimum_version) = update_required_from_status(&status) {
        observed.report(Observed::UpdateRequired {
            minimum_version: Some(minimum_version),
        });
        return CloudConnectionError::NonRetriable(status.to_string());
    }
    if is_update_required_status(&status) {
        observed.report(Observed::UpdateRequired {
            minimum_version: None,
        });
        return CloudConnectionError::NonRetriable(status.to_string());
    }
    if status.code() == tonic::Code::Unauthenticated {
        observed.report(Observed::AuthenticationRequired);
        audit::auth_jwt_failure(&status);
        return CloudConnectionError::NonRetriable(
            "Invalid credentials — run 'amux init' to re-authenticate".to_string(),
        );
    }
    CloudConnectionError::Retriable {
        msg: status.to_string(),
        reset_backoff: should_reset_backoff_after_connection(connection_uptime),
    }
}

fn is_update_required_status(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::FailedPrecondition
        && status.message().contains("amux update required")
}

fn update_required_from_status(status: &tonic::Status) -> Option<String> {
    match protocol_error_from_status_details(status)? {
        ProtocolError::UpdateRequired {
            minimum_version, ..
        } => Some(minimum_version),
        _ => None,
    }
}

trait CloudIo: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T> CloudIo for T where T: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

type BoxedCloudIo = Box<dyn CloudIo>;

async fn cloud_routing_stream(
    host: &str,
    port: u16,
    transport: Option<std::net::SocketAddr>,
) -> crate::transport::Result<BoxedCloudIo> {
    if let Some(address) = transport {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        crate::transport::configure_relay_tcp_keepalive(&stream);
        return Ok(Box::new(stream));
    }
    Ok(Box::new(tls_connect_stream(host, port).await?))
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct CloudLinkTokenRefresher {
    config: Config,
    credentials: Arc<dyn CredentialProvider>,
    current_host: String,
    current_port: u16,
}

fn cloud_token_refresh_status(error: CloudError) -> tonic::Status {
    match error {
        CloudError::NotAuthenticated | CloudError::Auth(_) => {
            tonic::Status::unauthenticated("invalid cloud credentials")
        }
        CloudError::Rejected(message) => tonic::Status::permission_denied(message),
        CloudError::Connection(message) => tonic::Status::unavailable(message),
    }
}

fn cloud_error_from_refresh_status(status: tonic::Status) -> CloudError {
    match status.code() {
        tonic::Code::Unauthenticated => CloudError::Auth(status.message().to_string()),
        tonic::Code::PermissionDenied => CloudError::Rejected(status.message().to_string()),
        _ => CloudError::Connection(status.message().to_string()),
    }
}

#[tonic::async_trait]
impl LinkConnectorTokenRefresher for CloudLinkTokenRefresher {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        let details = fetch_routing_connection_details(&self.config, self.credentials.as_ref())
            .await
            .map_err(cloud_token_refresh_status)?;

        if details.host != self.current_host || details.port != self.current_port {
            return Err(tonic::Status::unavailable(
                "cloud routing endpoint changed during reauth",
            ));
        }

        Ok(LinkConnectorToken {
            token: details.token,
            expires_at: SystemTime::from(details.expires_at),
            tier: details.tier,
        })
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use model::ProtocolError;
    use tokio::io::AsyncReadExt;
    use tokio::sync::{RwLock, oneshot};
    use uuid::Uuid;
    use wire::protocol_status;

    use super::{
        ABSOLUTE_JITTER_MAX, BACKOFF_RESET_AFTER_ESTABLISHED, FREE_TIER_REFRESH_INTERVAL,
        INITIAL_BACKOFF, MAX_BACKOFF, TCP_FALLBACK_DELAY, UDP_BLOCKED_MEMORY, UdpBlockedMemory,
        await_cloud_establishment, cloud_connection_error_from_fetch,
        cloud_connection_error_from_status, establish_cloud_link, jittered_backoff_with_samples,
        next_backoff, select_cloud_carrier, should_reset_backoff_after_connection, sleep_or_stop,
    };
    use crate::auth::{AccessToken, AuthError, CredentialProvider};
    use crate::config::Config;
    use crate::link::{CarrierKind, LinkCarrier, MuxCarrier, MuxRole};
    use crate::profile::status::{Observed, RelayCarrier, RuntimeStatus};
    use crate::routing::{Capabilities, Host, LinkConnectorCtx, LinkRegistry, RoutingCore};
    use crate::update::{UpdateReporter, UpdateStatus};
    use crate::user_state::ServerState;

    #[derive(Default)]
    struct CapturingUpdateReporter {
        statuses: Mutex<Vec<UpdateStatus>>,
    }

    impl UpdateReporter for CapturingUpdateReporter {
        fn report(&self, status: UpdateStatus) {
            self.statuses.lock().unwrap().push(status);
        }
    }

    struct StaticCredentials;

    #[async_trait::async_trait]
    impl CredentialProvider for StaticCredentials {
        async fn access_token(&self) -> Result<AccessToken, AuthError> {
            Ok(AccessToken {
                bearer: "test-token".to_string(),
                expires_at: None,
            })
        }

        fn invalidate(&self, _token: &AccessToken) {}
    }

    #[test]
    fn fetch_error_classification_controls_retries() {
        assert!(matches!(
            cloud_connection_error_from_fetch(
                crate::auth::cloud::CloudError::Rejected("403 Forbidden".to_string()),
                &RuntimeStatus::new(None)
            ),
            super::CloudConnectionError::NonRetriable(_)
        ));
        assert!(matches!(
            cloud_connection_error_from_fetch(
                crate::auth::cloud::CloudError::Connection("temporary".to_string()),
                &RuntimeStatus::new(None)
            ),
            super::CloudConnectionError::Retriable { .. }
        ));
    }

    #[test]
    fn free_tier_refresh_uses_the_product_cadence() {
        assert_eq!(FREE_TIER_REFRESH_INTERVAL, Duration::from_secs(180));
        assert_eq!(TCP_FALLBACK_DELAY, Duration::from_millis(300));
        assert_eq!(UDP_BLOCKED_MEMORY, Duration::from_secs(3600));
    }

    #[tokio::test(start_paused = true)]
    async fn udp_blocked_memory_expires_and_a_new_process_memory_starts_empty() {
        let memory = UdpBlockedMemory::new(Duration::from_secs(10));
        memory.record("relay.test");
        assert!(memory.holds("relay.test"));

        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(!memory.holds("relay.test"));
        assert!(!UdpBlockedMemory::new(Duration::from_secs(10)).holds("relay.test"));
    }

    #[tokio::test]
    async fn simultaneous_cloud_dials_prefer_quic() {
        let (quic_io, _quic_peer) = tokio::io::duplex(64);
        let quic: Arc<dyn LinkCarrier> = Arc::new(MuxCarrier::new(
            quic_io,
            MuxRole::Connector,
            CarrierKind::RelayQuic,
        ));
        let (tcp_io, _tcp_peer) = tokio::io::duplex(64);
        let tcp: Arc<dyn LinkCarrier> = Arc::new(MuxCarrier::new(
            tcp_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let memory = Arc::new(UdpBlockedMemory::new(Duration::from_secs(10)));
        let (winner, carrier) = select_cloud_carrier(
            "relay.test",
            memory.clone(),
            Duration::ZERO,
            std::future::ready(Ok(quic)),
            std::future::ready(Ok(tcp)),
        )
        .await
        .unwrap();

        assert_eq!(carrier, RelayCarrier::Quic);
        assert_eq!(winner.kind(), CarrierKind::RelayQuic);
        assert!(!memory.holds("relay.test"));
    }

    #[tokio::test]
    async fn tcp_winner_is_not_remembered_until_the_complete_quic_probe_fails() {
        let memory = Arc::new(UdpBlockedMemory::new(Duration::from_secs(10)));
        let (probe_tx, probe_rx) = oneshot::channel();
        let quic = async move {
            probe_rx.await.unwrap();
            Err("all QUIC candidates failed".to_string())
        };
        let (tcp_io, _tcp_peer) = tokio::io::duplex(64);
        let tcp: Arc<dyn LinkCarrier> = Arc::new(MuxCarrier::new(
            tcp_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));

        let (_, carrier) = select_cloud_carrier(
            "relay.test",
            memory.clone(),
            Duration::ZERO,
            quic,
            std::future::ready(Ok(tcp)),
        )
        .await
        .unwrap();
        assert_eq!(carrier, RelayCarrier::Tcp);
        assert!(!memory.holds("relay.test"));

        probe_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !memory.holds("relay.test") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed QUIC probe was not remembered");
    }

    #[tokio::test]
    async fn profile_runtime_update_status_reports_through_configured_reporter() {
        let reporter = Arc::new(CapturingUpdateReporter::default());
        let state = RuntimeStatus::new(Some(reporter.clone()));

        state.report(Observed::UpdateRequired {
            minimum_version: Some("0.4.0".to_string()),
        });
        assert_eq!(
            *state.subscribe().borrow(),
            Observed::UpdateRequired {
                minimum_version: Some("0.4.0".into())
            }
        );
        state.report(Observed::Connecting);
        assert_eq!(reporter.statuses.lock().unwrap().len(), 1);
        state.report(Observed::Connected {
            tier: crate::Tier::Pro,
            carrier: RelayCarrier::Tcp,
        });

        let statuses = reporter.statuses.lock().unwrap();
        assert_eq!(statuses.len(), 2);
        match &statuses[0] {
            UpdateStatus::Required(Some(minimum_version)) => {
                assert_eq!(minimum_version, "0.4.0");
            }
            other => panic!("unexpected first update status: {other:?}"),
        }
        match &statuses[1] {
            UpdateStatus::Required(None) => {}
            other => panic!("unexpected second update status: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_required_status_reports_required_update_and_stops_retrying() {
        let reporter = Arc::new(CapturingUpdateReporter::default());
        let state = RuntimeStatus::new(Some(reporter.clone()));

        let status = protocol_status(ProtocolError::UpdateRequired {
            minimum_version: "0.4.0".to_string(),
            client_version: "0.3.0".to_string(),
        });
        let error = cloud_connection_error_from_status(&state, status, Duration::ZERO).await;

        match error {
            super::CloudConnectionError::NonRetriable(message) => {
                assert!(message.contains("amux update required"));
            }
            super::CloudConnectionError::Retriable { .. } => {
                panic!("update-required status must stop reconnecting")
            }
        }
        let statuses = reporter.statuses.lock().unwrap();
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            UpdateStatus::Required(Some(minimum_version)) => {
                assert_eq!(minimum_version, "0.4.0");
            }
            other => panic!("unexpected update status: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bare_permission_denied_status_remains_retriable() {
        let state = RuntimeStatus::new(None);
        let status = tonic::Status::permission_denied("cloud request rejected");

        let error = cloud_connection_error_from_status(&state, status, Duration::ZERO).await;

        assert!(matches!(
            error,
            super::CloudConnectionError::Retriable { .. }
        ));
    }

    #[tokio::test]
    async fn cloud_establishment_wait_times_out() {
        let state = RuntimeStatus::new(None);
        let (_tx, rx) = oneshot::channel();

        let error = await_cloud_establishment(
            &state,
            rx,
            std::time::Instant::now(),
            Duration::from_millis(10),
            crate::Tier::Pro,
            RelayCarrier::Quic,
        )
        .await
        .unwrap_err();

        match error {
            super::CloudConnectionError::Retriable { msg, reset_backoff } => {
                assert!(msg.contains("timed out"));
                assert!(!reset_backoff);
            }
            super::CloudConnectionError::NonRetriable(message) => {
                panic!("timeout must be retriable, got non-retriable: {message}");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cloud_backoff_waits_after_stop_handle_is_dropped_and_honors_stop() {
        let delay = Duration::from_secs(5);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        drop(stop_tx);

        let wait = tokio::spawn(async move { sleep_or_stop(delay, &mut stop_rx).await });
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());

        tokio::time::advance(delay - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());

        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(!wait.await.unwrap());

        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let wait = tokio::spawn(async move { sleep_or_stop(delay, &mut stop_rx).await });
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());

        stop_tx.send(true).unwrap();
        assert!(wait.await.unwrap());
    }

    #[tokio::test]
    async fn connector_stop_during_connect_cancels_a_hanging_details_request() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (request_started_tx, request_started_rx) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).await.unwrap();
            assert!(read > 0);
            let _ = request_started_tx.send(());
            std::future::pending::<()>().await;
        });

        let config = Config {
            cloud_url: format!("http://{address}"),

            ..Config::default()
        };
        let host_id = Uuid::new_v4();

        let state = Arc::new(RwLock::new(ServerState::new(
            config.clone(),
            host_id,
            Some(Arc::new(StaticCredentials)),
            None,
        )));
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let connector_ctx = LinkConnectorCtx::new(
            Host {
                id: host_id,
                name: "local".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: Capabilities::default(),
                signed_in: Some(true),
            },
            routing,
            links,
        );
        let connector = establish_cloud_link(
            config,
            state,
            connector_ctx,
            RuntimeStatus::new(None),
            super::CloudTransport::new(
                quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap(),
                Arc::new(super::UdpBlockedMemory::new(super::UDP_BLOCKED_MEMORY)),
                None,
                None,
            ),
        );

        tokio::time::timeout(Duration::from_secs(1), request_started_rx)
            .await
            .expect("cloud connect-details request did not start")
            .expect("hanging server stopped before receiving the request");

        tokio::time::timeout(Duration::from_secs(1), connector.stop())
            .await
            .expect("connector stop waited for the hanging connect-details request");
        server_task.abort();
    }

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
