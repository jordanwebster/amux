use std::any::Any;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;
use model::envelope::Envelope;
use model::{
    Agent, AgentEvent, AgentId, ArtifactId, ArtifactKind, ArtifactRef, CreateAgentRequest,
    HookEnvironment, ProtocolError, RenameAgentRequest, SessionArgs, SessionInput, ShutdownReason,
    SpawnInheritance,
};
pub use model::{
    SessionArgs as HostSessionArgs, SessionInput as HostSessionInput,
    SubscribeSessionEvent as HostSessionEvent,
};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequest {
    pub agent_id: AgentId,
    pub args: SessionArgs,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInputRequest {
    pub agent_id: AgentId,
    pub input_id: Vec<u8>,
    pub input: SessionInput,
    pub pin: Vec<ArtifactId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSetAgentStatus {
    pub agent_id: AgentId,
    pub working_on: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ArtifactBlob {
    pub artifact: ArtifactRef,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct HostDebugAgent {
    pub agent: Agent,
    pub session: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostResourceInventory {
    pub agents: usize,
    pub retained_artifacts: usize,
}

/// Opaque provider state prepared by a host for an installation transaction.
/// The node persists it without depending on provider persistence records.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreparedHostState {
    pub agent_ids: Vec<AgentId>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HostResumeStatus {
    Resumed,
    AlreadyRunning,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostResumeResult {
    pub agent_id: AgentId,
    pub status: HostResumeStatus,
}

#[derive(Clone, Debug, Default)]
pub struct HostResumeBatch {
    pub agents: Vec<HostResumeResult>,
    /// Persistence cleanup failed after resume work ran. The installation
    /// keeps its journal pending and may safely retry the same prepared state.
    pub cleanup_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HostStreamError {
    #[error("{0}")]
    Protocol(#[from] ProtocolError),
    #[error("node is shutting down: {0}")]
    Shutdown(ShutdownReason),
}

pub type HostSessionStream =
    Pin<Box<dyn Stream<Item = Result<HostSessionEvent, HostStreamError>> + Send + 'static>>;

/// Runtime resources supplied by the desktop composition for one profile.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub host_id: Uuid,
    pub data_dir: PathBuf,
    pub state_path: PathBuf,
    pub runtime_dir: PathBuf,
    pub server_socket_path: PathBuf,
    pub executable: PathBuf,
    pub profile_config_path: Option<PathBuf>,
    pub claude_user_keymap_dir: PathBuf,
    /// Directories searched for Git repositories when a client asks where an
    /// agent could be started. Empty means nothing is enumerated.
    pub repository_roots: Vec<PathBuf>,
}

#[async_trait]
pub trait LocalAgentHost: Send + Sync {
    /// Returns the concrete host for diagnostics and feature-gated test support.
    fn as_any(&self) -> &dyn Any;
    fn capabilities(&self) -> model::Capabilities;
    async fn agent(&self, agent_id: AgentId) -> Result<Agent, ProtocolError>;
    /// Recent projects and repositories under the configured roots, filtered
    /// by an optional substring and bounded by `limit`.
    async fn list_repositories(
        &self,
        query: Option<String>,
        limit: u32,
    ) -> Result<model::ListRepositoriesResponse, ProtocolError>;
    async fn create(
        &self,
        request: CreateAgentRequest,
        operations: &OperationGate,
    ) -> Result<Agent, ProtocolError>;
    async fn spawn_inheritance(&self, agent_id: AgentId)
    -> Result<SpawnInheritance, ProtocolError>;
    async fn rename(&self, request: RenameAgentRequest) -> Result<Agent, ProtocolError>;
    async fn delete(
        &self,
        agent_id: AgentId,
        operation: OperationBarrier,
    ) -> Result<(), ProtocolError>;
    async fn send_message(&self, envelope: Envelope) -> Result<(), ProtocolError>;
    async fn send_message_waiting(
        &self,
        envelope: Envelope,
        timeout: Duration,
    ) -> Result<(), ProtocolError>;
    async fn set_agent_status(&self, request: HostSetAgentStatus) -> Result<(), ProtocolError>;
    async fn send_input(
        &self,
        request: SessionInputRequest,
        operation: OperationLease,
    ) -> Result<(), ProtocolError>;
    async fn put_artifact(
        &self,
        agent_id: AgentId,
        kind: ArtifactKind,
        name: String,
        mime: String,
        bytes: Vec<u8>,
        operation: OperationLease,
    ) -> Result<ArtifactRef, ProtocolError>;
    async fn put_artifact_by_agent(
        &self,
        agent_id: AgentId,
        kind: ArtifactKind,
        name: String,
        mime: String,
        bytes: Vec<u8>,
        operation: OperationLease,
    ) -> Result<ArtifactRef, ProtocolError>;
    async fn get_artifact(
        &self,
        agent_id: AgentId,
        id: ArtifactId,
        operation: OperationLease,
    ) -> Result<ArtifactBlob, ProtocolError>;
    async fn diff(
        &self,
        agent_id: AgentId,
        base: model::DiffBase,
        operation: OperationLease,
    ) -> Result<model::DiffResponse, ProtocolError>;
    async fn subscribe_session(
        &self,
        request: SessionRequest,
    ) -> Result<HostSessionStream, ProtocolError>;
    async fn agent_events_snapshot(
        &self,
    ) -> (Vec<AgentEvent>, tokio::sync::mpsc::Receiver<AgentEvent>);
    async fn subscribe_agent_events(&self) -> tokio::sync::mpsc::Receiver<AgentEvent>;
    async fn subscribe_outbound_envelopes(&self) -> tokio::sync::mpsc::Receiver<Envelope>;
    async fn handle_hook(
        &self,
        agent_id: AgentId,
        payload: Vec<u8>,
        env: HookEnvironment,
        external: bool,
    ) -> Result<(), ProtocolError>;
    async fn resume(
        &self,
        state_path: PathBuf,
        operations: &OperationGate,
    ) -> Result<(u64, u64), ProtocolError>;
    async fn prepare_update(&self) -> Result<PreparedHostState, ProtocolError>;
    async fn resume_update(
        &self,
        state: PreparedHostState,
        operations: &OperationGate,
    ) -> HostResumeBatch;
    async fn stop_all(&self);
    async fn prepare_suspend(&self, state_path: PathBuf) -> Result<u64, ProtocolError>;
    async fn commit_suspend(&self);
    async fn notify_shutdown(&self, reason: ShutdownReason);
    async fn agent_count(&self) -> usize;
    async fn resource_inventory(
        &self,
        state_path: PathBuf,
    ) -> Result<HostResourceInventory, ProtocolError>;
    async fn debug_dump(&self, verbose: bool) -> Vec<HostDebugAgent>;
}

pub trait LocalAgentHostFactory: Send + Sync {
    fn create(&self, config: HostConfig) -> Result<Arc<dyn LocalAgentHost>, std::io::Error>;
    /// Re-persist an opaque prepared state when its profile cannot start.
    fn restore_prepared(
        &self,
        state_path: &std::path::Path,
        state: PreparedHostState,
    ) -> Result<(), ProtocolError>;
}

/// An admitted operation. Holding this opaque value keeps lifecycle teardown
/// from removing the profile's storage until the operation has finished.
pub struct OperationLease {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive lifecycle access after all admitted operations have drained.
pub struct OperationBarrier {
    _guard: OwnedRwLockWriteGuard<()>,
}

/// Service work shares access to profile storage; lifecycle and trust commits
/// take exclusive access. Closing under the write lock drains accepted storage
/// work and prevents queued work from recreating a deleted device's state.
#[derive(Default)]
pub struct OperationGate {
    lock: Arc<RwLock<()>>,
    closed: AtomicBool,
    frozen: AtomicBool,
}

impl OperationGate {
    pub async fn admit(&self) -> Result<OperationLease, model::ProtocolError> {
        let guard = self.lock.clone().read_owned().await;
        self.check()?;
        Ok(OperationLease { _guard: guard })
    }

    pub async fn admit_mutation(&self) -> Result<OperationLease, model::ProtocolError> {
        let guard = self.lock.clone().read_owned().await;
        self.check_mutation()?;
        Ok(OperationLease { _guard: guard })
    }

    pub async fn barrier(&self) -> OperationBarrier {
        OperationBarrier {
            _guard: self.lock.clone().write_owned().await,
        }
    }

    /// Refuse new work, then wait until every accepted operation releases its
    /// owned lease. The returned barrier keeps storage teardown exclusive.
    pub async fn close_and_drain(&self) -> OperationBarrier {
        self.close();
        self.barrier().await
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Call under the exclusive gate to drain admitted lifecycle work first.
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    pub fn thaw(&self) {
        self.frozen.store(false, Ordering::Release);
    }

    pub fn check_mutation(&self) -> Result<(), model::ProtocolError> {
        self.check()?;
        if self.frozen.load(Ordering::Acquire) {
            return Err(model::ProtocolError::FailedPrecondition {
                message: "installation update is in progress".into(),
            });
        }
        Ok(())
    }

    pub fn check(&self) -> Result<(), model::ProtocolError> {
        if self.closed.load(Ordering::Acquire) {
            Err(model::ProtocolError::FailedPrecondition {
                message: "profile is unavailable".into(),
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn close_drains_accepted_work_and_rejects_queued_work() {
        let gate = Arc::new(OperationGate::default());
        let accepted = gate.admit_mutation().await.unwrap();

        let closing = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.close_and_drain().await })
        };
        tokio::task::yield_now().await;

        let queued = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.admit_mutation().await })
        };
        assert!(
            !closing.is_finished(),
            "accepted work must drain before teardown"
        );
        drop(accepted);
        let barrier = closing.await.unwrap();
        drop(barrier);
        assert!(matches!(
            queued.await.unwrap(),
            Err(model::ProtocolError::FailedPrecondition { .. })
        ));
    }
}
