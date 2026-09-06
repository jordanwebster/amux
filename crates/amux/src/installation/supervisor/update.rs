//! A durable installation transaction selects exactly the sessions to restore.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::*;
use crate::services::LocalAgentHost;
use crate::suspend::{self, SuspendedAgent, SuspendedServerState};

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
    agents: Vec<SuspendedAgent>,
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
                    agent_ids: profile
                        .agents
                        .iter()
                        .map(SuspendedAgent::agent_id)
                        .collect(),
                })
                .collect(),
        }
    }
}

struct Target {
    id: ProfileId,
    slot: Arc<Slot>,
    host: Option<Arc<dyn LocalAgentHost>>,
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
            let _gate = slot.operations.lock().await;
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
            let mut profiles = Vec::new();
            for target in &targets {
                let agents = match &target.host {
                    Some(host) => host.prepare_update().await.map_err(|error| {
                        InstallationError::Unavailable(format!("profile {}: {error}", target.id))
                    })?,
                    None => Vec::new(),
                };
                // Reading even empty profiles detects corrupt retained state
                // before any other profile can lose a running session.
                let mut retained =
                    suspend::load_suspended(&target.state_path).map_err(|error| {
                        InstallationError::Unavailable(format!("profile {}: {error}", target.id))
                    })?;
                if !agents.is_empty() {
                    let ids: HashSet<_> = agents.iter().map(SuspendedAgent::agent_id).collect();
                    retained
                        .agents
                        .retain(|agent| !ids.contains(&agent.agent_id()));
                    retained.agents.extend(agents.clone());
                    suspend::save_suspended(&target.state_path, &retained)?;
                }
                profiles.push(PreparedProfile {
                    id: target.id,
                    agents,
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
            journal.save(&self.root)?;
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
            let (agents, mut error) = match target {
                Some(Target {
                    host: Some(host),
                    slot,
                    ..
                }) => (
                    host.resume_update(profile.agents.clone(), &slot.operations)
                        .await,
                    None,
                ),
                _ if profile.agents.is_empty() => (Vec::new(), None),
                _ => (
                    profile
                        .agents
                        .iter()
                        .map(|agent| AgentResumeResult {
                            agent_id: agent.agent_id(),
                            status: AgentResumeStatus::Failed,
                        })
                        .collect(),
                    Some("profile agent host is unavailable".into()),
                ),
            };
            if let Some(target) = target {
                let successful: HashSet<_> = agents
                    .iter()
                    .filter(|agent| agent.status != AgentResumeStatus::Failed)
                    .map(|agent| agent.agent_id)
                    .collect();
                let cleanup = (|| -> Result<(), InstallationError> {
                    let mut retained = suspend::load_suspended(&target.state_path)
                        .map_err(|error| InstallationError::Unavailable(error.to_string()))?;
                    retained
                        .agents
                        .retain(|agent| !successful.contains(&agent.agent_id()));
                    // A previous attempt may have consumed this profile's
                    // records before another profile's cleanup failed. If a
                    // restart now cannot restore them, park the journal copies.
                    let retained_ids: HashSet<_> = retained
                        .agents
                        .iter()
                        .map(SuspendedAgent::agent_id)
                        .collect();
                    retained.agents.extend(
                        profile
                            .agents
                            .iter()
                            .filter(|agent| {
                                !successful.contains(&agent.agent_id())
                                    && !retained_ids.contains(&agent.agent_id())
                            })
                            .cloned(),
                    );
                    if retained.agents.is_empty() {
                        suspend::remove_suspended(&target.state_path)?;
                    } else {
                        suspend::save_suspended(
                            &target.state_path,
                            &SuspendedServerState {
                                agents: retained.agents,
                            },
                        )?;
                    }
                    Ok(())
                })();
                if let Err(failure) = cleanup {
                    cleanup_pending = true;
                    error = Some(failure.to_string());
                }
            }
            report.profiles.push(ProfileResumeResult {
                profile_id: profile.id,
                agents,
                error,
            });
        }
        // Start failures are final for this transaction; their snapshots stay
        // parked in the owning profile. Only storage failures need recovery to
        // keep admission closed. Persist completion before update() thaws it.
        if !cleanup_pending {
            journal.phase = Phase::Complete;
            journal.resume_operation = Some(op);
            journal.resume_report = Some(report.clone());
            journal.save(&self.root)?;
        }
        Ok(report)
    }
}
