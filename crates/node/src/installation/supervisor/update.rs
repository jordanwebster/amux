//! A durable installation transaction selects exactly the sessions to restore.

use std::io::Write;
use std::path::{Path, PathBuf};

use host_api::{HostResumeBatch, HostResumeStatus, LocalAgentHost, PreparedHostState};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuspendReason {
    User,
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSuspendResult {
    pub profile_id: ProfileId,
    pub agent_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendReport {
    pub profiles: Vec<ProfileSuspendResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentResumeStatus {
    Resumed,
    AlreadyRunning,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResumeResult {
    pub agent_id: Uuid,
    pub status: AgentResumeStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileResumeResult {
    pub profile_id: ProfileId,
    pub agents: Vec<AgentResumeResult>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeReport {
    pub profiles: Vec<ProfileResumeResult>,
}

#[derive(Serialize, Deserialize)]
struct PreparedProfile {
    id: ProfileId,
    host_state: PreparedHostState,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Prepared,
    Suspended,
    Complete,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Journal {
    suspend_operation: OperationId,
    reason: SuspendReason,
    phase: Phase,
    profiles: Vec<PreparedProfile>,
    resume_operation: Option<OperationId>,
    resume_report: Option<ResumeReport>,
}

impl Journal {
    pub(super) fn load(root: &Path) -> Result<Option<Self>, InstallationError> {
        match std::fs::read(root.join("update.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                InstallationError::Registry(format!("invalid update journal: {error}"))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, root: &Path) -> Result<(), InstallationError> {
        let mut staged = tempfile::NamedTempFile::new_in(root)?;
        serde_json::to_writer(&mut staged, self)
            .map_err(|error| InstallationError::Registry(error.to_string()))?;
        staged.flush()?;
        staged.as_file().sync_all()?;
        staged
            .persist(root.join("update.json"))
            .map_err(|e| e.error)?;
        #[cfg(unix)]
        std::fs::File::open(root)?.sync_all()?;
        Ok(())
    }

    pub(super) fn pending(&self) -> bool {
        self.phase != Phase::Complete
    }

    fn suspend_report(&self) -> SuspendReport {
        SuspendReport {
            profiles: self
                .profiles
                .iter()
                .map(|profile| ProfileSuspendResult {
                    profile_id: profile.id,
                    agent_ids: profile.host_state.agent_ids.clone(),
                })
                .collect(),
        }
    }
}

struct Target {
    id: ProfileId,
    slot: Arc<Slot>,
    host: Option<Arc<dyn LocalAgentHost>>,
    host_factory: Option<Arc<dyn host_api::LocalAgentHostFactory>>,
    state_path: PathBuf,
}

impl Installation {
    pub async fn suspend_all(
        &self,
        op: OperationId,
        reason: SuspendReason,
    ) -> Result<SuspendReport, InstallationError> {
        match self
            .inner
            .operate(op, Mutation::SuspendAll(op, reason))
            .await?
        {
            Outcome::Suspended(report) => Ok(report),
            _ => unreachable!(),
        }
    }

    pub async fn resume_all(&self, op: OperationId) -> Result<ResumeReport, InstallationError> {
        match self.inner.operate(op, Mutation::ResumeAll(op)).await? {
            Outcome::Resumed(report) => Ok(report),
            _ => unreachable!(),
        }
    }
}

impl Inner {
    async fn update_targets(&self) -> Result<Vec<Target>, InstallationError> {
        let slots: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .profiles
            .iter()
            .filter(|(_, entry)| !entry.deleting)
            .map(|(id, entry)| (*id, entry.slot.clone()))
            .collect();
        // Do not retain these guards while waiting on a host: an admitted
        // resume can need the read gate before releasing its host resume lock.
        for (_, slot) in &slots {
            let _gate = slot.operations.barrier().await;
            slot.operations.freeze();
        }
        let mut targets = Vec::new();
        for (id, slot) in slots {
            let runtime = slot.runtime.lock().await;
            let host = runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_host.clone());
            drop(runtime);
            targets.push(Target {
                id,
                slot,
                host,
                host_factory: self.host_factory.clone(),
                state_path: self
                    .root
                    .join("profiles")
                    .join(id.to_string())
                    .join("state/state.yaml"),
            });
        }
        Ok(targets)
    }

    fn thaw_update(&self) {
        let mut state = self.state.lock().unwrap();
        state.update_active = false;
        for entry in state.profiles.values() {
            entry.slot.operations.thaw();
        }
    }

    pub(super) async fn update(&self, request: Mutation) -> OperationResult {
        let result = match request {
            Mutation::SuspendAll(op, reason) => {
                self.suspend_all(op, reason).await.map(Outcome::Suspended)
            }
            Mutation::ResumeAll(op) => self.resume_all(op).await.map(Outcome::Resumed),
            _ => unreachable!(),
        };
        // A rename can become visible even when directory fsync reports an
        // error. Keep admission closed whenever recovery may need the journal.
        if !Journal::load(&self.root)?.is_some_and(|journal| journal.pending()) {
            self.thaw_update();
        } else {
            self.state.lock().unwrap().update_active = true;
        }
        result
    }

    async fn suspend_all(
        &self,
        op: OperationId,
        reason: SuspendReason,
    ) -> Result<SuspendReport, InstallationError> {
        let previous = Journal::load(&self.root)?;
        if let Some(journal) = &previous {
            if journal.suspend_operation == op && journal.reason != reason {
                return Err(InstallationError::Registry(
                    "operation id reused with a different request".into(),
                ));
            }
            if journal.phase == Phase::Complete && journal.suspend_operation == op {
                return Ok(journal.suspend_report());
            }
            if journal.phase == Phase::Suspended {
                return Ok(journal.suspend_report());
            }
        }
        let targets = self.update_targets().await?;
        let mut journal = if let Some(journal) = previous.filter(|journal| journal.pending()) {
            journal
        } else {
            let mut profiles: Vec<PreparedProfile> = Vec::new();
            for target in &targets {
                let host_state = match &target.host {
                    Some(host) => match host.prepare_update().await {
                        Ok(state) => state,
                        Err(error) => {
                            for prepared in &profiles {
                                if let Some(prepared_host) = targets
                                    .iter()
                                    .find(|candidate| candidate.id == prepared.id)
                                    .and_then(|candidate| candidate.host.as_ref())
                                {
                                    let _ = prepared_host
                                        .abort_update(prepared.host_state.clone())
                                        .await;
                                }
                            }
                            return Err(InstallationError::Unavailable(format!(
                                "profile {}: {error}",
                                target.id
                            )));
                        }
                    },
                    None => PreparedHostState {
                        agent_ids: Vec::new(),
                        payload: Vec::new(),
                    },
                };
                profiles.push(PreparedProfile {
                    id: target.id,
                    host_state,
                });
            }
            let journal = Journal {
                suspend_operation: op,
                reason,
                phase: Phase::Prepared,
                profiles,
                resume_operation: None,
                resume_report: None,
            };
            if let Err(error) = journal.save(&self.root) {
                for prepared in &journal.profiles {
                    if let Some(host) = targets
                        .iter()
                        .find(|target| target.id == prepared.id)
                        .and_then(|target| target.host.as_ref())
                    {
                        let _ = host.abort_update(prepared.host_state.clone()).await;
                    }
                }
                return Err(error);
            }
            journal
        };
        for target in &targets {
            if let Some(host) = &target.host {
                host.notify_shutdown(match journal.reason {
                    SuspendReason::User => ShutdownReason::Suspending,
                    SuspendReason::Update => ShutdownReason::Updating,
                })
                .await;
                host.commit_suspend().await;
            }
        }
        journal.phase = Phase::Suspended;
        journal.save(&self.root)?;
        Ok(journal.suspend_report())
    }

    async fn resume_all(&self, op: OperationId) -> Result<ResumeReport, InstallationError> {
        let Some(mut journal) = Journal::load(&self.root)? else {
            return Ok(ResumeReport::default());
        };
        if !journal.pending() {
            return Ok(journal.resume_report.unwrap_or_default());
        }
        let targets = self.update_targets().await?;
        let mut report = ResumeReport::default();
        let mut cleanup_pending = false;
        for profile in &journal.profiles {
            let target = targets.iter().find(|target| target.id == profile.id);
            let (batch, mut error) = match target {
                Some(Target {
                    host: Some(host),
                    slot,
                    ..
                }) => (
                    host.resume_update(profile.host_state.clone(), &slot.operations)
                        .await,
                    None,
                ),
                _ if profile.host_state.agent_ids.is_empty() => (HostResumeBatch::default(), None),
                _ => {
                    let cleanup_error = target
                        .and_then(|target| {
                            target
                                .host_factory
                                .as_ref()
                                .map(|factory| (target, factory))
                        })
                        .and_then(|(target, factory)| {
                            factory
                                .restore_prepared(&target.state_path, profile.host_state.clone())
                                .err()
                                .map(|error| error.to_string())
                        });
                    (
                        HostResumeBatch {
                            agents: profile
                                .host_state
                                .agent_ids
                                .iter()
                                .map(|agent_id| host_api::HostResumeResult {
                                    agent_id: *agent_id,
                                    status: HostResumeStatus::Failed,
                                })
                                .collect(),
                            cleanup_error,
                        },
                        Some("profile agent host is unavailable".into()),
                    )
                }
            };
            if let Some(cleanup_error) = batch.cleanup_error {
                cleanup_pending = true;
                error = Some(cleanup_error);
            }
            report.profiles.push(ProfileResumeResult {
                profile_id: profile.id,
                agents: batch
                    .agents
                    .into_iter()
                    .map(|agent| AgentResumeResult {
                        agent_id: agent.agent_id,
                        status: match agent.status {
                            HostResumeStatus::Resumed => AgentResumeStatus::Resumed,
                            HostResumeStatus::AlreadyRunning => AgentResumeStatus::AlreadyRunning,
                            HostResumeStatus::Failed => AgentResumeStatus::Failed,
                        },
                    })
                    .collect(),
                error,
            });
        }
        if !cleanup_pending {
            journal.phase = Phase::Complete;
            journal.resume_operation = Some(op);
            journal.resume_report = Some(report.clone());
            journal.save(&self.root)?;
        }
        Ok(report)
    }
}
