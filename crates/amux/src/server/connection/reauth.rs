use std::time::Duration;

use tokio::sync::mpsc;

use super::context::{ConnectionError, Result};
use crate::auth::cloud::{CloudError, TokenRefreshState};
use crate::protocol::message::Message;

pub(super) const REFRESH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);

/// Manages token refresh lifecycle within a connection loop.
///
/// Encapsulates the two-phase refresh protocol: wait for a deadline, send a
/// Reauth with a fresh token, then await the ReauthResult response (with a
/// timeout). The connection loop uses [`deadlines`](Self::deadlines) for
/// select! guards, [`send_refresh`](Self::send_refresh) to initiate, and
/// [`try_intercept`](Self::try_intercept) to consume ReauthResult responses.
pub(super) struct TokenRefresher {
    inner: TokenRefreshState,
    deadline: tokio::time::Instant,
    awaiting_since: Option<tokio::time::Instant>,
}

impl TokenRefresher {
    pub(super) fn new(state: TokenRefreshState) -> Self {
        let deadline = state.refresh_deadline();
        Self {
            inner: state,
            deadline,
            awaiting_since: None,
        }
    }

    /// Returns (refresh_deadline, refresh_timeout) for use in select! guards.
    ///
    /// When idle, returns `(Some(deadline), None)`.
    /// When awaiting a response, returns `(None, Some(timeout))`.
    pub(super) fn deadlines(&self) -> (Option<tokio::time::Instant>, Option<tokio::time::Instant>) {
        if let Some(since) = self.awaiting_since {
            (None, Some(since + REFRESH_RESPONSE_TIMEOUT))
        } else {
            (Some(self.deadline), None)
        }
    }

    pub(super) fn is_awaiting_response(&self) -> bool {
        self.awaiting_since.is_some()
    }

    /// Send the token refresh request. Call when refresh_deadline fires.
    pub(super) async fn send_refresh(&mut self, tx: &mpsc::Sender<Message>) -> Result<()> {
        tracing::debug!("refreshing cloud token");
        self.inner
            .send_reauth(tx)
            .await
            .map_err(cloud_err_to_connection)?;
        self.awaiting_since = Some(tokio::time::Instant::now());
        Ok(())
    }

    /// Try to consume an incoming ReauthResult as a refresh response.
    /// Returns `true` if consumed, `false` if the message is not a ReauthResult.
    pub(super) fn try_intercept(&mut self, msg: &Message) -> Result<bool> {
        if !matches!(msg, Message::ReauthResponse(_)) {
            return Ok(false);
        }
        if self.awaiting_since.is_none() {
            tracing::warn!("unexpected ReauthResult");
            return Ok(true);
        }
        self.inner
            .handle_reauth_result(msg)
            .map_err(cloud_err_to_connection)?;
        self.deadline = self.inner.refresh_deadline();
        self.awaiting_since = None;
        Ok(true)
    }
}

fn cloud_err_to_connection(e: CloudError) -> ConnectionError {
    match e {
        CloudError::HostChanged => {
            tracing::warn!("cloud host changed, reconnection required");
            ConnectionError::Config(
                "cloud host changed — will reconnect to new host automatically".to_string(),
            )
        }
        CloudError::ProtocolMismatch {
            server_version,
            client_version,
        } => ConnectionError::ProtocolMismatch {
            server_version,
            client_version,
        },
        CloudError::UpdateRequired {
            minimum_version,
            client_version,
        } => ConnectionError::UpdateRequired {
            minimum_version,
            client_version,
        },
        CloudError::Auth(_) => ConnectionError::InvalidCredentials,
        other => {
            tracing::error!(error = %other, "token refresh failed");
            ConnectionError::Config(format!("token refresh failed: {other}"))
        }
    }
}
