pub mod approval;
pub mod config;
#[cfg(unix)]
pub mod daemon;
pub(crate) mod dispatch;
pub mod error;
pub mod init;
pub mod notification;
pub mod server;
pub mod thread;
pub mod transport;
pub mod turn_stream;
pub mod types;

pub use approval::{ApprovalHandler, ApprovalRequest, ApprovalResponse, AutoApprove, RequestId};
pub use config::{
    ApprovalPolicy, ApprovalsReviewer, CodexConfig, CollaborationMode, CollaborationModeKind,
    CollaborationModeSettings, GranularApprovalPolicy, InputItem, NetworkAccess, Personality,
    ReadOnlyAccess, ReasoningEffort, SandboxMode, SandboxPolicy, SummaryMode, ThreadConfig,
    TurnConfig, TurnInput,
};
#[cfg(unix)]
pub use daemon::{
    DaemonMode, DaemonProcess, connect_daemon, connect_socket, daemon_socket_path, ensure_daemon,
    ensure_daemon_with_fallback,
};
pub use error::Error;
pub use init::InitializationResult;
pub use notification::{ServerNotification, TurnEvent};
pub use server::Codex;
pub use thread::Thread;
pub use turn_stream::TurnStream;
pub use types::*;

/// Spawn a codex app-server subprocess, perform the initialize handshake,
/// and return a ready-to-use `Codex` handle.
pub async fn connect(config: CodexConfig) -> Result<Codex, Error> {
    Codex::connect(config).await
}

/// One-shot convenience: start a thread, run one turn, return the completed turn.
pub async fn prompt(
    config: CodexConfig,
    prompt: &str,
    thread_config: ThreadConfig,
) -> Result<Turn, Error> {
    let codex = connect(config).await?;
    let result = codex.prompt(prompt, thread_config).await;
    codex.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let config = CodexConfig::default();
        assert_eq!(config.client_name, "codex-rust-sdk");
        assert!(config.experimental_api);
        assert!(config.approval_handler.is_none());
        assert!(config.codex_path.is_none());
    }

    #[test]
    fn turn_input_from_str() {
        let input: TurnInput = "hello".into();
        assert!(matches!(input, TurnInput::Text(ref s) if s == "hello"));
    }

    #[test]
    fn turn_input_from_items() {
        let items = vec![
            InputItem::text("hi"),
            InputItem::image("https://example.com/img.png"),
        ];
        let input: TurnInput = items.into();
        assert!(matches!(input, TurnInput::Items(ref v) if v.len() == 2));
    }

    #[test]
    fn approval_response_wire_values() {
        let accept = ApprovalResponse::Accept.to_wire_value();
        assert_eq!(accept["decision"], "accept");

        let decline = ApprovalResponse::Decline.to_wire_value();
        assert_eq!(decline["decision"], "decline");

        let cancel = ApprovalResponse::Cancel.to_wire_value();
        assert_eq!(cancel["decision"], "cancel");
    }

    #[test]
    fn thread_config_serialization() {
        let config = ThreadConfig {
            model: Some("o3-pro".into()),
            ephemeral: Some(true),
            ..Default::default()
        };
        let params = config::thread_config_to_params(&config);
        assert_eq!(params["model"], "o3-pro");
        assert_eq!(params["ephemeral"], true);
    }

    #[test]
    fn turn_config_extra_fields() {
        let mut config = TurnConfig::default();
        config.extra.insert("foo".into(), serde_json::json!("bar"));
        let params = config::turn_config_to_params(&config);
        assert_eq!(params["foo"], "bar");
    }

    #[test]
    fn thread_item_unknown_variant_deserializes() {
        let json = r#"{"type":"futureItemType","id":"x"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::Unknown));
    }

    #[test]
    fn thread_item_agent_message_deserializes() {
        let json = r#"{"type":"agentMessage","id":"msg1","text":"hello","phase":null}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::AgentMessage { ref id, .. } if id == "msg1"));
    }

    #[test]
    fn thread_item_command_execution_deserializes_camel_case_fields() {
        let json = r#"{
            "type":"commandExecution",
            "id":"cmd1",
            "command":"printf TESTOUTPUT",
            "status":"completed",
            "processId":"proc1",
            "aggregatedOutput":"TESTOUTPUT\n",
            "exitCode":0,
            "durationMs":12,
            "commandActions":[{"type":"search","command":"rg TESTOUTPUT .","query":"TESTOUTPUT","path":"."}]
        }"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(
            item,
            ThreadItem::CommandExecution {
                id,
                process_id,
                exit_code,
                aggregated_output,
                duration_ms,
                command_actions,
                ..
            }
            if id == "cmd1"
                && process_id.as_deref() == Some("proc1")
                && exit_code == Some(0)
                && aggregated_output.as_deref() == Some("TESTOUTPUT\n")
                && duration_ms == Some(12)
                && command_actions.len() == 1
        ));
    }

    #[test]
    fn rpc_error_display() {
        let err = Error::Rpc {
            code: -32600,
            message: "Invalid Request".into(),
            codex_error_info: None,
            data: None,
        };
        assert_eq!(err.to_string(), "JSON-RPC error (-32600): Invalid Request");
    }

    #[test]
    fn notification_parse_turn_completed() {
        let params = serde_json::json!({
            "threadId": "t1",
            "turn": {
                "id": "turn1",
                "items": [],
                "status": "completed"
            }
        });
        let event = notification::parse_turn_event("turn/completed", &params);
        assert!(matches!(event, TurnEvent::TurnCompleted { ref turn } if turn.id == "turn1"));
    }

    #[test]
    fn notification_parse_thread_name_updated_uses_thread_name() {
        let params = serde_json::json!({
            "threadId": "t1",
            "threadName": "Renamed thread"
        });
        let event = notification::parse_turn_event("thread/name/updated", &params);
        assert!(matches!(
            event,
            TurnEvent::ThreadNameUpdated { ref name }
            if name.as_deref() == Some("Renamed thread")
        ));
    }

    #[test]
    fn notification_parse_unknown_method() {
        let params = serde_json::json!({ "threadId": "t1" });
        let event = notification::parse_turn_event("thread/future.event", &params);
        assert!(
            matches!(event, TurnEvent::Unknown { ref method, .. } if method == "thread/future.event")
        );
    }

    #[test]
    fn notification_parse_agent_message_delta() {
        let params = serde_json::json!({
            "threadId": "t1",
            "itemId": "item1",
            "delta": "Hello "
        });
        let event = notification::parse_turn_event("item/agentMessage/delta", &params);
        assert!(matches!(
            event,
            TurnEvent::AgentMessageDelta { ref item_id, ref delta }
            if item_id == "item1" && delta == "Hello "
        ));
    }

    #[test]
    fn notification_parse_invalid_item_returns_unknown() {
        let params = serde_json::json!({
            "threadId": "t1",
            "item": "bad payload"
        });
        let event = notification::parse_turn_event("item/started", &params);
        assert!(matches!(
            event,
            TurnEvent::Unknown { ref method, .. } if method == "item/started"
        ));
    }

    #[test]
    fn account_read_response_deserializes_current_shape() {
        let response: AccountReadResponse = serde_json::from_value(serde_json::json!({
            "account": {
                "type": "chatgpt",
                "email": "test@example.com",
                "planType": "prolite"
            },
            "requiresOpenaiAuth": true
        }))
        .unwrap();
        assert!(matches!(
            response.account,
            Some(Account::Chatgpt {
                plan_type: PlanType::Prolite,
                ..
            })
        ));
    }

    #[test]
    fn thread_session_info_deserializes_read_only_sandbox_defaults() {
        let value = serde_json::json!({
            "thread": {
                "id": "thr_1",
                "preview": "",
                "ephemeral": true,
                "modelProvider": "openai",
                "createdAt": 1,
                "updatedAt": 1,
                "status": { "type": "idle" },
                "path": null,
                "cwd": "/tmp",
                "cliVersion": "0.1.0",
                "source": "vscode",
                "agentNickname": null,
                "agentRole": null,
                "gitInfo": null,
                "name": null,
                "turns": []
            },
            "model": "gpt-5.4",
            "modelProvider": "openai",
            "serviceTier": null,
            "cwd": "/tmp",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandbox": {
                "type": "readOnly",
                "access": { "type": "fullAccess" }
            },
            "reasoningEffort": "medium"
        });
        let session: ThreadSessionInfo = serde_json::from_value(value).unwrap();
        assert!(matches!(
            session.sandbox,
            SandboxPolicy::ReadOnly {
                network_access: false,
                ..
            }
        ));
    }

    #[test]
    fn config_merge_extra_wins() {
        let typed = serde_json::json!({"model": "a", "cwd": "/tmp"});
        let mut extra = std::collections::HashMap::new();
        extra.insert("model".into(), serde_json::json!("b"));
        extra.insert("newField".into(), serde_json::json!(42));
        let merged = config::merge_config(typed, &extra);
        assert_eq!(merged["model"], "b");
        assert_eq!(merged["cwd"], "/tmp");
        assert_eq!(merged["newField"], 42);
    }
}
