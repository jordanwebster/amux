use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{
    ApprovalPolicy, ApprovalResponse, Error, ReasoningEffort, RequestId, SandboxPolicy, Thread,
    ThreadEventStream, TurnConfig, TurnInput,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub supported_reasoning_efforts: Vec<EffortInfo>,
    pub default_reasoning_effort: ReasoningEffort,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffortInfo {
    pub reasoning_effort: ReasoningEffort,
    pub description: String,
}

/// An enabled skill reported for this thread's working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCommand {
    pub name: String,
    pub path: std::path::PathBuf,
}

#[derive(Default)]
struct Settings {
    config: TurnConfig,
    models: Vec<ModelInfo>,
    commands: Vec<SkillCommand>,
}

/// Host-owned next-turn selections, retained when a thread's transport reconnects.
#[derive(Clone, Default)]
pub struct SessionSettings(Arc<Mutex<Settings>>);

/// Codex's opaque identifier for one turn in a thread.
pub type TurnId = String;

/// One owned event stream and the control handle for its Codex thread.
pub struct Session {
    pub events: ThreadEventStream,
    pub control: ThreadControl,
}

/// Opens a thread's event stream and pairs it with its restricted control handle.
pub async fn open(thread: Thread) -> Result<Session, Error> {
    open_with_settings(thread, SessionSettings::default()).await
}

/// Reattach a thread using the host's existing selections.
pub async fn open_with_settings(
    thread: Thread,
    settings: SessionSettings,
) -> Result<Session, Error> {
    let events = thread.events().await?;
    Ok(Session {
        events,
        control: ThreadControl { thread, settings },
    })
}

/// The operations a session host may perform on a Codex thread.
#[derive(Clone)]
pub struct ThreadControl {
    thread: Thread,
    settings: SessionSettings,
}

impl ThreadControl {
    pub fn session_settings(&self) -> SessionSettings {
        self.settings.clone()
    }

    pub async fn user_turn(&self, input: TurnInput) -> Result<TurnId, Error> {
        let config = self
            .settings
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .config
            .clone();
        self.thread.start_turn_with(input, config).await
    }

    pub async fn empty_turn(&self) -> Result<TurnId, Error> {
        self.user_turn(TurnInput::Items(Vec::new())).await
    }

    /// Fetch all model pages from this session's app-server, never a built-in list.
    pub async fn discover_models(&self) -> Result<(), Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            data: Vec<ModelInfo>,
            next_cursor: Option<String>,
        }
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut models = Vec::new();
        loop {
            let page: Page = self
                .thread
                .inner
                .server
                .request("model/list", serde_json::json!({"cursor":cursor}))
                .await?;
            models.extend(page.data);
            let Some(next) = page.next_cursor else {
                break;
            };
            if !seen.insert(next.clone()) {
                return Err(anyhow::anyhow!("model/list repeated a cursor").into());
            }
            cursor = Some(next);
        }
        self.settings
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .models = models;
        Ok(())
    }

    /// Discover provider commands in the thread's directory. Terminal slash menus
    /// are client-local and are not part of the app-server's skill catalogue.
    pub async fn discover_commands(&self) -> Result<(), Error> {
        #[derive(Deserialize)]
        struct Skill {
            name: String,
            path: std::path::PathBuf,
            enabled: bool,
        }
        #[derive(Deserialize)]
        struct Entry {
            cwd: std::path::PathBuf,
            skills: Vec<Skill>,
        }
        #[derive(Deserialize)]
        struct Response {
            data: Vec<Entry>,
        }
        // A failed refresh must not leave stale commands sendable.
        self.settings
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .commands
            .clear();
        let cwd = &self.thread.session_info().cwd;
        let response: Response = self
            .thread
            .inner
            .server
            .request("skills/list", serde_json::json!({"cwds":[cwd]}))
            .await?;
        let mut commands: Vec<_> = response
            .data
            .into_iter()
            .filter(|entry| &entry.cwd == cwd)
            .flat_map(|entry| entry.skills)
            .filter(|skill| skill.enabled && !skill.name.is_empty() && skill.path.is_absolute())
            .map(|skill| SkillCommand {
                name: skill.name,
                path: skill.path,
            })
            .collect();
        commands.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
        commands.dedup();
        let mut counts = std::collections::BTreeMap::new();
        for command in &commands {
            *counts.entry(command.name.clone()).or_insert(0) += 1;
        }
        commands.retain(|command| counts[&command.name] == 1);
        self.settings
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .commands = commands;
        Ok(())
    }

    /// Resolve the selected token on the host; clients never supply filesystem paths.
    pub async fn command(&self, name: String, args: String) -> Result<TurnId, Error> {
        let skill = self
            .settings
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .commands
            .iter()
            .find(|command| command.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("command is not offered by this session"))?;
        self.user_turn(TurnInput::Items(vec![
            crate::InputItem::Skill {
                name: skill.name,
                path: skill.path,
            },
            crate::InputItem::text(args),
        ]))
        .await
    }

    /// Snapshot the selected next-turn configuration alongside provider metadata.
    pub fn session_facts(&self) -> serde_json::Value {
        let settings = self.settings.0.lock().unwrap_or_else(|p| p.into_inner());
        let initial = self.thread.session_info();
        serde_json::json!({
            "model":settings.config.model.as_ref().unwrap_or(&initial.model),
            "effort":settings.config.effort.as_ref().or(initial.reasoning_effort.as_ref()),
            "approvalPolicy":settings.config.approval_policy.as_ref().unwrap_or(&initial.approval_policy),
            "sandbox":settings.config.sandbox_policy.as_ref().unwrap_or(&initial.sandbox),
            "models":settings.models,
            "commands":settings.commands.iter().map(|command| serde_json::json!({
                "name":command.name, "source":"codex", "terminal_only":false
            })).collect::<Vec<_>>(),
        })
    }

    pub fn set_model(&self, model: String) -> Result<(), Error> {
        let mut settings = self.settings.0.lock().unwrap_or_else(|p| p.into_inner());
        let selected = settings
            .models
            .iter()
            .find(|item| item.model == model)
            .ok_or_else(|| anyhow::anyhow!("model is not offered by this session"))?;
        let effort = selected.default_reasoning_effort.clone();
        settings.config.model = Some(model);
        settings.config.effort = Some(effort);
        Ok(())
    }

    pub fn set_effort(&self, effort: ReasoningEffort) -> Result<(), Error> {
        let mut settings = self.settings.0.lock().unwrap_or_else(|p| p.into_inner());
        let model = settings
            .config
            .model
            .as_ref()
            .unwrap_or(&self.thread.session_info().model);
        if !settings.models.iter().any(|item| {
            &item.model == model
                && item
                    .supported_reasoning_efforts
                    .iter()
                    .any(|level| level.reasoning_effort == effort)
        }) {
            return Err(anyhow::anyhow!("effort is not offered by this session's model").into());
        }
        settings.config.effort = Some(effort);
        Ok(())
    }

    pub fn set_preset(&self, approval: ApprovalPolicy, sandbox: SandboxPolicy) {
        let mut settings = self.settings.0.lock().unwrap_or_else(|p| p.into_inner());
        settings.config.approval_policy = Some(approval);
        settings.config.sandbox_policy = Some(sandbox);
    }

    pub async fn steer(&self, turn: &TurnId, input: TurnInput) -> Result<TurnId, Error> {
        self.thread.steer(turn, input).await
    }

    pub async fn interrupt(&self, turn: &TurnId) -> Result<(), Error> {
        self.thread.interrupt(turn).await
    }

    pub async fn approve(
        &self,
        request: RequestId,
        decision: ApprovalResponse,
    ) -> Result<(), Error> {
        self.thread.respond_approval(request, decision).await
    }

    pub async fn inject(&self, items: Vec<serde_json::Value>) -> Result<(), Error> {
        self.thread.inject_items(items).await
    }

    pub fn thread_id(&self) -> &str {
        self.thread.id()
    }
}
