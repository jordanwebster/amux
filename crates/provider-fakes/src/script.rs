//! The authored script a fake provider plays.
//!
//! A script is the model's side of a session: what it says, which tools it
//! calls, what it asks, and when it stops. How the provider reacts to the
//! host — handshakes, echoes of what was sent, queueing and folding of
//! messages written mid-turn, answers to control requests — is the fake's
//! protocol engine, not the script, so every script gets the same provider
//! behaviour.
//!
//! Steps run in order. When the fake is idle it waits for the next prompt
//! (or injected message) and then plays steps until a [`Step::TurnEnd`]. An
//! [`Step::Ask`] blocks until the host answers it. A [`Step::WaitFor`]
//! blocks until a file exists, so a test can hold a turn open while it
//! kills a daemon; a [`Step::Pause`] keeps it busy for a while. When the steps run out the fake stays idle until its
//! input closes, then exits 0; [`Step::Exit`] ends the process early.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The environment variable naming the script a fake plays.
pub const SCRIPT_ENV: &str = "AMUX_FAKE_SCRIPT";
/// The environment variable naming a recording a fake plays back verbatim.
pub const PLAYBACK_ENV: &str = "AMUX_FAKE_PLAYBACK";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The model the session reports when the launch names none.
    #[serde(default)]
    pub model: Option<String>,
    /// Slash commands the session offers.
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Streamed assistant text: one message, delivered chunk by chunk where
    /// the provider streams.
    Text { chunks: Vec<String> },
    /// Reasoning before the next step.
    Thinking { text: String },
    /// A tool call that runs without asking.
    Tool(Tool),
    /// Something the host must answer before the turn goes on.
    Ask(Ask),
    /// Hold the turn until this file exists.
    WaitFor { path: PathBuf },
    /// Stay busy for this long, as a model at work does.
    Pause { ms: u64 },
    /// Finish the turn successfully.
    TurnEnd,
    /// Exit the provider process with this code, mid-turn or not.
    Exit { code: i32 },
}

/// A tool call. The class picks a tool of that class when no name is given:
/// exploration reads, consequential runs a command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(default)]
    pub name: Option<String>,
    pub class: ToolClass,
    /// The call's input in the provider's terms; a default for the class
    /// otherwise.
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub outcome: Outcome,
    /// The call runs until this file exists, as a long command does, so a
    /// test can write to the provider while a tool is running.
    #[serde(default)]
    pub wait_for: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Exploration,
    Consequential,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    #[serde(default)]
    pub output: String,
    /// The tool ran and failed.
    #[serde(default)]
    pub error: bool,
}

/// Every kind of ask the providers raise. Not every provider raises every
/// kind; a script asking a provider for one it cannot raise fails to load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Ask {
    /// Permission to make this call; allowed, it runs with its outcome.
    Permission(Tool),
    /// Multiple-choice questions.
    Question { questions: Vec<Question> },
    /// A plan to approve before the work starts.
    Plan { markdown: String },
    /// A tool server's form.
    Form {
        server: String,
        message: String,
        schema: Value,
    },
    /// A tool server's request to open a link.
    Link {
        server: String,
        message: String,
        url: String,
    },
    /// Wider access for the rest of the turn or session (Codex).
    Grant { reason: String, paths: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("reading script {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing script {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{provider} cannot raise a {ask} ask")]
    Unsupported {
        provider: &'static str,
        ask: &'static str,
    },
}

impl Script {
    pub fn load(path: &Path) -> Result<Self, ScriptError> {
        let bytes = std::fs::read(path).map_err(|source| ScriptError::Read {
            path: path.to_owned(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| ScriptError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Refuse asks this provider has no way to raise.
    pub fn check(&self, provider: &'static str, raises: &[&str]) -> Result<(), ScriptError> {
        for step in &self.steps {
            if let Step::Ask(ask) = step
                && !raises.contains(&ask.kind())
            {
                return Err(ScriptError::Unsupported {
                    provider,
                    ask: ask.kind(),
                });
            }
        }
        Ok(())
    }
}

impl Ask {
    pub fn kind(&self) -> &'static str {
        match self {
            Ask::Permission(_) => "permission",
            Ask::Question { .. } => "question",
            Ask::Plan { .. } => "plan",
            Ask::Form { .. } => "form",
            Ask::Link { .. } => "link",
            Ask::Grant { .. } => "grant",
        }
    }
}

impl Tool {
    pub fn is_exploration(&self) -> bool {
        self.class == ToolClass::Exploration
    }
}

/// Where a scripted pause stands: polled rather than watched so it works the
/// same on every platform and filesystem.
pub async fn wait_for(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
