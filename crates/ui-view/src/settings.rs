//! The settings view: what the agent offers to change (model, effort, mode)
//! with the current value of each marked, the commands it offers, and, per
//! kind, the sentence that says why a setting cannot change from here.
//!
//! The models, their efforts and the commands come from the provider through
//! the snapshot; a client never supplies a catalogue of its own. The mode
//! choices are the provider's closed set, so they are held here.

use schemars::JsonSchema;
use serde::Serialize;
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

/// Codex's presets: name, approval policy, sandbox.
const CODEX_PRESETS: &[(&str, &str, &str)] = &[
    ("read-only", "on-request", "read-only"),
    ("auto", "on-request", "workspace-write"),
    ("full-access", "never", "danger-full-access"),
];

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
    pub modes: Vec<ModeChoice>,
    pub commands: Vec<CommandView>,
    /// Why the model cannot change from here, when it cannot.
    pub model_refusal: Option<String>,
    pub effort_refusal: Option<String>,
    pub mode_refusal: Option<String>,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommandView {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// A plugin's namespace or the provider's scope word; empty when the
    /// provider does not say.
    pub source: String,
}

/// Why a setting cannot change from a client, by kind: only where the
/// interpreter refuses that input.
fn refusals(kind: Kind) -> [Option<&'static str>; 3] {
    match kind {
        Kind::ClaudePty => [
            Some("Terminal Claude changes its model only in its own terminal."),
            Some("Terminal Claude changes its effort only in its own terminal."),
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
    let mut models: Vec<ModelChoice> = agent
        .models
        .iter()
        .map(|model| ModelChoice {
            value: model.value.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            efforts: model.efforts.clone(),
            default_effort: model.default_effort.clone(),
            current: current_model == Some(model.value.as_str()),
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
    let terminal_only = |name: &str| {
        matches!(kind, Kind::ClaudePty | Kind::ClaudeSdk) && TERMINAL_ONLY_COMMANDS.contains(&name)
    };
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
        commands,
        model_refusal,
        effort_refusal,
        mode_refusal,
    }
}

/// The kind's mode set with the reported mode marked; a reported mode
/// outside the set is added after it.
fn modes(kind: Kind, mode: Option<&str>, sandbox: Option<&str>) -> Vec<ModeChoice> {
    match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => {
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
