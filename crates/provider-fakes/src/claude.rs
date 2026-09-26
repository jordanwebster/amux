//! What both Claude fakes share: the launch flags they honour, ids and
//! timestamps in Claude's formats.

use std::path::PathBuf;

use serde_json::Value;

/// The launch arguments the fake honours; everything else is accepted and
/// ignored, as a provider ignores flags a host passes for other versions.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub settings: Vec<Value>,
    pub session_id: Option<String>,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub messaging_socket: Option<PathBuf>,
    pub permission_mode: Option<String>,
    pub replay_user_messages: bool,
    pub include_partial_messages: bool,
}

impl Args {
    pub fn parse(args: &[String]) -> Self {
        let mut parsed = Args::default();
        let mut index = 0;
        while index < args.len() {
            let (flag, inline) = match args[index].split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                _ => (args[index].as_str(), None),
            };
            let mut value = || {
                inline.clone().or_else(|| {
                    index += 1;
                    args.get(index).cloned()
                })
            };
            match flag {
                "--settings" => {
                    if let Some(source) = value() {
                        let settings = serde_json::from_str(&source).or_else(|_| {
                            std::fs::read(&source)
                                .ok()
                                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                                .ok_or(())
                        });
                        if let Ok(settings) = settings {
                            parsed.settings.push(settings);
                        }
                    }
                }
                "--session-id" => parsed.session_id = value(),
                "--resume" | "-r" => parsed.resume = value(),
                "--model" => parsed.model = value(),
                "--messaging-socket-path" => parsed.messaging_socket = value().map(PathBuf::from),
                "--permission-mode" => parsed.permission_mode = value(),
                "--replay-user-messages" => parsed.replay_user_messages = true,
                "--include-partial-messages" => parsed.include_partial_messages = true,
                _ => {}
            }
            index += 1;
        }
        parsed
    }

    /// Every hook command registered for `event`, in settings order.
    pub fn hook_commands(&self, event: &str) -> Vec<String> {
        self.settings
            .iter()
            .filter_map(|settings| settings.get("hooks")?.get(event)?.as_array())
            .flatten()
            .filter_map(|matcher| matcher.get("hooks")?.as_array())
            .flatten()
            .filter_map(|hook| hook.get("command")?.as_str().map(str::to_owned))
            .collect()
    }
}

/// A timestamp as Claude writes one: UTC, milliseconds, `Z`.
pub fn timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Ids in the provider's shapes, numbered so a run is easy to read.
#[derive(Default)]
pub struct Ids {
    next: u64,
}

impl Ids {
    pub fn next(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{:024}", self.next)
    }
}

/// The Claude Code version the fakes report: the newest one the corpora
/// were recorded against.
pub const VERSION: &str = "2.1.283";
