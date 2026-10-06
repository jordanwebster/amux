//! The settings view: what the agent offers to change (model, effort,
//! permission, mode) with the current value of each marked, the commands it
//! offers, and, per kind, the sentence that says why a setting cannot change
//! from here.
//!
//! Everything offered comes from the agent's catalogue; a client never
//! supplies a list of its own.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui_state::SessionState;
use wire::Kind;

/// Claude commands that open an interactive screen of Claude's own
/// terminal, or change a setting this view offers directly: typed into a
/// headless agent they do nothing useful.
const TERMINAL_ONLY_COMMANDS: &[&str] = &[
    "agents",
    "auto-mode-setup",
    "color",
    "config",
    "doctor",
    "effort",
    "exit",
    "heapdump",
    "hooks",
    "ide",
    "keybindings",
    "login",
    "logout",
    "mcp",
    "memory",
    "model",
    "permissions",
    "plugin",
    "resume",
    "statusline",
    "terminal-setup",
    "theme",
    "vim",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct SettingsView {
    /// The offered models, then a reported model the provider does not
    /// list.
    pub models: Vec<ModelChoice>,
    /// The current model's efforts, then a reported effort outside them.
    pub efforts: Vec<EffortChoice>,
    /// The offered permissions, then a reported one the agent does not
    /// offer.
    pub permissions: Vec<PermissionChoice>,
    /// The offered modes, then a reported one the agent does not offer;
    /// empty for an agent without modes (Claude).
    pub modes: Vec<ModeChoice>,
    /// The permission changes by cycling to the next (the cycle key),
    /// never by a pick.
    pub cycle_permission: bool,
    pub commands: Vec<CommandView>,
    /// Why the model cannot change from here, when it cannot.
    pub model_refusal: Option<String>,
    pub effort_refusal: Option<String>,
    pub permission_refusal: Option<String>,
    /// How a person changes the model and effort of a kind that offers no
    /// pick: by typing the agent's own command in the composer.
    pub change_by_typing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ModelChoice {
    /// What a model input names.
    pub value: String,
    pub display_name: String,
    pub description: String,
    /// The efforts it takes, in the provider's order.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub current: bool,
    /// The agent reports it but the provider does not offer it.
    pub reported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffortChoice {
    pub value: String,
    pub current: bool,
    /// What the model runs at when none is chosen.
    pub default: bool,
    pub reported: bool,
}

/// A permission: how much the agent may do without asking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PermissionChoice {
    /// What a permission input names. Empty for Codex settings that match
    /// no named permission, which read as custom.
    pub value: String,
    /// The catalogue's name for it; empty for a reported one.
    pub display_name: String,
    pub current: bool,
    /// The agent reports it but does not offer it.
    pub reported: bool,
    /// The agent's ordinary one, which a client may leave unsaid.
    pub normal: bool,
    /// Under it the agent acts without asking first.
    pub never_asks: bool,
    /// A pick sets it now: the agent offers it to be set and the running
    /// model takes it.
    pub settable: bool,
}

/// A mode: how the agent works (Codex's default or plan).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModeChoice {
    /// What a mode input names.
    pub value: String,
    /// The catalogue's name for it; empty for a reported one.
    pub display_name: String,
    pub current: bool,
    pub reported: bool,
    pub normal: bool,
    pub settable: bool,
}

/// A person's pick on the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SettingChange {
    Model(String),
    Effort(String),
    /// A permission by its value.
    Permission(String),
    /// A mode by its value.
    Mode(String),
    /// The next permission in the agent's own cycle.
    CyclePermission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommandView {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// A plugin's namespace or the provider's scope word; empty when the
    /// provider does not say.
    pub source: String,
}

/// Terminal Claude lists no models or efforts and takes no model or effort
/// input; it runs its own commands, typed as any prompt.
const CLAUDE_PTY_TYPING: &str =
    "To change the model or effort, type /model <name> or /effort <level> in the composer.";

/// Why a setting cannot change from a client, by kind: only where the
/// interpreter refuses that input.
fn refusals(kind: Kind) -> [Option<&'static str>; 3] {
    match kind {
        Kind::ClaudePty => [
            None,
            None,
            Some("Terminal Claude changes permission only by cycling through its permissions."),
        ],
        Kind::ClaudeSdk | Kind::Codex | Kind::Unspecified => [None, None, None],
    }
}

/// The effort the agent runs at: the one it reports, else its current
/// model's default. Codex reports none until one is chosen, which means
/// the model's default, and its model list says what that is.
pub fn effort_in_force(agent: &ui_state::AgentState) -> Option<String> {
    if agent.effort.is_some() {
        return agent.effort.clone();
    }
    let current = agent.model.as_deref()?;
    let offered = &agent.models;
    offered
        .iter()
        .find(|model| model.value == current)
        .or_else(|| offered.iter().find(|model| model.resolved_model == current))
        .and_then(|model| model.default_effort.clone())
}

pub fn settings(state: &SessionState) -> SettingsView {
    let kind = state.kind();
    let agent = state.agent_state();
    let current_model = agent.model.as_deref();
    // The agent reports a model by its id; the offer names it by an alias
    // that resolves to that id, and several aliases may: the one named
    // exactly wins, else the first that resolves to it.
    let current = current_model.and_then(|current| {
        let offered = &agent.models;
        offered
            .iter()
            .position(|model| model.value == current)
            .or_else(|| {
                offered
                    .iter()
                    .position(|model| model.resolved_model == current)
            })
    });
    let mut models: Vec<ModelChoice> = agent
        .models
        .iter()
        .enumerate()
        .map(|(index, model)| ModelChoice {
            value: model.value.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            efforts: model.efforts.clone(),
            default_effort: model.default_effort.clone(),
            current: current == Some(index),
            reported: false,
        })
        .collect();
    if let Some(value) = current_model
        && !models.iter().any(|model| model.current)
    {
        models.push(ModelChoice {
            value: value.to_owned(),
            display_name: String::new(),
            description: String::new(),
            efforts: Vec::new(),
            default_effort: None,
            current: true,
            reported: true,
        });
    }
    let (offered, default) = models
        .iter()
        .find(|model| model.current)
        .map(|model| (model.efforts.as_slice(), model.default_effort.as_deref()))
        .unwrap_or_default();
    let in_force = effort_in_force(agent);
    let current_effort = in_force.as_deref();
    let mut efforts: Vec<EffortChoice> = offered
        .iter()
        .map(|effort| EffortChoice {
            value: effort.clone(),
            current: current_effort == Some(effort.as_str()),
            default: default == Some(effort.as_str()),
            reported: false,
        })
        .collect();
    if let Some(value) = current_effort
        && !efforts.iter().any(|effort| effort.current)
    {
        efforts.push(EffortChoice {
            value: value.to_owned(),
            current: true,
            default: false,
            reported: true,
        });
    }
    let current_model = models.iter().find(|model| model.current);
    let permissions = permissions(kind, agent, current_model);
    let modes = modes(agent);
    let terminal_only =
        |name: &str| kind == Kind::ClaudeSdk && TERMINAL_ONLY_COMMANDS.contains(&name);
    let commands = agent
        .commands
        .iter()
        .filter(|command| !terminal_only(&command.name))
        .map(|command| CommandView {
            name: command.name.clone(),
            description: command.description.clone(),
            argument_hint: command.argument_hint.clone(),
            source: command.source.clone(),
        })
        .collect();
    let [model_refusal, effort_refusal, permission_refusal] =
        refusals(kind).map(|refusal| refusal.map(str::to_owned));
    SettingsView {
        models,
        efforts,
        permissions,
        modes,
        cycle_permission: kind == Kind::ClaudePty,
        commands,
        model_refusal,
        effort_refusal,
        permission_refusal,
        change_by_typing: (kind == Kind::ClaudePty).then(|| CLAUDE_PTY_TYPING.to_owned()),
    }
}

/// The input a pick sends, in the kind's arm; None where the view offers
/// no such pick (it says why instead). The id is the sender's to fill.
pub fn setting_input(kind: Kind, change: &SettingChange) -> Option<wire::Input> {
    use wire::{
        ClaudePtyInput, ClaudeSdkInput, CodexInput, KeyName, SetEffort, SetMode, SetModel,
        SetPermission, claude_pty_input, claude_sdk_input, codex_input, input,
    };
    let model = |model: &String| SetModel {
        model: Some(model.clone()),
    };
    let of = match (kind, change) {
        (Kind::ClaudePty, SettingChange::CyclePermission) => input::Of::ClaudePty(ClaudePtyInput {
            of: Some(claude_pty_input::Of::Key(wire::Key {
                key: KeyName::CyclePermissionMode.into(),
            })),
        }),
        (Kind::ClaudeSdk, SettingChange::Model(name)) => input::Of::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Model(model(name))),
        }),
        (Kind::ClaudeSdk, SettingChange::Effort(effort)) => input::Of::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Effort(SetEffort {
                effort: Some(effort.clone()),
            })),
        }),
        (Kind::ClaudeSdk, SettingChange::Permission(value)) if !value.is_empty() => {
            input::Of::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Permission(SetPermission {
                    value: value.clone(),
                })),
            })
        }
        (Kind::Codex, SettingChange::Model(name)) => input::Of::Codex(CodexInput {
            of: Some(codex_input::Of::Model(model(name))),
        }),
        (Kind::Codex, SettingChange::Effort(effort)) => input::Of::Codex(CodexInput {
            of: Some(codex_input::Of::Effort(SetEffort {
                effort: Some(effort.clone()),
            })),
        }),
        // Settings that match no named permission have no value to send.
        (Kind::Codex, SettingChange::Permission(value)) if !value.is_empty() => {
            input::Of::Codex(CodexInput {
                of: Some(codex_input::Of::Permission(SetPermission {
                    value: value.clone(),
                })),
            })
        }
        (Kind::Codex, SettingChange::Mode(value)) if !value.is_empty() => {
            input::Of::Codex(CodexInput {
                of: Some(codex_input::Of::Mode(SetMode {
                    value: value.clone(),
                })),
            })
        }
        _ => return None,
    };
    Some(wire::Input {
        input_id: Vec::new(),
        of: Some(of),
    })
}

/// The catalogue's permissions with the reported one marked; a reported
/// permission the catalogue lacks is added after them. Codex settings that
/// match no named permission are reported with no value. A permission that
/// names its models is settable only while one of them runs.
fn permissions(
    kind: Kind,
    agent: &ui_state::AgentState,
    model: Option<&ModelChoice>,
) -> Vec<PermissionChoice> {
    let reported = match kind {
        Kind::Codex if agent.permission.is_none() && agent.approval_policy.is_some() => Some(""),
        _ => agent.permission.as_deref(),
    };
    let model_takes = |models: &[String]| {
        models.is_empty() || model.is_some_and(|model| models.contains(&model.value))
    };
    let mut permissions: Vec<PermissionChoice> = agent
        .permissions
        .iter()
        .map(|offered| PermissionChoice {
            value: offered.value.clone(),
            display_name: offered.display_name.clone(),
            current: reported == Some(offered.value.as_str()),
            reported: false,
            normal: offered.normal,
            never_asks: offered.never_asks,
            settable: offered.settable && model_takes(&offered.models),
        })
        .collect();
    if let Some(value) = reported
        && !permissions.iter().any(|permission| permission.current)
    {
        permissions.push(PermissionChoice {
            value: value.to_owned(),
            display_name: String::new(),
            current: true,
            reported: true,
            normal: false,
            never_asks: false,
            settable: false,
        });
    }
    permissions
}

/// The catalogue's modes with the reported one marked, a reported mode the
/// catalogue lacks added after them.
fn modes(agent: &ui_state::AgentState) -> Vec<ModeChoice> {
    let reported = agent.mode.as_deref();
    let mut modes: Vec<ModeChoice> = agent
        .modes
        .iter()
        .map(|offered| ModeChoice {
            value: offered.value.clone(),
            display_name: offered.display_name.clone(),
            current: reported == Some(offered.value.as_str()),
            reported: false,
            normal: offered.normal,
            settable: offered.settable,
        })
        .collect();
    if let Some(value) = reported
        && !modes.iter().any(|mode| mode.current)
    {
        modes.push(ModeChoice {
            value: value.to_owned(),
            display_name: String::new(),
            current: true,
            reported: true,
            normal: false,
            settable: false,
        });
    }
    modes
}
