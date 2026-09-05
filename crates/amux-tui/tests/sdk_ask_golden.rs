//! What a Claude session driven over stream-JSON asks a person, and how
//! they answer it: permission, plan review, questions, an MCP server's own
//! form, and the dialogs whose shape nobody has recorded yet.
//!
//! The permission and elicitation frames come from sessions the daemon
//! really recorded. Plan, question and dialog requests have no recording
//! in the corpus, so those asks are appended to a real session as the
//! provider would send them, and each test says so.

use amux_tui::view::{UiAction, ViewState};
use amux_tui::{ChatView, ColorMode, FrameContext, Theme, render};
use amux_ui::claude_sdk::{
    AskWhy, ClaudeSdkCommand, DialogAnswer, ElicitationAnswer, PermissionAnswer, PlanAnswer,
    SdkAnswer,
};
use amux_ui::{
    Agent, AgentId, Command, HostEntry, HostId, Model, Msg, ServerMsg, StreamEntry, StreamMsg,
    update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use uuid::Uuid;

const NOW: &str = "2026-08-12T09:12:30Z";
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn host_id() -> HostId {
    Uuid::from_u128(1)
}

fn agent_id() -> AgentId {
    Uuid::from_u128(9)
}

fn an_sdk_agent() -> Agent {
    Agent {
        id: agent_id(),
        host_id: host_id(),
        name: Some("fix-sync".to_string()),
        command: "claude".to_string(),
        working_dir: "/work/amux".into(),
        kind: amux_ui::AgentKind::Claude {
            driver: amux_ui::ClaudeDriver::Sdk,
        },
        readonly: false,
        args: Vec::new(),
        created_at: at("2026-08-12T09:00:00Z"),
        parent: None,
        working_on: None,
    }
}

/// The recorded rows of one session, exactly as the daemon wrote them.
fn recorded(name: &str) -> Vec<Value> {
    let raw = match name {
        "text" => include_str!("../../amux/tests/fixtures/rows/claude-sdk/text_turn.rows.jsonl"),
        "permission" => {
            include_str!("../../amux/tests/fixtures/rows/claude-sdk/permission_callback.rows.jsonl")
        }
        "elicitation" => {
            include_str!(
                "../../amux/tests/fixtures/rows/claude-sdk/elicitation_accepted.rows.jsonl"
            )
        }
        other => panic!("unknown recording {other}"),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).expect("recorded row"))
        .collect()
}

fn base(readonly: bool) -> Vec<Msg> {
    let host = HostEntry {
        id: host_id(),
        name: "mbp".to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(amux_ui::Capabilities::default()),
        trust_status: amux_ui::HostTrustStatus::Trusted,
        last_dial_error: None,
    };
    let mut agent = an_sdk_agent();
    agent.readonly = readonly;
    vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host }),
        Msg::Server(ServerMsg::AgentUpserted { agent }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::ReplayComplete,
        },
    ]
}

fn batch(seq: u64, row: Value) -> Msg {
    Msg::Stream {
        agent: agent_id(),
        event: StreamMsg::Batch {
            at: at("2026-08-12T09:12:00Z"),
            entries: vec![StreamEntry { seq, payload: row }],
        },
    }
}

fn fold(msgs: Vec<Msg>) -> Model {
    let mut model = Model::default();
    for msg in msgs {
        update(&mut model, msg);
    }
    let violations = model.check_invariants();
    assert!(violations.is_empty(), "fixture coherent: {violations:?}");
    model
}

/// A recorded session folded up to the moment it is waiting on an ask of
/// this kind — the frame a person would be looking at.
fn recorded_ask(name: &str, why: AskWhy) -> Model {
    let mut msgs = base(false);
    let mut model = fold(msgs.clone());
    for (index, row) in recorded(name).into_iter().enumerate() {
        let msg = batch(index as u64, row);
        msgs.push(msg.clone());
        update(&mut model, msg);
        let waiting = model
            .claude_sdk(agent_id())
            .and_then(|layer| layer.ask_head())
            .is_some_and(|ask| ask.why() == why);
        if waiting {
            return fold(msgs);
        }
    }
    panic!("the recording never raised a {why:?} ask");
}

/// A real finished session with one further request appended, for the
/// kinds no recording in the corpus contains.
fn session_asking(row: Value) -> Model {
    let mut msgs = base(false);
    for (index, recorded_row) in recorded("text").into_iter().enumerate() {
        msgs.push(batch(index as u64, recorded_row));
    }
    msgs.push(batch(900, row));
    fold(msgs)
}

fn plan_ask() -> Value {
    json!({
        "type": "amux.claude_sdk.permission_required",
        "request_id": "plan-1",
        "tool_name": "ExitPlanMode",
        "input": {
            "plan": "# Plan: make the retry count configurable\n\n- read the current cap\n- add a `max_attempts` key\n- thread it through the retry loop\n- state the default where it is read\n\n## Verification\n\ncargo test -p amux-sync\n",
            "planFilePath": "plan.md"
        },
        "suggestions": []
    })
}

fn question_ask() -> Value {
    json!({
        "type": "amux.claude_sdk.permission_required",
        "request_id": "question-1",
        "tool_name": "AskUserQuestion",
        "input": {"questions": [
            {
                "header": "storage",
                "question": "Which stores should the migration cover?",
                "multiSelect": true,
                "options": [
                    {"label": "trust store", "description": "pairing + relay trust records"},
                    {"label": "session index", "description": "bounded tail metadata"},
                    {"label": "recorder dumps", "description": "panic-hook recordings"}
                ]
            },
            {
                "header": "rollout",
                "question": "When should it run?",
                "options": [
                    {"label": "on next start"},
                    {"label": "behind a flag"}
                ]
            }
        ]},
        "suggestions": []
    })
}

fn choice_dialog() -> Value {
    json!({
        "type": "amux.claude_sdk.dialog_required",
        "request_id": "dialog-1",
        "dialog_kind": "trust_prompt",
        "payload": {
            "message": "The workspace ~/work/amux is not in your trusted folders.",
            "options": [
                {"label": "Trust this folder", "value": {"trust": true}},
                {"label": "Don't trust it", "value": {"trust": false}}
            ]
        }
    })
}

fn opaque_dialog() -> Value {
    json!({
        "type": "amux.claude_sdk.dialog_required",
        "request_id": "dialog-2",
        "dialog_kind": "settings_editor",
        "payload": {"scope": "user", "path": "~/.claude/settings.json", "edits": [], "revision": 3}
    })
}

fn nested_elicitation() -> Value {
    json!({
        "type": "amux.claude_sdk.elicitation_required",
        "request_id": "form-2",
        "server": "external",
        "message": "Describe the release.",
        "schema": {
            "type": "object",
            "properties": {"release": {"type": "object"}},
            "required": ["release"]
        }
    })
}

fn open_chat(model: &Model) -> ChatView {
    let mut chat = ChatView::open(model, agent_id(), 'a', false).expect("the session opens a chat");
    chat.reconcile(model);
    chat
}

fn render_buffer(model: &Model, chat: ChatView, theme: Theme) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let ctx = FrameContext {
        viewport: (WIDTH, HEIGHT),
        theme,
        now: at(NOW),
    };
    let view = ViewState {
        chat: Some(chat),
        ..ViewState::default()
    };
    terminal
        .draw(|frame| render(model, &view, &ctx, frame))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        out.push('\n');
    }
    out
}

fn buffer_styles(buffer: &ratatui::buffer::Buffer, theme: Theme) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let style = buffer.cell((x, y)).expect("cell").style();
            out.push(theme.classify(style));
        }
        out.push('\n');
    }
    out
}

fn assert_golden(name: &str, rendered: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name} — run with UPDATE_GOLDENS=1"));
    assert_eq!(rendered, expected, "frame {name} diverged");
}

/// Both themes of one screen, keyed by name; the dark text comes back for
/// the assertions that read words rather than cells.
fn assert_surface(name: &str, model: &Model, chat: impl Fn(&Model) -> ChatView) -> String {
    let mut dark = String::new();
    for (theme_name, theme) in [
        ("dark", Theme::default()),
        ("light", Theme::light(ColorMode::TrueColor)),
    ] {
        let buffer = render_buffer(model, chat(model), theme);
        let text = buffer_text(&buffer);
        let rendered = format!(
            "--- text ---\n{text}--- styles ---\n{}",
            buffer_styles(&buffer, theme)
        );
        assert_golden(&format!("{name}_{theme_name}"), &rendered);
        if theme_name == "dark" {
            dark = text;
        }
    }
    dark
}

fn key(chat: &mut ChatView, model: &Model, key: KeyEvent) -> Option<UiAction> {
    amux_tui::chat::handle_chat_key(chat, model, key, (WIDTH, HEIGHT), at(NOW))
}

fn press(chat: &mut ChatView, model: &Model, code: KeyCode) -> Option<UiAction> {
    key(chat, model, KeyEvent::from(code))
}

fn answered(action: Option<UiAction>) -> SdkAnswer {
    match action {
        Some(UiAction::Dispatch(Command::ClaudeSdk(ClaudeSdkCommand::AnswerAsk {
            answer,
            ..
        }))) => answer,
        other => panic!("expected an answer for this session, got {other:?}"),
    }
}

/// A tool the session cannot run without being told to: the request, the
/// file it would write, and the three answers on offer.
#[test]
fn claude_sdk_ask_permission_docks_with_its_document() {
    let model = recorded_ask("permission", AskWhy::Permission);
    let text = assert_surface("sdk_ask_permission", &model, open_chat);
    assert!(
        text.contains("permission — Write"),
        "the panel names the tool and the file: {text}"
    );
    assert!(
        text.contains("Allow once") && text.contains("Deny"),
        "and the answers on offer: {text}"
    );
    let mut chat = open_chat(&model);
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Permission(PermissionAnswer::AllowOnce),
        "Enter on the first row allows once"
    );
}

/// Deny opens a one-line reason; Esc steps back to the action row without
/// answering, and what was typed survives it.
#[test]
fn claude_sdk_ask_esc_steps_back_and_never_answers() {
    let model = recorded_ask("permission", AskWhy::Permission);
    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Char('3')).is_none());
    assert!(
        press(&mut chat, &model, KeyCode::Enter).is_none(),
        "the reason stage opens"
    );
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "write it elsewhere");
    assert!(
        press(&mut chat, &model, KeyCode::Esc).is_none(),
        "Esc answers nothing"
    );
    let text = buffer_text(&render_buffer(&model, chat, Theme::default()));
    assert!(
        text.contains("Allow once"),
        "Esc is back at the action row: {text}"
    );
    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Esc).is_none());
    let text = buffer_text(&render_buffer(&model, chat, Theme::default()));
    assert!(
        text.contains("permission — Write"),
        "and the panel itself is never dismissed while the ask pends: {text}"
    );
}

/// A plan is read before it is approved, so it opens the reader with the
/// three-way review under it. No recording proposes a plan; this one is
/// appended to a real session.
#[test]
fn claude_sdk_ask_plan_opens_the_reader() {
    let model = session_asking(plan_ask());
    let text = assert_surface("sdk_ask_plan", &model, open_chat);
    assert!(
        text.contains("make the retry count configurable"),
        "the whole plan is on screen: {text}"
    );
    assert!(
        text.contains("Approve — auto")
            && text.contains("Approve — manual")
            && text.contains("Request changes"),
        "with the three-way review: {text}"
    );
    let mut chat = open_chat(&model);
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Plan(PlanAnswer::ApproveAuto),
        "the first row approves and lets the agent proceed"
    );
    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Char('3')).is_none());
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "cap it at four");
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Plan(PlanAnswer::RequestChanges {
            feedback: "cap it at four".to_string()
        }),
        "and the third asks for changes in the person's own words"
    );
}

/// The question form: one tab per question, an `Other…` row on each, and a
/// submit step that will not send an unanswered form. No recording asks a
/// question; this one is appended to a real session.
#[test]
fn claude_sdk_ask_question_shows_tabs_and_other() {
    let model = session_asking(question_ask());
    let text = assert_surface("sdk_ask_question", &model, open_chat);
    assert!(
        text.contains("[storage*]") && text.contains("[rollout]") && text.contains("[submit]"),
        "every question has its tab: {text}"
    );
    assert!(text.contains("Other…"), "and its own answer row: {text}");

    let mut chat = open_chat(&model);
    // First question is multi-select: space checks a box, Enter advances.
    assert!(press(&mut chat, &model, KeyCode::Char(' ')).is_none());
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    assert!(press(&mut chat, &model, KeyCode::Char('2')).is_none());
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    let answer = answered(press(&mut chat, &model, KeyCode::Enter));
    let SdkAnswer::Question(answers) = answer else {
        panic!("the form answers a question");
    };
    assert_eq!(answers.len(), 2, "both questions travel: {answers:?}");
    assert_eq!(answers[0].selected, vec![0]);
    assert_eq!(answers[1].selected, vec![1]);
}

/// An MCP server's own question, as a form over the schema it sent. This
/// is a session the daemon really recorded.
#[test]
fn claude_sdk_ask_elicitation_forms_the_schema() {
    let model = recorded_ask("elicitation", AskWhy::Elicitation);
    let text = assert_surface("sdk_ask_elicitation", &model, open_chat);
    assert!(
        text.contains("Confirm the word PELICAN."),
        "the server's own words: {text}"
    );
    assert!(
        text.contains("confirmed") && text.contains("required · text"),
        "one row per property, saying what it is: {text}"
    );
    assert!(
        text.contains("Send") && text.contains("Decline") && text.contains("Cancel"),
        "and the three answers a form takes: {text}"
    );

    let mut chat = open_chat(&model);
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "PELICAN");
    // Enter leaves the field for the action list, where Send is first.
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Elicitation(ElicitationAnswer::Accept {
            content: json!({"confirmed": "PELICAN"})
        }),
        "Send carries what was typed, under the schema's own field name"
    );

    // An empty required field is not sendable, and the panel says which.
    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    assert!(
        press(&mut chat, &model, KeyCode::Enter).is_none(),
        "Send is refused while the required field is empty"
    );
    let text = buffer_text(&render_buffer(&model, chat, Theme::default()));
    assert!(
        text.contains("confirmed is required"),
        "and the reason sits where Send is: {text}"
    );
}

/// A schema this build cannot express is blocked, with the reason and
/// only the answers that need no fields.
#[test]
fn claude_sdk_ask_elicitation_blocked_schema_states_why() {
    let model = session_asking(nested_elicitation());
    let text = assert_surface("sdk_ask_elicitation_blocked", &model, open_chat);
    assert!(
        text.contains("only text, number, boolean and enum fields are supported"),
        "the reason is stated in full: {text}"
    );
    assert!(
        !text.contains("Send"),
        "and nothing offers to send a form nobody can fill: {text}"
    );
    let mut chat = open_chat(&model);
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Elicitation(ElicitationAnswer::Decline),
        "declining is the person's own answer, not the daemon's"
    );
}

/// A dialog whose payload carries a message and labelled choices is
/// answered by choosing one. No dialog has ever been recorded; this one
/// is appended to a real session in the shape the protocol documents.
#[test]
fn claude_sdk_ask_dialog_offers_its_own_choices() {
    let model = session_asking(choice_dialog());
    let text = assert_surface("sdk_ask_dialog", &model, open_chat);
    assert!(
        text.contains("dialog — trust_prompt"),
        "the kind is named as it arrived: {text}"
    );
    assert!(
        text.contains("Trust this folder") && text.contains("the agent is told the dialog"),
        "its choices and an honest cancel: {text}"
    );
    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Char('2')).is_none());
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Dialog(DialogAnswer::Choose { option: 1 }),
        "the second row chooses the second option"
    );
}

/// A payload in no shape this build can answer states what it holds and
/// offers only cancel — never raw JSON, never something that reads as
/// agreement.
#[test]
fn claude_sdk_ask_dialog_without_a_shape_offers_only_cancel() {
    let model = session_asking(opaque_dialog());
    let text = assert_surface("sdk_ask_dialog_blocked", &model, open_chat);
    assert!(
        text.contains("This request cannot be answered from the chat."),
        "the limit is stated: {text}"
    );
    assert!(
        text.contains("object with 4 fields"),
        "with what the payload holds, in words: {text}"
    );
    assert!(
        !text.contains('{'),
        "and never the payload's own JSON: {text}"
    );
    let mut chat = open_chat(&model);
    assert_eq!(
        answered(press(&mut chat, &model, KeyCode::Enter)),
        SdkAnswer::Dialog(DialogAnswer::Cancel),
        "cancel is the only answer, and it says so"
    );
}

/// A read-only chat states what the agent is asking and waits: read
/// affordances only, no action row.
#[test]
fn claude_sdk_ask_read_only_chat_shows_the_fact_panel() {
    let mut msgs = base(true);
    let mut model = fold(msgs.clone());
    for (index, row) in recorded("permission").into_iter().enumerate() {
        let msg = batch(index as u64, row);
        msgs.push(msg.clone());
        update(&mut model, msg);
        if model
            .claude_sdk(agent_id())
            .and_then(|layer| layer.ask_head())
            .is_some()
        {
            break;
        }
    }
    let model = fold(msgs);
    let text = assert_surface("sdk_ask_readonly", &model, open_chat);
    assert!(
        text.contains("the agent is asking permission"),
        "the fact, not the offer: {text}"
    );
    assert!(
        text.contains("waiting for a writable client"),
        "and the honest wait: {text}"
    );
    assert!(
        !text.contains("Allow once"),
        "no action an observer cannot take: {text}"
    );
    let mut chat = open_chat(&model);
    assert!(
        press(&mut chat, &model, KeyCode::Enter).is_none(),
        "Enter answers nothing here"
    );
}

/// Every ask kind draws at every viewport, including sizes below the
/// frame's minimum.
#[test]
fn claude_sdk_ask_panels_render_at_every_viewport() {
    for model in [
        recorded_ask("permission", AskWhy::Permission),
        recorded_ask("elicitation", AskWhy::Elicitation),
        session_asking(plan_ask()),
        session_asking(question_ask()),
        session_asking(choice_dialog()),
        session_asking(opaque_dialog()),
    ] {
        for size in [(20, 6), (24, 10), (40, 12), (80, 24), (200, 60)] {
            let backend = TestBackend::new(size.0, size.1);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let ctx = FrameContext {
                viewport: size,
                theme: Theme::default(),
                now: at(NOW),
            };
            let view = ViewState {
                chat: Some(open_chat(&model)),
                ..ViewState::default()
            };
            terminal
                .draw(|frame| render(&model, &view, &ctx, frame))
                .expect("draw");
        }
    }
}

/// Ctrl+X still interrupts from under a docked panel, and Ctrl+C clears a
/// half-typed reason rather than answering or quitting.
#[test]
fn claude_sdk_ask_control_keys_reach_past_the_panel() {
    let model = recorded_ask("permission", AskWhy::Permission);
    let mut chat = open_chat(&model);
    assert!(matches!(
        key(
            &mut chat,
            &model,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ),
        Some(UiAction::Dispatch(Command::ClaudeSdk(
            ClaudeSdkCommand::Interrupt { .. }
        )))
    ));

    let mut chat = open_chat(&model);
    assert!(press(&mut chat, &model, KeyCode::Char('3')).is_none());
    assert!(press(&mut chat, &model, KeyCode::Enter).is_none());
    amux_tui::chat::handle_chat_paste(&mut chat, &model, "not this file");
    assert!(
        key(
            &mut chat,
            &model,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        )
        .is_none(),
        "the first Ctrl+C clears the reason instead of quitting"
    );
    let text = buffer_text(&render_buffer(&model, chat, Theme::default()));
    assert!(
        !text.contains("not this file"),
        "the typed reason is gone: {text}"
    );
}
