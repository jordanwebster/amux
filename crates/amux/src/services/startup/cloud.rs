//! Cloud relay connection with automatic reconnection.
//!
//! Manages the outbound TLS connection from a local server to a cloud relay.
//! Handles exponential backoff on retriable errors and stops on auth failures.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{RwLock, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::cloud::{
    CloudError, CloudRoutingConnectionDetails, fetch_routing_connection_details,
};
use crate::config::Config;
use crate::profile::status::{Observed, RuntimeStatus};
use crate::protocol::{ProtocolError, protocol_error_from_status_details, protocol_status};
use crate::routing::{
    Host, LinkConnectorAuth, LinkConnectorCtx, LinkConnectorToken, LinkConnectorTokenRefresher,
    spawn_connector_to_channel_with_auth_establishment_and_shutdown,
};
use crate::transport::tls_channel;
use crate::user_state::ServerState;
use crate::{audit, setup};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const RELATIVE_JITTER_RATIO: f64 = 0.25;
const ABSOLUTE_JITTER_MAX: Duration = Duration::from_secs(5);
const BACKOFF_RESET_AFTER_ESTABLISHED: Duration = Duration::from_secs(30);
const CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// Entitlement can change while the daemon is idle, so it probes periodically.
/// The probe is one cheap token fetch; a short interval keeps a fresh
/// purchase on the phone from looking broken while the desktop catches up.
const SUBSCRIPTION_RECHECK_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct CloudConnector {
    stop_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl CloudConnector {
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

    #[cfg(testnet)]
    pub(crate) fn testnet_bearer(
        connector_ctx: LinkConnectorCtx,
        channel: tonic::transport::Channel,
        token: String,
        status: RuntimeStatus,
    ) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        status.report(Observed::Connecting);
        let (connector_task, established_rx) =
            crate::routing::spawn_connector_to_channel_with_bearer_token_and_shutdown(
                connector_ctx,
                channel,
                token,
                stop_rx.clone(),
            );
        let task = tokio::spawn(observe_fixture_connector(
            connector_task,
            established_rx,
            stop_tx.clone(),
            stop_rx,
            status,
        ));
        Self { stop_tx, task }
    }

    #[cfg(testnet)]
    pub(crate) fn testnet_with_auth(
        connector_ctx: LinkConnectorCtx,
        channel: tonic::transport::Channel,
        auth: LinkConnectorAuth,
        status: RuntimeStatus,
    ) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        status.report(Observed::Connecting);
        let (connector_task, established_rx) =
            spawn_connector_to_channel_with_auth_establishment_and_shutdown(
                connector_ctx,
                channel,
                auth,
                stop_rx.clone(),
            );
        let task = tokio::spawn(observe_fixture_connector(
            connector_task,
            established_rx,
            stop_tx.clone(),
            stop_rx,
            status,
        ));
        Self { stop_tx, task }
    }
}

#[cfg(testnet)]
async fn observe_fixture_connector(
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    established_rx: oneshot::Receiver<Result<Host, tonic::Status>>,
    stop_tx: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
    status: RuntimeStatus,
) {
    let connected_at = std::time::Instant::now();
    let established = await_cloud_establishment(
        &status,
        established_rx,
        connected_at,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
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
        Err(CloudConnectionError::SubscriptionRequired) => {
            status.report(Observed::SubscriptionRequired)
        }
        Err(CloudConnectionError::NonRetriable(_)) => {}
        _ => status.report(Observed::Retrying),
    }
}

pub(crate) fn establish_cloud_connection(
    config: Config,
    state: Arc<RwLock<ServerState>>,
    connector_ctx: LinkConnectorCtx,
    status: RuntimeStatus,
    #[cfg(testnet)] transport: Option<tonic::transport::Channel>,
) -> CloudConnector {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let cloud_span = tracing::info_span!("cloud", url = %config.cloud_url);
    let task = tokio::spawn(
        async move {
            if !setup::cloud_enabled(&config) {
                status.report(Observed::Local);
                tracing::info!("cloud mode not enabled");
                return;
            }

            let mut backoff = INITIAL_BACKOFF;

            loop {
                if *stop_rx.borrow() {
                    return;
                }
                status.report(Observed::Connecting);
                tracing::info!("attempting cloud routing connection");
                match run_cloud_connection(
                    &config,
                    state.clone(),
                    connector_ctx.clone(),
                    stop_rx.clone(),
                    &status,
                    #[cfg(testnet)]
                    transport.clone(),
                )
                .await
                {
                    Ok(()) => {
                        tracing::info!("cloud routing connection closed cleanly");
                        backoff = INITIAL_BACKOFF;
                    }
                    Err(CloudConnectionError::NonRetriable(msg)) => {
                        tracing::error!(error = %msg, "cloud non-retriable error, stopping");
                        return;
                    }
                    Err(CloudConnectionError::SubscriptionRequired) => {
                        status.report(Observed::SubscriptionRequired);
                        tracing::warn!(
                            "cloud subscription required — manage at amux.sh/account; local agents remain available"
                        );
                        if !setup::cloud_enabled(&config) {
                            tracing::info!("cloud mode disabled, stopping reconnection");
                            return;
                        }
                        tracing::info!(
                            retry_delay = ?SUBSCRIPTION_RECHECK_INTERVAL,
                            "waiting to re-check cloud subscription"
                        );
                        if sleep_or_stop(SUBSCRIPTION_RECHECK_INTERVAL, &mut stop_rx).await {
                            return;
                        }
                        continue;
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
                status.report(Observed::Retrying);
                if !setup::cloud_enabled(&config) {
                    tracing::info!("cloud mode disabled, stopping reconnection");
                    return;
                }

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
    CloudConnector { stop_tx, task }
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable { msg: String, reset_backoff: bool },
    /// Error that should stop reconnection attempts.
    NonRetriable(String),
    /// Cloud access is unavailable until the user activates a subscription.
    SubscriptionRequired,
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
        CloudError::PaymentRequired => CloudConnectionError::SubscriptionRequired,
        CloudError::CloudDisabled => {
            status.report(Observed::Local);
            CloudConnectionError::NonRetriable("Cloud mode disabled".to_string())
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
    config: &Config,
    state: Arc<RwLock<ServerState>>,
    connector_ctx: LinkConnectorCtx,
    mut stop_rx: watch::Receiver<bool>,
    status: &RuntimeStatus,
    #[cfg(testnet)] transport: Option<tonic::transport::Channel>,
) -> std::result::Result<(), CloudConnectionError> {
    let prepared = tokio::select! {
        biased;
        _ = wait_for_stop(&mut stop_rx) => return Ok(()),
        prepared = prepare_cloud_connection(config, &state, status) => prepared,
    };
    let (credentials, details) = prepared?;

    run_cloud_connection_with_details(
        config,
        connector_ctx,
        credentials,
        details,
        stop_rx,
        status,
        #[cfg(testnet)]
        transport,
    )
    .await
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

async fn run_cloud_connection_with_details(
    config: &Config,
    connector_ctx: LinkConnectorCtx,
    credentials: Arc<dyn CredentialProvider>,
    details: CloudRoutingConnectionDetails,
    stop_rx: watch::Receiver<bool>,
    status: &RuntimeStatus,
    #[cfg(testnet)] transport: Option<tonic::transport::Channel>,
) -> std::result::Result<(), CloudConnectionError> {
    tracing::info!(host = %details.host, port = details.port, "connecting to cloud routing");
    #[cfg(testnet)]
    let channel = transport
        .map(Ok)
        .unwrap_or_else(|| cloud_routing_channel(details.host.clone(), details.port));
    #[cfg(not(testnet))]
    let channel = cloud_routing_channel(details.host.clone(), details.port);
    let channel = channel.map_err(|error| CloudConnectionError::Retriable {
        msg: format!("Connection failed: {error}"),
        reset_backoff: false,
    })?;
    let connected_at = std::time::Instant::now();
    let connector_auth = LinkConnectorAuth::new(
        LinkConnectorToken {
            token: details.token,
            expires_at: SystemTime::from(details.expires_at),
        },
        Arc::new(CloudLinkTokenRefresher {
            config: config.clone(),
            credentials,
            current_host: details.host,
            current_port: details.port,
        }),
    );
    let (connector_task, established_rx) =
        spawn_connector_to_channel_with_auth_establishment_and_shutdown(
            connector_ctx,
            channel,
            connector_auth,
            stop_rx,
        );
    let _abort_connector_on_drop = AbortTaskOnDrop(connector_task.abort_handle());
    await_cloud_establishment(
        status,
        established_rx,
        connected_at,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
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
            Err(cloud_connection_error_from_status(status, error, connected_at.elapsed()).await)
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
) -> std::result::Result<(), CloudConnectionError> {
    match tokio::time::timeout(timeout, established_rx).await {
        Ok(Ok(Ok(_))) => {
            observed.report(Observed::Connected);
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
    if payment_required_from_status(&status) {
        return CloudConnectionError::SubscriptionRequired;
    }
    CloudConnectionError::Retriable {
        msg: status.to_string(),
        reset_backoff: should_reset_backoff_after_connection(connection_uptime),
    }
}

fn payment_required_from_status(status: &tonic::Status) -> bool {
    protocol_error_from_status_details(status) == Some(ProtocolError::PaymentRequired)
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

fn cloud_routing_channel(
    host: String,
    port: u16,
) -> crate::transport::Result<tonic::transport::Channel> {
    tls_channel(host, port)
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
        CloudError::PaymentRequired => protocol_status(ProtocolError::PaymentRequired),
        CloudError::Rejected(message) => tonic::Status::permission_denied(message),
        CloudError::CloudDisabled => tonic::Status::failed_precondition("cloud disabled"),
        CloudError::Connection(message) => tonic::Status::unavailable(message),
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

    use tokio::io::AsyncReadExt;
    use tokio::sync::{RwLock, mpsc, oneshot};
    use uuid::Uuid;

    use super::{
        ABSOLUTE_JITTER_MAX, BACKOFF_RESET_AFTER_ESTABLISHED, INITIAL_BACKOFF, MAX_BACKOFF,
        SUBSCRIPTION_RECHECK_INTERVAL, await_cloud_establishment,
        cloud_connection_error_from_fetch, cloud_connection_error_from_status,
        cloud_token_refresh_status, establish_cloud_connection, jittered_backoff_with_samples,
        next_backoff, payment_required_from_status, should_reset_backoff_after_connection,
        sleep_or_stop,
    };
    use crate::auth::{AccessToken, AuthError, CredentialProvider};
    use crate::config::Config;
    use crate::profile::status::{Observed, RuntimeStatus};
    use crate::protocol::{ProtocolError, protocol_status};
    use crate::routing::{Capabilities, Host, LinkConnectorCtx, RoutingCore};
    use crate::subscription::SubscriptionReporter;
    use crate::tunnel::TunnelPool;
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

    #[derive(Default)]
    struct CapturingSubscriptionReporter {
        required: Mutex<Vec<bool>>,
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

    impl SubscriptionReporter for CapturingSubscriptionReporter {
        fn report_subscription_required(&self, required: bool) {
            self.required.lock().unwrap().push(required);
        }
    }

    #[test]
    fn fetch_error_classification_controls_retries() {
        assert!(matches!(
            cloud_connection_error_from_fetch(
                crate::auth::cloud::CloudError::PaymentRequired,
                &RuntimeStatus::new(None, None)
            ),
            super::CloudConnectionError::SubscriptionRequired
        ));
        assert!(matches!(
            cloud_connection_error_from_fetch(
                crate::auth::cloud::CloudError::Rejected("403 Forbidden".to_string()),
                &RuntimeStatus::new(None, None)
            ),
            super::CloudConnectionError::NonRetriable(_)
        ));
        assert!(matches!(
            cloud_connection_error_from_fetch(
                crate::auth::cloud::CloudError::Connection("temporary".to_string()),
                &RuntimeStatus::new(None, None)
            ),
            super::CloudConnectionError::Retriable { .. }
        ));
    }

    #[test]
    fn token_refresh_preserves_distinct_payment_required_protocol_error() {
        let status = cloud_token_refresh_status(crate::auth::cloud::CloudError::PaymentRequired);

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert!(payment_required_from_status(&status));
    }

    #[test]
    fn subscription_recheck_is_prompt_but_not_a_hot_loop() {
        assert!(SUBSCRIPTION_RECHECK_INTERVAL >= Duration::from_secs(1));
        assert!(SUBSCRIPTION_RECHECK_INTERVAL <= Duration::from_secs(30));
    }

    #[tokio::test]
    async fn profile_runtime_subscription_status_reports_required_then_healthy() {
        let reporter = Arc::new(CapturingSubscriptionReporter::default());
        let state = RuntimeStatus::new(None, Some(reporter.clone()));

        state.report(Observed::SubscriptionRequired);
        assert_eq!(*state.subscribe().borrow(), Observed::SubscriptionRequired);
        state.report(Observed::Retrying);
        assert_eq!(*reporter.required.lock().unwrap(), [true]);
        state.report(Observed::Connected);
        assert_eq!(*state.subscribe().borrow(), Observed::Connected);

        assert_eq!(*reporter.required.lock().unwrap(), [true, false]);
    }

    #[tokio::test]
    async fn profile_runtime_update_status_reports_through_configured_reporter() {
        let reporter = Arc::new(CapturingUpdateReporter::default());
        let state = RuntimeStatus::new(Some(reporter.clone()), None);

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
        state.report(Observed::Connected);

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
        let state = RuntimeStatus::new(Some(reporter.clone()), None);

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
            super::CloudConnectionError::SubscriptionRequired => {
                panic!("update-required status must not become subscription-required")
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
    async fn payment_required_status_uses_dedicated_recheck_state() {
        let state = RuntimeStatus::new(None, None);
        let status = protocol_status(ProtocolError::PaymentRequired);

        let error = cloud_connection_error_from_status(&state, status, Duration::ZERO).await;

        assert!(matches!(
            error,
            super::CloudConnectionError::SubscriptionRequired
        ));
    }

    #[tokio::test]
    async fn bare_permission_denied_status_remains_retriable() {
        let state = RuntimeStatus::new(None, None);
        let status = tonic::Status::permission_denied("cloud request rejected");

        let error = cloud_connection_error_from_status(&state, status, Duration::ZERO).await;

        assert!(matches!(
            error,
            super::CloudConnectionError::Retriable { .. }
        ));
    }

    #[tokio::test]
    async fn cloud_establishment_wait_times_out() {
        let state = RuntimeStatus::new(None, None);
        let (_tx, rx) = oneshot::channel();

        let error = await_cloud_establishment(
            &state,
            rx,
            std::time::Instant::now(),
            Duration::from_millis(10),
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
            super::CloudConnectionError::SubscriptionRequired => {
                panic!("timeout must not become subscription-required")
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
            enable_cloud_mode: Some(true),
            ..Config::default()
        };
        let host_id = Uuid::new_v4();
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let state = Arc::new(RwLock::new(ServerState::new(
            config.clone(),
            host_id,
            shutdown_tx,
            Some(Arc::new(StaticCredentials)),
            None,
        )));
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(host_id, routing.clone(), incoming_tx));
        let connector_ctx = LinkConnectorCtx::new(
            Host {
                id: host_id,
                name: "local".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: Capabilities::default(),
            },
            routing,
            tunnels,
        );
        let connector = establish_cloud_connection(
            config,
            state,
            connector_ctx,
            RuntimeStatus::new(None, None),
            #[cfg(testnet)]
            None,
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
