//! Where a runtime obtains provider sessions when it must not launch real
//! providers, and how it lets that supplier watch the semantic input each
//! session receives.
//!
//! A production runtime carries no sources: every session comes from a
//! spawned provider process. A harness installs one so that the same backends,
//! folds and hooks run against scripted or recorded providers, without the
//! product knowing anything about scripts.

use async_trait::async_trait;
use claude::sdk::QueryOptions;
use model::{ArtifactId, ClaudePtyIntent};
use uuid::Uuid;

#[async_trait]
pub trait ProviderSources: Send + Sync {
    /// Every semantic input a Claude PTY agent accepted, with the artifacts
    /// pinned to it, after the session's control took it.
    ///
    /// A supplier that did not expect the input says so, and the runtime
    /// reports that refusal to the client as the input's outcome. The scripted
    /// provider stands in for a person's real agent, and a script that has no
    /// answer for a prompt is a test that went off its script: failing the
    /// send there, with the script's reason, is what stops a driver waiting
    /// on a reply that will never come.
    fn observe_claude_pty_input(
        &self,
        agent_id: Uuid,
        intent: &ClaudePtyIntent,
        pins: &[ArtifactId],
    ) -> Result<(), String> {
        let _ = (agent_id, intent, pins);
        Ok(())
    }

    /// An SDK session for this agent instead of spawning the SDK process, or
    /// the options handed back so the runtime launches normally.
    async fn claude_sdk(&self, agent_id: Uuid, options: QueryOptions) -> ClaudeSdkSource {
        let _ = agent_id;
        ClaudeSdkSource::Launch(Box::new(options))
    }
}

/// What a supplier answered when asked for an SDK session.
pub enum ClaudeSdkSource {
    Supplied(Result<claude::sdk::Session, claude::sdk::Error>),
    /// Boxed: the options are far larger than a session handle, and every
    /// caller moves them straight into the spawn.
    Launch(Box<QueryOptions>),
}
