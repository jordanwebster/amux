//! Observation and listener injection for the production-supervisor spec harness.

use super::*;

pub(crate) type RuntimeFixtureFactory = Arc<
    dyn Fn(ProfileId) -> futures_util::future::BoxFuture<'static, runtime::RuntimeFixtures>
        + Send
        + Sync,
>;

impl Installation {
    #[doc(hidden)]
    pub async fn hold_update_preparation_for_test(
        &self,
        id: ProfileId,
    ) -> (
        tokio::sync::OwnedMutexGuard<Option<ProfileRuntime>>,
        Arc<host_api::OperationGate>,
    ) {
        let slot = self.inner.state.lock().unwrap().profiles[&id].slot.clone();
        (
            slot.runtime.clone().lock_owned().await,
            slot.operations.clone(),
        )
    }

    #[doc(hidden)]
    pub async fn retained_work_for_test(
        &self,
        id: ProfileId,
    ) -> (
        crate::services::AgentServiceCtx,
        crate::services::PeerTrustCommitContext,
    ) {
        let slot = self.inner.state.lock().unwrap().profiles[&id].slot.clone();
        let runtime = slot.runtime.lock().await;
        let runtime = runtime.as_ref().unwrap();
        (
            runtime.services.agent.clone(),
            crate::services::PeerTrustCommitContext::new(
                runtime.trust.clone(),
                slot.operations.clone(),
                runtime.services.connections.clone(),
                self.inner
                    .root
                    .join("profiles")
                    .join(id.to_string())
                    .join("data"),
            ),
        )
    }

    #[doc(hidden)]
    pub async fn refresh_for_test(&self, id: ProfileId) -> Result<(), crate::auth::AuthError> {
        use crate::auth::CredentialProvider;
        let store = self.inner.state.lock().unwrap().profiles[&id]
            .slot
            .credentials
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        let token = store.access_token().await?;
        store.invalidate(&token);
        store.access_token().await.map(|_| ())
    }

    #[doc(hidden)]
    pub async fn open_for_test(
        options: InstallationOptions,
        fixtures: RuntimeFixtureFactory,
    ) -> Result<Self, InstallationError> {
        Self::open_inner(options, None, Some(fixtures)).await
    }

    #[doc(hidden)]
    pub fn test_root(&self) -> PathBuf {
        self.inner.root.clone()
    }

    #[doc(hidden)]
    pub async fn stop_for_test(&self) {
        self.inner.shutdown(ShutdownReason::UserRequested).await;
    }
}
