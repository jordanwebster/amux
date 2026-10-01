//! The settings view: what the agent offers to change (model, effort, mode)
//! with the current value of each marked, the commands it offers, and, per
//! kind, the sentence that says why a setting cannot change from here.
//!
//! The models, their efforts and the commands come from the provider through
//! the snapshot; a client never supplies a catalogue of its own. The mode
//! choices are the provider's closed set, so they are held here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui_state::SessionState;
use wire::Kind;

/// Claude's permission modes, in the order its own interface cycles them.
const CLAUDE_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "plan",
    "auto",
    "bypassPermissions",
];

/// The Claude mode under which it never asks before acting.
const CLAUDE_STOPS_ASKING: &str = "bypassPermissions";

/// Codex's presets: name, approval policy, sandbox. "plan" stands in for
/// Codex's Plan collaboration mode, which amux cannot set yet: no real
/// Codex takes "plan" as an approval policy, so only the lab offers it.
const CODEX_PRESETS: &[(&str, &str, &str)] = &[
    ("read-only", "on-request", "read-only"),
    ("auto", "on-request", "workspace-write"),
    ("plan", "plan", "read-only"),
    ("full-access", "never", "danger-full-access"),
];

/// A Codex preset by name, as a mode a pick sends.
pub(crate) fn codex_preset(name: &str) -> Option<ModeValue> {
    CODEX_PRESETS
        .iter()
        .find(|(preset, _, _)| *preset == name)
        .map(|(preset, approval, sandbox)| ModeValue::Codex {
            preset: Some((*preset).to_owned()),
            approval_policy: (*approval).to_owned(),
            sandbox: (*sandbox).to_owned(),
        })
}

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
    /// The modes to pick from; for a kind that only cycles, the current
    /// mode alone.
    pub modes: Vec<ModeChoice>,
    /// The mode changes by cycling to the next (the cycle key), never by a
    /// pick: offered beside the current mode.
    pub cycle_mode: bool,
    pub commands: Vec<CommandView>,
    /// Why the model cannot change from here, when it cannot.
    pub model_refusal: Option<String>,
    pub effort_refusal: Option<String>,
    pub mode_refusal: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModeChoice {
    pub value: ModeValue,
    pub current: bool,
    pub reported: bool,
    /// Under this mode the agent acts without asking first.
    pub stops_asking: bool,
}

/// What a mode input sets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ModeValue {
    /// Claude's permission mode.
    Claude(String),
    /// Codex's approval policy and sandbox; `preset` names the preset the
    /// pair is, None for a reported pair outside the presets.
    Codex {
        preset: Option<String>,
        approval_policy: String,
        sandbox: String,
    },
}

/// A person's pick on the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SettingChange {
    Model(String),
    Effort(String),
    Mode(ModeValue),
    /// The next mode in the agent's own cycle.
    CycleMode,
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
            Some("Terminal Claude changes mode only by cycling through its modes."),
        ],
        Kind::ClaudeSdk => [
            None,
            Some("Claude takes its effort when the agent starts and keeps it until it restarts."),
            None,
        ],
        Kind::Codex | Kind::Unspecified => [None, None, None],
    }
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
    let current_effort = agent.effort.as_deref();
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
    let modes = modes(kind, agent.mode.as_deref(), agent.sandbox.as_deref());
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
    let [model_refusal, effort_refusal, mode_refusal] =
        refusals(kind).map(|refusal| refusal.map(str::to_owned));
    SettingsView {
        models,
        efforts,
        modes,
        cycle_mode: kind == Kind::ClaudePty,
        commands,
        model_refusal,
        effort_refusal,
        mode_refusal,
        change_by_typing: (kind == Kind::ClaudePty).then(|| CLAUDE_PTY_TYPING.to_owned()),
    }
}

/// The input a pick sends, in the kind's arm; None where the view offers
/// no such pick (it says why instead). The id is the sender's to fill.
pub fn setting_input(kind: Kind, change: &SettingChange) -> Option<wire::Input> {
    use wire::{
        ClaudePtyInput, ClaudeSdkInput, CodexInput, KeyName, SetApproval, SetEffort, SetModel,
        SetPermissionMode, claude_pty_input, claude_sdk_input, codex_input, input,
    };
    let model = |model: &String| SetModel {
        model: Some(model.clone()),
    };
    let of = match (kind, change) {
        (Kind::ClaudePty, SettingChange::CycleMode) => input::Of::ClaudePty(ClaudePtyInput {
            of: Some(claude_pty_input::Of::Key(wire::Key {
                key: KeyName::CyclePermissionMode.into(),
            })),
        }),
        (Kind::ClaudeSdk, SettingChange::Model(name)) => input::Of::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Model(model(name))),
        }),
        (Kind::ClaudeSdk, SettingChange::Mode(ModeValue::Claude(mode))) => {
            input::Of::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Mode(SetPermissionMode {
                    mode: mode.clone(),
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
        (
            Kind::Codex,
            SettingChange::Mode(ModeValue::Codex {
                approval_policy,
                sandbox,
                ..
            }),
        ) => input::Of::Codex(CodexInput {
            of: Some(codex_input::Of::Approval(SetApproval {
                approval_policy: approval_policy.clone(),
                sandbox: sandbox.clone(),
            })),
        }),
        _ => return None,
    };
    Some(wire::Input {
        input_id: Vec::new(),
        of: Some(of),
    })
}

/// The kind's mode set with the reported mode marked; a reported mode
/// outside the set is added after it. Terminal Claude's is the reported
/// mode alone: the order its cycle takes depends on how it was launched,
/// so no other mode can be reached by a known number of steps.
fn modes(kind: Kind, mode: Option<&str>, sandbox: Option<&str>) -> Vec<ModeChoice> {
    match kind {
        Kind::ClaudePty => mode
            .map(|mode| ModeChoice {
                value: ModeValue::Claude(mode.to_owned()),
                current: true,
                reported: !CLAUDE_MODES.contains(&mode),
                stops_asking: mode == CLAUDE_STOPS_ASKING,
            })
            .into_iter()
            .collect(),
        Kind::ClaudeSdk => {
            let mut modes: Vec<ModeChoice> = CLAUDE_MODES
                .iter()
                .map(|name| ModeChoice {
                    value: ModeValue::Claude((*name).to_owned()),
                    current: mode == Some(*name),
                    reported: false,
                    stops_asking: *name == CLAUDE_STOPS_ASKING,
                })
                .collect();
            if let Some(mode) = mode
                && !CLAUDE_MODES.contains(&mode)
            {
                modes.push(ModeChoice {
                    value: ModeValue::Claude(mode.to_owned()),
                    current: true,
                    reported: true,
                    stops_asking: false,
                });
            }
            modes
        }
        Kind::Codex => {
            let reported = mode.zip(sandbox);
            let mut modes: Vec<ModeChoice> = CODEX_PRESETS
                .iter()
                .map(|(preset, approval, sandbox)| ModeChoice {
                    value: ModeValue::Codex {
                        preset: Some((*preset).to_owned()),
                        approval_policy: (*approval).to_owned(),
                        sandbox: (*sandbox).to_owned(),
                    },
                    current: reported == Some((*approval, *sandbox)),
                    reported: false,
                    stops_asking: *approval == "never",
                })
                .collect();
            if let Some((approval, sandbox)) = reported
                && !modes.iter().any(|mode| mode.current)
            {
                modes.push(ModeChoice {
                    value: ModeValue::Codex {
                        preset: None,
                        approval_policy: approval.to_owned(),
                        sandbox: sandbox.to_owned(),
                    },
                    current: true,
                    reported: true,
                    stops_asking: approval == "never",
                });
            }
            modes
        }
        Kind::Unspecified => Vec::new(),
    }
}
