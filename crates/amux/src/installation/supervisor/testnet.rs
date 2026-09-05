//! Observation and listener injection for the production-supervisor spec harness.

use super::*;

pub(super) type RuntimeFixtureFactory =
    Arc<dyn Fn(ProfileId) -> runtime::RuntimeFixtures + Send + Sync>;

impl Installation {
    pub(crate) async fn refresh_for_test(
        &self,
        id: ProfileId,
    ) -> Result<(), crate::auth::AuthError> {
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

    pub(crate) async fn open_for_test(
        options: InstallationOptions,
        fixtures: RuntimeFixtureFactory,
    ) -> Result<Self, InstallationError> {
        Self::open_inner(options, Some(fixtures)).await
    }

    pub(crate) fn test_root(&self) -> PathBuf {
        self.inner.root.clone()
    }

    pub(crate) async fn test_runtime(
        &self,
        id: ProfileId,
    ) -> Option<tokio::sync::OwnedMutexGuard<Option<ProfileRuntime>>> {
        let runtime = {
            let state = self.inner.state.lock().unwrap();
            state.active(id).ok()?.slot.runtime.clone()
        };
        Some(runtime.lock_owned().await)
    }

    pub(crate) async fn stop_for_test(&self) {
        self.inner.shutdown(ShutdownReason::UserRequested).await;
    }
}
