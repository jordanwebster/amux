//! Passive session facts for the Claude SDK output plane.

use std::collections::HashMap;

use claude::sdk::{Message, PermissionMode, ResultMessage};

use super::sdk_io::{ClaudeSdkSynthesized, ContextMeter, ContextMeterSource, McpServerFact};

#[derive(Default)]
pub(super) struct SessionFacts {
    pub model: Option<String>,
    pub launch_model: Option<String>,
    pub effort: Option<String>,
    pub slash_commands: Vec<String>,
    terminal_slash_commands: Vec<String>,
    pub permission_mode: Option<String>,
    pub bypass_granted: bool,
    context: Option<ContextMeter>,
    context_model: Option<String>,
    windows: HashMap<String, u64>,
    mcp_servers: Vec<McpServerFact>,
}

impl SessionFacts {
    pub fn initialize_commands(&mut self, commands: &[claude::sdk::init::SlashCommand]) {
        self.slash_commands = commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
    }

    pub fn from_args(args: &[String]) -> Self {
        let value = |flag: &str| {
            args.iter().enumerate().find_map(|(index, arg)| {
                arg.strip_prefix(&format!("{flag}="))
                    .map(str::to_owned)
                    .or_else(|| {
                        (arg == flag)
                            .then(|| args.get(index + 1).cloned())
                            .flatten()
                    })
            })
        };
        let launch_model = value("--model");
        let skip = args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions");
        Self {
            model: launch_model.clone(),
            launch_model,
            effort: value("--effort"),
            permission_mode: if skip {
                Some("bypassPermissions".into())
            } else {
                value("--permission-mode").or_else(|| Some("default".into()))
            },
            bypass_granted: skip
                || args
                    .iter()
                    .any(|arg| arg == "--allow-dangerously-skip-permissions"),
            ..Self::default()
        }
    }

    pub fn check_mode(&self, mode: &PermissionMode) -> anyhow::Result<()> {
        if *mode == PermissionMode::BypassPermissions && !self.bypass_granted {
            anyhow::bail!("bypass permissions was not granted at launch");
        }
        Ok(())
    }

    pub fn row(&self) -> ClaudeSdkSynthesized {
        ClaudeSdkSynthesized::SessionFacts {
            model: self.model.clone(),
            effort: self.effort.clone(),
            slash_commands: self.slash_commands.clone(),
            terminal_slash_commands: self.terminal_slash_commands.clone(),
            permission_mode: self.permission_mode.clone(),
            context: self.context.clone(),
            mcp_servers: self.mcp_servers.clone(),
        }
    }

    /// Returns whether this message should be followed by a facts snapshot.
    pub fn observe(&mut self, message: &Message) -> bool {
        match message {
            Message::System(init) => {
                if self.launch_model.is_none() {
                    self.launch_model = Some(init.model.clone());
                }
                self.model = Some(init.model.clone());
                self.terminal_slash_commands =
                    init.terminal_slash_commands.clone().unwrap_or_default();
                if let Some(effort) = &init.effort {
                    self.effort = effort.clone();
                }
                self.permission_mode = Some(init.permission_mode.as_str().into());
                self.mcp_servers = init
                    .mcp_servers
                    .iter()
                    .map(|server| McpServerFact {
                        name: server.name.clone(),
                        status: server.status.clone(),
                    })
                    .collect();
            }
            Message::Status(status) => {
                let Some(mode) = &status.permission_mode else {
                    return false;
                };
                self.permission_mode = Some(mode.as_str().into());
            }
            Message::Assistant(assistant) => {
                // Child calls have their own context; they must not replace the parent's meter.
                if assistant.parent_tool_use_id.is_none() {
                    let message = &assistant.message;
                    self.model = Some(message.model.clone());
                    self.context_model = Some(message.model.clone());
                    let usage = &message.usage;
                    self.context = Some(ContextMeter {
                        used_tokens: usage
                            .input_tokens
                            .saturating_add(usage.cache_read_input_tokens.unwrap_or(0))
                            .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0)),
                        window_tokens: self.windows.get(&message.model).copied(),
                        source: ContextMeterSource::AssistantUsage,
                    });
                }
            }
            Message::CompactBoundary(boundary) => {
                self.context_model = boundary
                    .compact_metadata
                    .post_tokens
                    .and(self.model.clone());
                self.context =
                    boundary
                        .compact_metadata
                        .post_tokens
                        .map(|used_tokens| ContextMeter {
                            used_tokens,
                            window_tokens: self
                                .context_model
                                .as_ref()
                                .and_then(|model| self.windows.get(model))
                                .copied(),
                            source: ContextMeterSource::CompactBoundary,
                        });
            }
            Message::ConversationReset(_) => {
                self.context = None;
                self.context_model = None;
            }
            Message::Result(result) => {
                let common = match result {
                    ResultMessage::Success(result) => &result.common,
                    ResultMessage::ErrorDuringExecution(result)
                    | ResultMessage::ErrorMaxTurns(result)
                    | ResultMessage::ErrorMaxBudgetUsd(result)
                    | ResultMessage::ErrorMaxStructuredOutputRetries(result) => &result.common,
                    ResultMessage::Unknown(_) => return true,
                };
                for (model, usage) in &common.model_usage {
                    self.windows.insert(model.clone(), usage.context_window);
                    if let Some(canonical) = &usage.canonical_model {
                        self.windows.insert(canonical.clone(), usage.context_window);
                    }
                }
                // Result token totals can include multiple calls and subagents. Only their
                // model's window belongs in the meter for the latest assistant call.
                if let Some(context) = &mut self.context {
                    context.window_tokens = self
                        .context_model
                        .as_ref()
                        .and_then(|model| self.windows.get(model))
                        .copied();
                    context.source = ContextMeterSource::ResultUsage;
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn claude_sdk_facts_publish_initialized_commands_before_any_turn() {
        let reply: claude::sdk::InitializationResult = serde_json::from_value(json!({
            "commands": [
                {"name": "compact", "description": "Compact context", "argumentHint": "[instructions]"},
                {"name": "tools:review", "description": "Review changes", "argumentHint": ""}
            ],
            "agents": [], "models": [], "account": {},
            "output_style": "default", "available_output_styles": []
        })).unwrap();
        let mut facts = SessionFacts::from_args(&["--effort=low".into()]);
        facts.initialize_commands(&reply.commands);
        let row = serde_json::to_value(facts.row()).unwrap();
        assert_eq!(row["slash_commands"], json!(["compact", "tools:review"]));
        assert_eq!(row["effort"], "low");
        let observed = Message::parse(json!({
            "type": "system", "subtype": "init", "uuid": uuid::Uuid::nil(),
            "session_id": "session", "apiKeySource": "none", "claude_code_version": "2.1.247",
            "cwd": "/", "tools": [], "mcp_servers": [], "model": "model",
            "permissionMode": "default", "slash_commands": [], "output_style": "default",
            "terminal_slash_commands": ["tools:review"],
            "skills": [], "plugins": [], "effort": "medium"
        }))
        .unwrap();
        assert!(facts.observe(&observed));
        let row = serde_json::to_value(facts.row()).unwrap();
        assert_eq!(row["effort"], "medium");
        assert_eq!(row["slash_commands"], json!(["compact", "tools:review"]));
        assert_eq!(row["terminal_slash_commands"], json!(["tools:review"]));
        let mut raw = serde_json::to_value(&observed).unwrap();
        raw.as_object_mut().unwrap().remove("effort");
        facts.observe(&Message::parse(raw.clone()).unwrap());
        assert_eq!(facts.effort.as_deref(), Some("medium"));
        raw["effort"] = json!(null);
        let cleared = Message::parse(raw).unwrap();
        assert_eq!(
            serde_json::to_value(&cleared).unwrap()["effort"],
            json!(null)
        );
        facts.observe(&cleared);
        assert!(facts.effort.is_none());
    }

    #[test]
    fn claude_sdk_bypass_requires_an_explicit_launch_grant() {
        for args in [
            vec![],
            vec!["--permission-mode", "bypassPermissions"],
            vec!["--model", "bypassPermissions"],
        ] {
            let facts =
                SessionFacts::from_args(&args.into_iter().map(String::from).collect::<Vec<_>>());
            assert!(
                facts
                    .check_mode(&PermissionMode::BypassPermissions)
                    .is_err()
            );
            assert!(facts.check_mode(&PermissionMode::AcceptEdits).is_ok());
        }
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
        ] {
            let facts = SessionFacts::from_args(&[flag.into()]);
            assert!(facts.check_mode(&PermissionMode::BypassPermissions).is_ok());
        }
    }

    #[test]
    fn claude_sdk_context_resets_across_compaction_and_conversation_reset() {
        let assistant = Message::parse(json!({"type": "assistant", "uuid": uuid::Uuid::nil(),
            "session_id": "session", "parent_tool_use_id": null,
            "message": {"type": "message", "id": "m", "role": "assistant", "model": "parent",
                "content": [], "usage": {"input_tokens": 28483, "output_tokens": 0}}}))
        .unwrap();
        let reset = Message::parse(json!({"type": "conversation_reset",
            "uuid": uuid::Uuid::nil(), "session_id": "session",
            "new_conversation_id": uuid::Uuid::nil()}))
        .unwrap();
        let mut facts = SessionFacts::default();
        facts.windows.insert("parent".into(), 200000);

        for post_tokens in [Some(2963), Some(0), None] {
            assert!(facts.observe(&assistant));
            assert_eq!(facts.context.as_ref().unwrap().used_tokens, 28483);
            let mut metadata = json!({"trigger": "manual", "pre_tokens": 28483});
            if let Some(tokens) = post_tokens {
                metadata["post_tokens"] = json!(tokens);
            }
            let compact = Message::parse(json!({"type": "system", "subtype": "compact_boundary",
                "uuid": uuid::Uuid::nil(), "session_id": "session", "compact_metadata": metadata}))
            .unwrap();
            assert!(facts.observe(&compact));
            let row = serde_json::to_value(facts.row()).unwrap();
            assert_eq!(
                row["context"],
                post_tokens.map_or(json!(null), |tokens| json!({
                    "used_tokens": tokens, "window_tokens": 200000, "source": "compact_boundary"
                }))
            );
            assert!(facts.observe(&reset));
            assert!(facts.context_model.is_none());
            assert_eq!(
                serde_json::to_value(facts.row()).unwrap()["context"],
                json!(null)
            );
        }

        assert!(facts.observe(&assistant));
        assert_eq!(facts.context.as_ref().unwrap().window_tokens, Some(200000));
        assert!(facts.observe(&reset));
        assert!(facts.context.is_none());
    }

    #[test]
    fn claude_sdk_context_ignores_child_usage_and_unmatched_model_windows() {
        let mut facts = SessionFacts::default();
        let mut assistant = json!({"type": "assistant", "uuid": uuid::Uuid::nil(),
            "session_id": "session", "parent_tool_use_id": null,
            "message": {"type": "message", "id": "m", "role": "assistant", "model": "parent",
                "content": [], "usage": {"input_tokens": 12, "output_tokens": 999}}});
        facts.observe(&Message::parse(assistant.clone()).unwrap());
        assert_eq!(facts.context.as_ref().unwrap().used_tokens, 12);
        assert_eq!(facts.context.as_ref().unwrap().window_tokens, None);
        assistant["parent_tool_use_id"] = json!("child");
        assistant["message"]["model"] = json!("child-model");
        assistant["message"]["usage"]["input_tokens"] = json!(100000);
        facts.observe(&Message::parse(assistant.clone()).unwrap());
        assert_eq!(facts.model.as_deref(), Some("parent"));
        assert_eq!(facts.context.as_ref().unwrap().used_tokens, 12);
        facts.windows.insert("parent".into(), 200000);
        assistant["parent_tool_use_id"] = json!(null);
        facts.observe(&Message::parse(assistant).unwrap());
        assert_eq!(
            facts.context.as_ref().unwrap().window_tokens,
            None,
            "a newly selected model must not inherit a different model's window"
        );
    }
}
