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
/// The environment variable naming a file a stdio fake appends every line
/// its host writes to, so a test can see exactly what the provider got.
pub const INPUT_LOG_ENV: &str = "AMUX_FAKE_INPUT_LOG";

/// Append one host line to the input log, when the environment names one.
pub fn log_input(line: &str) {
    use std::io::Write as _;

    let Some(path) = std::env::var_os(INPUT_LOG_ENV) else {
        return;
    };
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = appended {
        eprintln!("input log {}: {error}", Path::new(&path).display());
    }
}

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
    /// Play `steps` this many times in a row, as if written out: a flood
    /// of messages without a script the size of the flood.
    Repeat { times: usize, steps: Vec<Step> },
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
        check_steps(&self.steps, provider, raises)
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

fn check_steps(steps: &[Step], provider: &'static str, raises: &[&str]) -> Result<(), ScriptError> {
    for step in steps {
        match step {
            Step::Ask(ask) if !raises.contains(&ask.kind()) => {
                return Err(ScriptError::Unsupported {
                    provider,
                    ask: ask.kind(),
                });
            }
            Step::Repeat { steps, .. } => check_steps(steps, provider, raises)?,
            _ => {}
        }
    }
    Ok(())
}

/// The next step to play, with repeats unrolled one pass at a time so a
/// long repeat never sits in memory written out. Never returns a
/// [`Step::Repeat`].
pub fn next_step(steps: &mut std::collections::VecDeque<Step>) -> Option<Step> {
    loop {
        match steps.pop_front()? {
            Step::Repeat { times, steps: body } => {
                if times == 0 || body.is_empty() {
                    continue;
                }
                if times > 1 {
                    steps.push_front(Step::Repeat {
                        times: times - 1,
                        steps: body.clone(),
                    });
                }
                for step in body.into_iter().rev() {
                    steps.push_front(step);
                }
            }
            step => return Some(step),
        }
    }
}

/// Where a scripted pause stands: polled rather than watched so it works the
/// same on every platform and filesystem.
pub async fn wait_for(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod repeat_tests {
    use std::collections::VecDeque;

    use super::{Step, next_step};

    fn text(text: &str) -> Step {
        Step::Text {
            chunks: vec![text.to_owned()],
        }
    }

    #[test]
    fn a_repeat_plays_as_if_written_out_and_parses_from_json() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"repeat": {"times": 3, "steps": [{"text": {"chunks": ["a"]}}, {"repeat": {"times": 2, "steps": [{"text": {"chunks": ["b"]}}]}}]}}, "turn_end"]"#,
        )
        .unwrap();
        let mut queue: VecDeque<Step> = steps.into();
        let mut played = Vec::new();
        while let Some(step) = next_step(&mut queue) {
            played.push(step);
        }
        let mut expected = Vec::new();
        for _ in 0..3 {
            expected.extend([text("a"), text("b"), text("b")]);
        }
        expected.push(Step::TurnEnd);
        assert_eq!(played, expected);
    }
}
