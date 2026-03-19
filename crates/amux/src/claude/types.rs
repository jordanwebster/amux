//! Claude Code integration types.
//!
//! This module contains all types specific to Claude Code integration:
//! hook events, permission requests, tool input structs, structured
//! input/output envelopes, and permission responses.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Hook {
    Claude(ClaudeHook),
}

/// Claude Code hook event - uses Claude's tagged JSON format
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
    PermissionRequest(ClaudePermissionRequest),
    Stop(ClaudeStop),
    #[serde(other)]
    Unknown,
}

/// SessionStart hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeSessionStart {
    pub session_id: Uuid,
    pub transcript_path: String,
}

/// Stop hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeStop {
    pub session_id: Uuid,
    pub stop_hook_active: bool,
    pub last_assistant_message: String,
}

/// PermissionRequest hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudePermissionRequest {
    pub session_id: Uuid,
    #[serde(flatten)]
    pub tool: ClaudePermissionTool,
}

/// Tool input fields for the Edit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EditToolInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Tool input fields for the AskUserQuestion tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionToolInput {
    pub questions: Vec<AskUserQuestionItem>,
}

/// A single question within an AskUserQuestion request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionItem {
    pub question: String,
    pub header: String,
    pub options: Vec<AskUserQuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

/// An option within an AskUserQuestion question
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AskUserQuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub preview: Option<String>,
}

/// Tool input fields for the Bash tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BashToolInput {
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Tool input fields for the Write tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WriteToolInput {
    pub file_path: String,
    pub content: String,
}

/// Tool input fields for the WebFetch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebFetchToolInput {
    pub url: String,
    pub prompt: String,
}

/// Tool input fields for the WebSearch tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WebSearchToolInput {
    pub query: String,
}

/// Tool input fields for the NotebookEdit tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct NotebookEditToolInput {
    pub notebook_path: String,
    pub new_source: String,
    #[serde(default)]
    pub cell_type: Option<String>,
    #[serde(default)]
    pub edit_mode: Option<String>,
}

/// Tool input fields for the Skill tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillToolInput {
    pub skill: String,
    #[serde(default)]
    pub args: Option<String>,
}

/// Tool input fields for the ExitPlanMode tool
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModeToolInput {
    #[serde(default, rename = "allowedPrompts")]
    pub allowed_prompts: Vec<ExitPlanModePrompt>,
}

/// A single allowed prompt within an ExitPlanMode request
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ExitPlanModePrompt {
    pub tool: String,
    pub prompt: String,
}

/// Tool data from Claude Code permission requests.
/// Uses internally-tagged format so #[serde(other)] can ignore unknown tool_input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "tool_name")]
pub enum ClaudePermissionTool {
    Edit {
        tool_input: EditToolInput,
    },
    AskUserQuestion {
        tool_input: AskUserQuestionToolInput,
    },
    Bash {
        tool_input: BashToolInput,
    },
    Write {
        tool_input: WriteToolInput,
    },
    WebFetch {
        tool_input: WebFetchToolInput,
    },
    WebSearch {
        tool_input: WebSearchToolInput,
    },
    NotebookEdit {
        tool_input: NotebookEditToolInput,
    },
    Skill {
        tool_input: SkillToolInput,
    },
    ExitPlanMode {
        tool_input: ExitPlanModeToolInput,
    },
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for ClaudePermissionTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudePermissionTool::Edit { tool_input } => {
                write!(f, "Edit {}", tool_input.file_path)
            }
            ClaudePermissionTool::AskUserQuestion { tool_input } => {
                write!(
                    f,
                    "AskUserQuestion ({} question(s))",
                    tool_input.questions.len()
                )
            }
            ClaudePermissionTool::Bash { tool_input } => {
                write!(f, "Bash `{}`", tool_input.command)
            }
            ClaudePermissionTool::Write { tool_input } => {
                write!(f, "Write {}", tool_input.file_path)
            }
            ClaudePermissionTool::WebFetch { tool_input } => {
                write!(f, "WebFetch {}", tool_input.url)
            }
            ClaudePermissionTool::WebSearch { tool_input } => {
                write!(f, "WebSearch {}", tool_input.query)
            }
            ClaudePermissionTool::NotebookEdit { tool_input } => {
                write!(f, "NotebookEdit {}", tool_input.notebook_path)
            }
            ClaudePermissionTool::Skill { tool_input } => {
                write!(f, "Skill {}", tool_input.skill)
            }
            ClaudePermissionTool::ExitPlanMode { .. } => write!(f, "ExitPlanMode"),
            ClaudePermissionTool::Unknown => write!(f, "Unknown tool"),
        }
    }
}

impl std::fmt::Display for ClaudeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeHook::SessionStart(s) => {
                write!(f, "session {} at {}", s.session_id, s.transcript_path)
            }
            ClaudeHook::PermissionRequest(p) => {
                write!(f, "session {} {}", p.session_id, p.tool)
            }
            ClaudeHook::Stop(s) => {
                write!(f, "session {} stopped", s.session_id)
            }
            ClaudeHook::Unknown => write!(f, "unknown hook"),
        }
    }
}

/// Response to a permission request (sent from client to server)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PermissionResponse {
    /// Press "1" - accept this edit
    Yes,
    /// Press "2" - accept all edits
    YesAll,
    /// Press "3" - deny
    No,
}

/// Claude-specific structured output (internally tagged by "type")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClaudeStructuredOutput {
    /// User message to the agent
    UserMessage {
        content: String,
        timestamp: String,
        uuid: String,
    },
    /// Assistant response (text content)
    AssistantMessage {
        content: String,
        timestamp: String,
        uuid: String,
    },
    /// Permission request from agent
    PermissionRequest { tool: ClaudePermissionTool },
    /// Agent has stopped and is waiting for input
    AgentStopped,
    /// Unknown entry type (forward-compatibility)
    #[serde(other)]
    Unknown,
}

/// Wrapper enum for structured output, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredOutput {
    Claude(ClaudeStructuredOutput),
}

/// Response to an AskUserQuestion tool call.
/// Matches Claude Code's tool_result format: echoes back the questions
/// and provides answers as label strings (or custom text for "Other").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserQuestionResponse {
    /// Echo of the original questions (determines question type + option indices)
    pub questions: Vec<AskUserQuestionItem>,
    /// Map from question text to selected label (or custom text for "Other").
    /// Multi-select: comma-separated labels (e.g. "Auth, Cache").
    pub answers: HashMap<String, String>,
    /// Question text where "Chat about this" was selected.
    /// Answered questions are processed first, then we navigate
    /// to this question's page to select ChatAboutThis.
    /// Ends the tool immediately (no submit step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_about_this: Option<String>,
}

/// Claude-specific structured input
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClaudeStructuredInput {
    /// Response to a permission request
    PermissionResponse(PermissionResponse),
    /// Submit a message (text input with trailing carriage return)
    SubmitMessage { data: Vec<u8> },
    /// Response to an AskUserQuestion tool call
    AskUserQuestionResponse(AskUserQuestionResponse),
}

/// Wrapper enum for structured input, keyed by agent type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StructuredInput {
    Claude(ClaudeStructuredInput),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "SomeNewTool",
            "tool_input": {"foo": "bar"}
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        assert!(matches!(p.tool, ClaudePermissionTool::Unknown));
    }

    #[test]
    fn test_ask_user_question_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which library should we use?",
                    "header": "Library",
                    "options": [
                        {"label": "reqwest", "description": "HTTP client"},
                        {"label": "ureq", "description": "Blocking HTTP client"}
                    ],
                    "multiSelect": false
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert_eq!(tool_input.questions.len(), 1);
        let q = &tool_input.questions[0];
        assert_eq!(q.question, "Which library should we use?");
        assert_eq!(q.header, "Library");
        assert!(!q.multi_select);
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].label, "reqwest");
        assert_eq!(q.options[1].label, "ureq");
    }

    #[test]
    fn test_ask_user_question_multi_select() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Which features?",
                    "header": "Features",
                    "options": [
                        {"label": "Auth", "description": "Authentication"},
                        {"label": "Cache", "description": "Caching layer"}
                    ],
                    "multiSelect": true
                }]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::AskUserQuestion { tool_input } = p.tool else {
            panic!("Expected AskUserQuestion tool");
        };
        assert!(tool_input.questions[0].multi_select);
    }

    #[test]
    fn test_known_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Edit",
            "tool_input": {
                "file_path": "/tmp/test.rs",
                "old_string": "foo",
                "new_string": "bar"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Edit { tool_input } = p.tool else {
            panic!("Expected Edit tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/test.rs");
        assert_eq!(tool_input.old_string, "foo");
        assert_eq!(tool_input.new_string, "bar");
        assert!(!tool_input.replace_all);
    }

    #[test]
    fn test_bash_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "cargo test",
                "description": "Run tests",
                "timeout": 30000
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "cargo test");
        assert_eq!(tool_input.description.as_deref(), Some("Run tests"));
        assert_eq!(tool_input.timeout, Some(30000));
    }

    #[test]
    fn test_bash_tool_optional_fields() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Bash",
            "tool_input": {
                "command": "ls"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Bash { tool_input } = p.tool else {
            panic!("Expected Bash tool");
        };
        assert_eq!(tool_input.command, "ls");
        assert!(tool_input.description.is_none());
        assert!(tool_input.timeout.is_none());
    }

    #[test]
    fn test_write_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/output.txt",
                "content": "hello world"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Write { tool_input } = p.tool else {
            panic!("Expected Write tool");
        };
        assert_eq!(tool_input.file_path, "/tmp/output.txt");
        assert_eq!(tool_input.content, "hello world");
    }

    #[test]
    fn test_web_search_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "WebSearch",
            "tool_input": {
                "query": "rust serde tutorial"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::WebSearch { tool_input } = p.tool else {
            panic!("Expected WebSearch tool");
        };
        assert_eq!(tool_input.query, "rust serde tutorial");
    }

    #[test]
    fn test_skill_tool_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "Skill",
            "tool_input": {
                "skill": "commit",
                "args": "-m 'Fix bug'"
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::Skill { tool_input } = p.tool else {
            panic!("Expected Skill tool");
        };
        assert_eq!(tool_input.skill, "commit");
        assert_eq!(tool_input.args.as_deref(), Some("-m 'Fix bug'"));
    }

    #[test]
    fn test_exit_plan_mode_deserializes_from_json() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "tool_name": "ExitPlanMode",
            "tool_input": {
                "allowedPrompts": [
                    {"tool": "Bash", "prompt": "run tests"}
                ]
            }
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        let ClaudeHook::PermissionRequest(p) = hook else {
            panic!("Expected PermissionRequest");
        };
        let ClaudePermissionTool::ExitPlanMode { tool_input } = p.tool else {
            panic!("Expected ExitPlanMode tool");
        };
        assert_eq!(tool_input.allowed_prompts.len(), 1);
        assert_eq!(tool_input.allowed_prompts[0].tool, "Bash");
        assert_eq!(tool_input.allowed_prompts[0].prompt, "run tests");
    }

    #[test]
    fn test_claude_permission_tool_display() {
        let tool = ClaudePermissionTool::Bash {
            tool_input: BashToolInput {
                command: "cargo test".to_string(),
                description: None,
                timeout: None,
            },
        };
        assert_eq!(tool.to_string(), "Bash `cargo test`");

        let tool = ClaudePermissionTool::Edit {
            tool_input: EditToolInput {
                file_path: "/tmp/test.rs".to_string(),
                old_string: "a".to_string(),
                new_string: "b".to_string(),
                replace_all: false,
            },
        };
        assert_eq!(tool.to_string(), "Edit /tmp/test.rs");

        assert_eq!(ClaudePermissionTool::Unknown.to_string(), "Unknown tool");
    }

    #[test]
    fn test_ask_user_question_option_without_preview() {
        let json = r#"{"label": "reqwest", "description": "HTTP client"}"#;
        let opt: AskUserQuestionOption = serde_json::from_str(json).unwrap();
        assert_eq!(opt.label, "reqwest");
        assert_eq!(opt.description, "HTTP client");
        assert!(opt.preview.is_none());
    }

    #[test]
    fn test_ask_user_question_option_with_preview() {
        let json = r#"{
            "label": "Layout A",
            "description": "Side-by-side layout",
            "preview": "```\n+-----+-----+\n| A   | B   |\n+-----+-----+\n```"
        }"#;
        let opt: AskUserQuestionOption = serde_json::from_str(json).unwrap();
        assert_eq!(opt.label, "Layout A");
        assert!(opt.preview.is_some());
        assert!(opt.preview.unwrap().contains("+-----+"));
    }

    #[test]
    fn test_ask_user_question_response_roundtrip() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![
                    AskUserQuestionOption {
                        label: "reqwest".to_string(),
                        description: "HTTP client".to_string(),
                        preview: None,
                    },
                    AskUserQuestionOption {
                        label: "ureq".to_string(),
                        description: "Blocking HTTP".to_string(),
                        preview: None,
                    },
                ],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "reqwest".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_custom_answer() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![AskUserQuestionOption {
                    label: "reqwest".to_string(),
                    description: "HTTP client".to_string(),
                    preview: None,
                }],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "my custom lib".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_multi_select_answer() {
        let resp = AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which features?".to_string(),
                header: "Features".to_string(),
                options: vec![
                    AskUserQuestionOption {
                        label: "Auth".to_string(),
                        description: "Authentication".to_string(),
                        preview: None,
                    },
                    AskUserQuestionOption {
                        label: "Cache".to_string(),
                        description: "Caching".to_string(),
                        preview: None,
                    },
                ],
                multi_select: true,
            }],
            answers: HashMap::from([("Which features?".to_string(), "Auth, Cache".to_string())]),
            chat_about_this: None,
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: AskUserQuestionResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn test_ask_user_question_response_as_structured_input() {
        let input = ClaudeStructuredInput::AskUserQuestionResponse(AskUserQuestionResponse {
            questions: vec![AskUserQuestionItem {
                question: "Which library?".to_string(),
                header: "Library".to_string(),
                options: vec![AskUserQuestionOption {
                    label: "reqwest".to_string(),
                    description: "HTTP client".to_string(),
                    preview: None,
                }],
                multi_select: false,
            }],
            answers: HashMap::from([("Which library?".to_string(), "reqwest".to_string())]),
            chat_about_this: None,
        });
        let serialized = serde_json::to_string(&input).unwrap();
        let deserialized: ClaudeStructuredInput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_claude_hook_display() {
        let hook = ClaudeHook::SessionStart(ClaudeSessionStart {
            session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
        });
        assert_eq!(
            hook.to_string(),
            "session 00000000-0000-0000-0000-000000000001 at /tmp/transcript.jsonl"
        );

        let hook = ClaudeHook::PermissionRequest(ClaudePermissionRequest {
            session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            tool: ClaudePermissionTool::Bash {
                tool_input: BashToolInput {
                    command: "ls".to_string(),
                    description: None,
                    timeout: None,
                },
            },
        });
        assert_eq!(
            hook.to_string(),
            "session 00000000-0000-0000-0000-000000000002 Bash `ls`"
        );
    }
}
