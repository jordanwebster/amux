//! The terminal client over sessions built from authored records and the
//! interpreter's recorded fixtures: layout from the anchor and paging,
//! runs keyed by item keys, every ask body, the composer's gates and
//! controls, Detached and Reset, the hosts overlay and the fleet.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prost::Message as _;
use ui_state::{FleetMsg, FleetState, InputOutcome, Msg, SessionState};
use ui_view::{AskBody, CardState, ask_card, queue_rows};
use wire::{Item, Kind, Phase, SessionEvent, ToolClass, ToolState, session_event};

use crate::chat::layout::{Anchor, PAGE};
use crate::chat::{ChatEffect, ChatView};
use crate::clipboard::ClipboardContent;
use crate::fixtures::{self, caught_up, draw, text};
use crate::fleet::{FleetEffect, FleetView};
use crate::theme::Theme;

const W: u16 = 100;
const H: u16 = 30;

fn theme() -> Theme {
    Theme::default()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn typed(view: &mut ChatView, state: &SessionState, words: &str) {
    for c in words.chars() {
        view.key(state, key(KeyCode::Char(c)), theme());
    }
}

/// A Claude SDK item: a reply, or an exploration Read of `path`.
fn item(order: u64, read: Option<&str>) -> Item {
    use wire::claude_sdk_item::Kind as K;
    let body = match read {
        Some(path) => wire::ClaudeSdkItem {
            kind: Some(K::Tool(wire::ToolCall {
                name: "Read".into(),
                state: ToolState::Succeeded as i32,
                class: ToolClass::Exploration as i32,
                input_json: format!(r#"{{"file_path":"{path}"}}"#).into_bytes(),
                ..Default::default()
            })),
        },
        None => wire::ClaudeSdkItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        },
    };
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        text: if read.is_none() {
            format!("reply number {order}")
        } else {
            String::new()
        },
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body: body.encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

fn snapshot(phase: Phase, body: Vec<u8>, queue: Vec<wire::QueuedInput>) -> Msg {
    event(session_event::Of::Snapshot(wire::Snapshot {
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body,
        phase: phase as i32,
        queue,
        ..wire::Snapshot::default()
    }))
}

/// A live Claude SDK chat holding `items`, caught up.
fn chat(items: Vec<Item>) -> SessionState {
    let mut state = SessionState::new(fixtures::agent(Kind::ClaudeSdk));
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    for item in items {
        state.update(event(session_event::Of::Item(item)));
    }
    state.update(caught_up(0));
    state
}

/// Replies at orders `from..=to`: a window with older history when `from`
/// is above one.
fn replies(from: u64, to: u64) -> Vec<Item> {
    (from..=to).map(|order| item(order, None)).collect()
}

fn feed(view: &mut ChatView, state: &SessionState) -> (String, Option<u32>) {
    let (buffer, page) = draw(view, state, 0, W, H, theme());
    (text(&buffer), page)
}

// --- layout from the anchor ------------------------------------------------

#[test]
fn the_feed_follows_the_newest_row() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, page) = feed(&mut view, &state);
    assert!(screen.contains("reply number 60"), "{screen}");
    assert!(!screen.contains("reply number 1\n"), "{screen}");
    assert_eq!(page, None, "order one is held: nothing older to fetch");
}

#[test]
fn fewer_than_a_page_of_held_rows_above_the_screen_asks_for_an_older_page() {
    // Orders 51..=70 held, older history exists.
    let state = chat(replies(51, 70));
    assert!(state.transcript().has_older());
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, Some(PAGE));
    // Once asked, it waits for the window to grow before asking again.
    view.page_sent(&state);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, None);

    // A page's worth above the screen: no fetch.
    let state = chat(replies(51, 200));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, None);
}

#[test]
fn an_open_collapsed_run_at_the_top_asks_for_a_larger_page() {
    // A run of 300 reads from the oldest held row, then two replies.
    let mut items: Vec<Item> = (101..=400)
        .map(|order| item(order, Some("src/lib.rs")))
        .collect();
    items.extend(replies(401, 402));
    let state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, page) = feed(&mut view, &state);
    assert!(screen.contains("300+ reads"), "{screen}");
    assert_eq!(page, Some(300), "the run's count, under the cap");
}

#[test]
fn a_scrolled_reader_keeps_its_place_as_rows_arrive() {
    let mut state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    assert!(matches!(view.anchor, Anchor::Top { .. }));
    let (before, _) = feed(&mut view, &state);
    for order in 61..=65 {
        state.update(event(session_event::Of::Item(item(order, None))));
    }
    let (after, _) = feed(&mut view, &state);
    let top = |screen: &str| {
        screen
            .lines()
            .skip(2)
            .take(10)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(top(&before), top(&after));
    assert!(after.contains("new activity below"), "{after}");
    view.key(
        &state,
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
        theme(),
    );
    assert_eq!(view.anchor, Anchor::Bottom);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("reply number 65"), "{screen}");
}

#[test]
fn scrolling_down_past_the_newest_row_follows_again() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    feed(&mut view, &state);
    for _ in 0..3 {
        view.key(&state, key(KeyCode::PageDown), theme());
        feed(&mut view, &state);
    }
    assert_eq!(view.anchor, Anchor::Bottom);
}

// --- runs ------------------------------------------------------------------

#[test]
fn a_run_collapses_to_its_summary_and_expands_by_item_key() {
    let mut items = replies(1, 1);
    items.extend((2..=5).map(|order| item(order, Some(&format!("src/file{order}.rs")))));
    items.extend(replies(6, 6));
    let mut state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("4 reads"), "{screen}");
    assert!(!screen.contains("src/file2.rs"), "{screen}");

    // Focus the summary and open it.
    view.move_focus(&state, true, theme());
    feed(&mut view, &state);
    view.move_focus(&state, true, theme());
    assert_eq!(view.focus.as_deref(), Some("k5"));
    view.toggle_expanded(&state);
    let (screen, _) = feed(&mut view, &state);
    for order in 2..=5 {
        assert!(screen.contains(&format!("src/file{order}.rs")), "{screen}");
    }

    // The run grows at the head and stays open: expansion is by key.
    state.update(event(session_event::Of::Item(item(
        7,
        Some("src/file7.rs"),
    ))));
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("src/file2.rs"), "{screen}");

    // And closes again from the summary.
    view.focus = Some("k5".into());
    view.toggle_expanded(&state);
    assert!(view.expanded.is_empty());
    let (screen, _) = feed(&mut view, &state);
    assert!(!screen.contains("src/file2.rs"), "{screen}");
}

// --- asks ------------------------------------------------------------------

fn body_name(body: &AskBody) -> &'static str {
    match body {
        AskBody::Command { .. } => "command",
        AskBody::Edit { .. } => "edit",
        AskBody::Tool { .. } => "tool",
        AskBody::Question(_) => "question",
        AskBody::Plan { .. } => "plan",
        AskBody::Form { .. } => "form",
        AskBody::Link { .. } => "link",
        AskBody::Access { .. } => "access",
        AskBody::Unanswerable { .. } => "unanswerable",
    }
}

/// A Claude SDK chat asking something this build cannot read.
fn unanswerable() -> SessionState {
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "menu-1".into(),
            body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
                reason: String::new(),
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    state
}

#[test]
fn every_ask_body_draws_with_stop_in_its_menu() {
    let mut seen = HashSet::new();
    let mut check = |state: &SessionState, at: i64, label: &str| {
        let Some(card) = ask_card(state) else {
            return;
        };
        if card.state != CardState::Open {
            return;
        }
        seen.insert(body_name(&card.body));
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        let (buffer, _) = draw(&mut view, state, at, 120, 60, theme());
        let screen = text(&buffer);
        assert!(screen.contains("Stop the turn"), "{label}: {screen}");
        assert!(screen.contains("ctrl+x stop"), "{label}: {screen}");
    };
    for (kind, dir) in [
        (Kind::ClaudePty, "claude_pty"),
        (Kind::ClaudeSdk, "claude_sdk"),
        (Kind::Codex, "codex"),
    ] {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../interpret/fixtures")
            .join(dir);
        let mut names: Vec<String> = std::fs::read_dir(&fixtures)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension()? == "json")
                    .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        for name in names {
            if name.starts_with("inject_") {
                continue;
            }
            for (label, state, at) in fixtures::frames(kind, &name) {
                check(&state, at, &format!("{dir}/{name} {label}"));
            }
        }
    }
    check(&unanswerable(), 0, "authored unanswerable");
    let mut seen: Vec<_> = seen.into_iter().collect();
    seen.sort_unstable();
    assert_eq!(
        seen,
        [
            "access",
            "command",
            "edit",
            "form",
            "link",
            "plan",
            "question",
            "tool",
            "unanswerable"
        ]
    );
}

#[test]
fn stop_in_the_menu_is_the_interrupt() {
    let (state, at) = fixtures::Named::CodexApproval.state();
    let card = ask_card(&state).expect("an approval");
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    draw(&mut view, &state, at, W, H, theme());
    let stop = card.choices.len() + 1;
    let digit = char::from_digit(stop as u32, 10).unwrap();
    view.key(&state, key(KeyCode::Char(digit)), theme());
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::Interrupt]
    );
    // Ctrl+X is the same act from anywhere in the chat.
    assert_eq!(
        view.key(&state, ctrl('x'), theme()),
        vec![ChatEffect::Interrupt]
    );
}

#[test]
fn the_likely_choice_answers_in_the_kinds_own_arm() {
    let (state, at) = fixtures::Named::CodexApproval.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    match effects.as_slice() {
        [ChatEffect::Answer(input)] => {
            assert!(
                matches!(input.of, Some(wire::input::Of::Codex(_))),
                "{input:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unanswerable_ask_offers_stop_and_the_terminal() {
    let state = unanswerable();
    let mut view = ChatView::new(b"agent".to_vec(), 0, true);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Can't answer this here"), "{screen}");
    assert!(screen.contains("1. Stop the turn"), "{screen}");
    assert!(
        screen.contains("2. Attach a terminal to answer it"),
        "{screen}"
    );
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::Interrupt]
    );
    view.key(&state, key(KeyCode::Down), theme());
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::RawAttach]
    );
}

#[test]
fn a_denial_takes_a_note_that_goes_back_to_the_agent() {
    let (state, at) = fixtures::Named::ClaudePermissionAsk.state();
    let card = ask_card(&state).unwrap();
    let deny = card
        .choices
        .iter()
        .position(|choice| choice.takes_note)
        .expect("Claude's deny takes a note");
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let digit = char::from_digit(deny as u32 + 1, 10).unwrap();
    view.key(&state, key(KeyCode::Char(digit)), theme());
    assert!(view.key(&state, key(KeyCode::Enter), theme()).is_empty());
    typed(&mut view, &state, "use cargo clean");
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    assert!(text(&buffer).contains("Tell it why (optional): use cargo clean"));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let Some(wire::input::Of::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = &input.of
    else {
        panic!("{input:?}");
    };
    let decoded = wire::ClaudeAnswer::decode(answer.body.as_slice()).unwrap();
    let Some(wire::claude_answer::Of::Permission(wire::PermissionAnswer {
        of: Some(wire::permission_answer::Of::Deny(deny)),
    })) = decoded.of
    else {
        panic!("{decoded:?}");
    };
    assert_eq!(deny.note, "use cargo clean");
}

#[test]
fn questions_answer_through_steps_and_a_review() {
    let (state, at) = fixtures::frame_where(
        Kind::ClaudeSdk,
        "recorded_question_every_shape",
        |state| matches!(ask_card(state).map(|card| card.body), Some(AskBody::Question(questions)) if questions.len() > 1),
    );
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let mut sent = None;
    for question in &questions {
        if question.multi_select {
            view.key(&state, key(KeyCode::Char(' ')), theme());
        }
        let effects = view.key(&state, key(KeyCode::Enter), theme());
        assert!(effects.is_empty(), "{effects:?}");
    }
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    assert!(text(&buffer).contains("[review]"));
    view.key(&state, key(KeyCode::Char('n')), theme());
    typed(&mut view, &state, "keep it short");
    view.key(&state, key(KeyCode::Enter), theme());
    for effect in view.key(&state, key(KeyCode::Enter), theme()) {
        if let ChatEffect::Answer(input) = effect {
            sent = Some(input);
        }
    }
    let input = sent.expect("the review sends the answers");
    let Some(wire::input::Of::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = &input.of
    else {
        panic!("{input:?}");
    };
    let decoded = wire::ClaudeAnswer::decode(answer.body.as_slice()).unwrap();
    let Some(wire::claude_answer::Of::Question(answers)) = decoded.of else {
        panic!("{decoded:?}");
    };
    assert_eq!(answers.answers.len(), questions.len());
    assert_eq!(answers.note, "keep it short");
}

#[test]
fn a_form_submits_what_was_typed() {
    let (state, at) =
        fixtures::frame_where(Kind::ClaudeSdk, "recorded_elicitation_accepted", |state| {
            matches!(
                ask_card(state).map(|card| card.body),
                Some(AskBody::Form { .. })
            )
        });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    // Edit the first field, then submit.
    view.key(&state, key(KeyCode::Enter), theme());
    typed(&mut view, &state, "jlw/amux");
    view.key(&state, key(KeyCode::Enter), theme());
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    assert!(text(&buffer).contains("jlw/amux"), "{}", text(&buffer));
    let AskBody::Form { schema_json, .. } = ask_card(&state).unwrap().body else {
        unreachable!()
    };
    let fields = serde_json::from_str::<serde_json::Value>(&schema_json).unwrap()["properties"]
        .as_object()
        .map_or(0, |fields| fields.len());
    for _ in 0..fields {
        view.key(&state, key(KeyCode::Down), theme());
    }
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let Some(wire::input::Of::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = &input.of
    else {
        panic!("{input:?}");
    };
    let decoded = wire::ClaudeAnswer::decode(answer.body.as_slice()).unwrap();
    let Some(wire::claude_answer::Of::Form(form)) = decoded.of else {
        panic!("{decoded:?}");
    };
    let content: serde_json::Value = serde_json::from_slice(&form.content_json).unwrap();
    assert!(content.to_string().contains("jlw/amux"), "{content}");
}

#[test]
fn an_unconfirmed_answer_offers_resend_and_discard() {
    let (mut state, at) = fixtures::Named::CodexApproval.state();
    let card = ask_card(&state).unwrap();
    let mut input = ui_view::answer_input(&card, &card.choices[0].answer, "").unwrap();
    input.input_id = b"answer-1".to_vec();
    state.update(Msg::Send(input));
    state.update(Msg::Sent(b"answer-1".to_vec(), InputOutcome::Lost));
    assert_eq!(ask_card(&state).unwrap().state, CardState::NotConfirmed);
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    assert!(text(&buffer).contains("r resend · d discard"));
    assert_eq!(
        view.key(&state, key(KeyCode::Char('r')), theme()),
        vec![ChatEffect::Resend {
            id: b"answer-1".to_vec()
        }]
    );
    assert_eq!(
        view.key(&state, key(KeyCode::Char('d')), theme()),
        vec![ChatEffect::Discard {
            id: b"answer-1".to_vec()
        }]
    );
}

// --- the composer ----------------------------------------------------------

#[test]
fn a_draft_is_taken_any_time_and_sent_only_when_caught_up_and_live() {
    let mut state = SessionState::new(fixtures::agent(Kind::ClaudeSdk));
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "hello");
    assert!(view.key(&state, key(KeyCode::Enter), theme()).is_empty());
    assert_eq!(view.editor.text(), "hello");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(text(&buffer).contains("catching up"), "{}", text(&buffer));

    state.update(caught_up(0));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(
        effects,
        vec![ChatEffect::Prompt {
            text: "hello".into(),
            attachments: vec![]
        }]
    );
    assert!(view.editor.is_empty());
}

#[test]
fn an_exited_entry_offers_resume_with_the_draft() {
    let mut state = chat(replies(1, 3));
    let mut exited = fixtures::agent(Kind::ClaudeSdk);
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    exited.exit_cause = Some("finished".into());
    state.update(Msg::Entry(exited));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("worker has exited"), "{screen}");
    assert!(screen.contains("exited · finished"), "{screen}");
    typed(&mut view, &state, "carry on");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(text(&buffer).contains("enter resume with this message"));
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::Resume {
            text: "carry on".into(),
            attachments: vec![]
        }]
    );
}

fn prompt(id: &[u8], words: &str) -> wire::Input {
    let mut input = ui_runtime::inputs::prompt(Kind::ClaudeSdk, words, vec![]).unwrap();
    input.input_id = id.to_vec();
    input
}

#[test]
fn an_unconfirmed_prompt_offers_resend_and_discard() {
    let mut state = chat(replies(1, 3));
    state.update(Msg::Send(prompt(b"p1", "did this land")));
    state.update(Msg::Sent(b"p1".to_vec(), InputOutcome::Lost));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("not confirmed  did this land"), "{screen}");
    assert!(screen.contains("resend · discard"), "{screen}");
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(view.tray, Some(0));
    assert_eq!(
        view.key(&state, key(KeyCode::Char('r')), theme()),
        vec![ChatEffect::Resend { id: b"p1".to_vec() }]
    );
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(
        view.key(&state, key(KeyCode::Char('d')), theme()),
        vec![ChatEffect::Discard { id: b"p1".to_vec() }]
    );
}

#[test]
fn a_queued_prompt_can_be_withdrawn_or_sent_now() {
    let queued = wire::QueuedInput {
        input_id: b"q1".to_vec(),
        text: "and then the docs".into(),
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::Working, vec![], vec![queued]));
    assert_eq!(queue_rows(&state).len(), 1);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(
        text(&buffer).contains("queued  and then the docs"),
        "{}",
        text(&buffer)
    );
    view.key(&state, key(KeyCode::Up), theme());
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(text(&buffer).contains("enter send now · w withdraw"));
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::SendNow { id: b"q1".to_vec() }]
    );
    view.tray = Some(0);
    assert_eq!(
        view.key(&state, key(KeyCode::Char('w')), theme()),
        vec![ChatEffect::Withdraw {
            id: b"q1".to_vec(),
            text: "and then the docs".into(),
            attachments: vec![]
        }]
    );
}

#[test]
fn a_steered_prompt_reads_steered_until_its_reflection() {
    let queued = wire::QueuedInput {
        input_id: b"q1".to_vec(),
        text: "use the other file".into(),
        steer: true,
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::Working, vec![], vec![queued]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(
        text(&buffer).contains("steered  use the other file"),
        "{}",
        text(&buffer)
    );
}

#[test]
fn ctrl_v_attaches_a_file_through_put_blob() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    assert_eq!(
        view.key(&state, ctrl('v'), theme()),
        vec![ChatEffect::Paste]
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"png bytes").unwrap();
    let effect = view.paste(ClipboardContent::Path(path)).unwrap();
    assert_eq!(
        effect,
        Some(ChatEffect::Attach {
            name: "shot.png".into(),
            mime: "image/png".into(),
            bytes: b"png bytes".to_vec()
        })
    );
    typed(&mut view, &state, "see ");
    view.attach_blob(wire::BlobRef {
        hash: vec![7; 32],
        name: "shot.png".into(),
        mime: "image/png".into(),
        size: 9,
    });
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(
        text(&buffer).contains("see [image shot.png · 9 B]"),
        "{}",
        text(&buffer)
    );
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Prompt { text, attachments }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(text, &format!("see {}", attachments::PLACEHOLDER));
    assert!(matches!(
        attachments[0].of,
        Some(wire::attachment::Of::Image(_))
    ));
}

// --- Detached and Reset ----------------------------------------------------

#[test]
fn detached_keeps_the_rows_and_disables_send() {
    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "studio".into(),
        ..Default::default()
    }));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "still typing");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("studio away · not current"), "{screen}");
    assert!(screen.contains("reply number 5"), "{screen}");
    assert!(screen.contains("draft kept · sending waits"), "{screen}");
    assert!(view.key(&state, key(KeyCode::Enter), theme()).is_empty());
    assert_eq!(view.editor.text(), "still typing");
}

#[test]
fn a_reset_keeps_the_rows_until_caught_up_then_shows_the_newest() {
    let mut state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    let (before, _) = feed(&mut view, &state);

    state.update(event(session_event::Of::Reset(wire::Reset {})));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    for order in 200..=210 {
        let mut fresh = item(order, None);
        fresh.key = format!("fresh{order}");
        state.update(event(session_event::Of::Item(fresh)));
    }
    let (during, _) = feed(&mut view, &state);
    assert_eq!(
        before.lines().nth(5),
        during.lines().nth(5),
        "rows stay on screen"
    );
    assert!(
        during.contains("refreshing") || during.contains("catching up"),
        "{during}"
    );

    state.update(caught_up(0));
    let (after, _) = feed(&mut view, &state);
    assert_eq!(view.anchor, Anchor::Bottom);
    assert!(after.contains("reply number 210"), "{after}");
}

// --- hosts and the fleet ---------------------------------------------------

fn inventory(fleet: &mut FleetState, of: wire::inventory_event::Of) {
    fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(of),
    })));
}

fn host(
    id: &[u8],
    name: &str,
    trust: wire::Trust,
    presence: wire::Presence,
) -> wire::inventory_event::Of {
    wire::inventory_event::Of::Host(wire::HostEntry {
        host_id: id.to_vec(),
        name: name.into(),
        trust: trust as i32,
        presence: presence as i32,
        via: wire::HostVia::Direct as i32,
        ..Default::default()
    })
}

#[test]
fn the_hosts_overlay_draws_trusted_hosts_and_candidates() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(
        &mut fleet,
        host(
            b"b",
            "laptop",
            wire::Trust::Trusted,
            wire::Presence::Offline,
        ),
    );
    inventory(
        &mut fleet,
        host(
            b"c",
            "den mac",
            wire::Trust::Candidate,
            wire::Presence::Online,
        ),
    );
    let lines = crate::hosts::overlay_lines(&fleet, 100, theme());
    let screen: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    assert!(
        screen.contains("● laptop") || screen.contains("○ laptop"),
        "{screen}"
    );
    assert!(screen.contains("offline"), "{screen}");
    assert!(screen.contains("online · direct"), "{screen}");
    assert!(screen.contains("Found nearby, not paired"), "{screen}");
    assert!(screen.contains("amux pair 'den mac'"), "{screen}");
    let studio = screen.find("studio").unwrap();
    let candidate = screen.find("den mac").unwrap();
    assert!(studio < candidate, "trusted hosts come first");
}

fn agent_row(
    id: &[u8],
    name: &str,
    phase: Phase,
    parent: Option<&[u8]>,
) -> wire::inventory_event::Of {
    wire::inventory_event::Of::Agent(wire::Agent {
        agent_id: id.to_vec(),
        host_id: b"a".to_vec(),
        kind: Kind::ClaudePty as i32,
        name: Some(name.into()),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: phase as i32,
        parent: parent.map(|parent| wire::AgentParent {
            host_id: b"a".to_vec(),
            agent_id: parent.to_vec(),
        }),
        ..Default::default()
    })
}

#[test]
fn the_fleet_lists_families_and_acts_on_the_selected_agent() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(&mut fleet, agent_row(b"p", "planner", Phase::Idle, None));
    inventory(
        &mut fleet,
        agent_row(b"c", "worker", Phase::NeedsYou, Some(b"p")),
    );
    inventory(&mut fleet, agent_row(b"s", "solo", Phase::Working, None));
    inventory(
        &mut fleet,
        wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    );
    let mut view = FleetView::default();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    let mut screen = String::new();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, &fleet, None, 0, theme());
        })
        .unwrap();
    screen.push_str(&text(terminal.backend().buffer()));
    // The family asking for the person leads, folded with its count.
    let planner = screen.find("planner").expect(&screen);
    let solo = screen.find("solo").expect(&screen);
    assert!(planner < solo, "{screen}");
    assert!(screen.contains("▸1"), "{screen}");
    assert!(!screen.contains("worker"), "{screen}");

    view.key(&fleet, key(KeyCode::Char('z')));
    let rows = view.rows(&fleet);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].card.name, "worker");
    assert_eq!(rows[1].depth, 1);

    let effects = view.key(&fleet, key(KeyCode::Enter));
    assert!(matches!(effects.as_slice(), [FleetEffect::Open(agent)] if agent.agent == b"p"));
    view.key(&fleet, key(KeyCode::Char('n')));
    assert_eq!(
        view.key(&fleet, key(KeyCode::Enter)),
        vec![FleetEffect::Create {
            kind: Kind::ClaudePty
        }]
    );
    view.key(&fleet, key(KeyCode::Char('d')));
    let effects = view.key(&fleet, key(KeyCode::Char('y')));
    assert!(matches!(effects.as_slice(), [FleetEffect::Delete(agent)] if agent.agent == b"p"));
    view.key(&fleet, key(KeyCode::Char('r')));
    view.key(&fleet, ctrl('u'));
    for c in "lead".chars() {
        view.key(&fleet, key(KeyCode::Char(c)));
    }
    let effects = view.key(&fleet, key(KeyCode::Enter));
    assert!(matches!(effects.as_slice(), [FleetEffect::Rename { name, .. }] if name == "lead"));
    // Raw attach is offered only when the CLI can hand the terminal over.
    assert!(view.key(&fleet, key(KeyCode::Char('o'))).is_empty());
    view.attach = true;
    assert!(matches!(
        view.key(&fleet, key(KeyCode::Char('o'))).as_slice(),
        [FleetEffect::Attach(_)]
    ));
}

fn fleet_screen(view: &mut FleetView, fleet: &FleetState) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, fleet, None, 0, theme());
        })
        .unwrap();
    text(terminal.backend().buffer())
}

#[test]
fn the_fleet_says_restart_to_update_when_the_daemon_runs_another_build() {
    let mut fleet = FleetState::new();
    let mut studio = host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.version = Some("0.8.0".into());
    }
    inventory(&mut fleet, studio);
    inventory(
        &mut fleet,
        wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    );
    let mut view = FleetView::default();
    view.version = "0.8.0".into();
    view.local_host = b"a".to_vec();
    let screen = fleet_screen(&mut view, &fleet);
    assert!(!screen.contains("restart to update"), "{screen}");
    view.version = "0.7.0".into();
    let screen = fleet_screen(&mut view, &fleet);
    assert!(
        screen.contains("amux 0.8.0 is running · restart to update"),
        "{screen}"
    );
}

#[test]
fn raw_attach_is_only_for_terminals_on_this_machine() {
    let config = crate::TuiConfig {
        working_dir: ".".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: true,
        version: "0.7.0".into(),
        local_host: b"a".to_vec(),
    };
    let agent = |host: &[u8], kind: Kind| wire::Agent {
        host_id: host.to_vec(),
        kind: kind as i32,
        ..Default::default()
    };
    let refusal = |agent: &wire::Agent| crate::app::terminal_refusal(agent, &config);
    assert_eq!(refusal(&agent(b"a", Kind::ClaudePty)), None);
    assert_eq!(refusal(&agent(b"a", Kind::Codex)), None);
    assert!(refusal(&agent(b"a", Kind::ClaudeSdk)).is_some_and(|why| why.contains("its chat")));
    assert!(refusal(&agent(b"b", Kind::ClaudePty)).is_some_and(|why| why.contains("its chat")));
}
