//! Where a scripted daemon's agents get their provider sessions.
//!
//! A daemon started by the harness installs one of these on its agent
//! runtime. The runtime keeps running the same backends, folds and hooks it
//! runs in production; only the far side of each session is scripted, and the
//! runtime reaches it through the supplier rather than by launching a
//! process.

use std::collections::HashMap;
use std::sync::Mutex;

use agent_runtime::test_support::{ClaudeSdkSource, ProviderSources};
use async_trait::async_trait;
use claude::sdk::QueryOptions;
use model::{ArtifactId, ClaudePtyIntent};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct DaemonSources {
    /// Every scripted Claude PTY agent on this daemon, by agent identity, so
    /// the semantic input the runtime accepts for it reaches its script.
    claude: Mutex<HashMap<Uuid, super::script::Provider>>,
    /// The one SDK transport script this daemon answers every SDK session
    /// with, when it has one.
    sdk: Mutex<Option<super::sdk::Provider>>,
}

impl DaemonSources {
    pub(crate) fn attach_claude(&self, agent: Uuid, provider: super::script::Provider) {
        self.claude
            .lock()
            .expect("scripted Claude providers poisoned")
            .insert(agent, provider);
    }

    pub(crate) fn claude(&self, agent: Uuid) -> Option<super::script::Provider> {
        self.claude
            .lock()
            .expect("scripted Claude providers poisoned")
            .get(&agent)
            .cloned()
    }

    pub(crate) fn script_sdk(&self, provider: super::sdk::Provider) {
        *self.sdk.lock().expect("scripted SDK provider poisoned") = Some(provider);
    }

    pub(crate) fn sdk(&self) -> Option<super::sdk::Provider> {
        self.sdk
            .lock()
            .expect("scripted SDK provider poisoned")
            .clone()
    }
}

#[async_trait]
impl ProviderSources for DaemonSources {
    fn observe_claude_pty_input(
        &self,
        agent_id: Uuid,
        intent: &ClaudePtyIntent,
        pins: &[ArtifactId],
    ) {
        if let Some(provider) = self.claude(agent_id) {
            // A script that refuses the input records the failure on its
            // provider, where the test reads it; the runtime already
            // accepted the input and is not told twice.
            let _ = provider.feed(intent.clone(), pins.to_vec());
        }
    }

    async fn claude_sdk(&self, agent_id: Uuid, options: QueryOptions) -> ClaudeSdkSource {
        match self.sdk() {
            Some(provider) => ClaudeSdkSource::Supplied(provider.open(agent_id, options).await),
            None => ClaudeSdkSource::Launch(Box::new(options)),
        }
    }
}
