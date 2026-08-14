//! Offline regressions for the opt-in real-Codex harness's parsed waiters.

#[path = "codex_capture/structure.rs"]
mod structure;

use structure::{Matcher, Row, find_match, parse_jsonl};

const ACTUAL_STARTUP_CAPTURE: &str =
    include_str!("codex_capture/fixtures/startup_pong.redacted.jsonl");
const ACTUAL_APPROVAL_CAPTURE: &str =
    include_str!("codex_capture/fixtures/approval_interrupt.redacted.jsonl");

#[test]
fn startup_noise_does_not_confuse_pong_waiters() {
    let rows = parse_jsonl(ACTUAL_STARTUP_CAPTURE).unwrap();
    let ready = find_match(&rows, 0, &Matcher::Type("amux.codex_ready")).unwrap();
    let text = find_match(&rows, ready + 1, &Matcher::AgentTextContains("PONG".into())).unwrap();
    let completed = find_match(&rows, text + 1, &Matcher::TurnCompleted("completed")).unwrap();

    assert!(ready < text && text < completed);
    assert_eq!(
        rows.iter()
            .filter(|row| row.row_type() == Some("mcpServer/startupStatus/updated"))
            .count(),
        4
    );
}

#[test]
fn approval_and_interrupt_waiters_follow_typed_fields() {
    let rows = parse_jsonl(ACTUAL_APPROVAL_CAPTURE).unwrap();
    let approval = find_match(&rows, 0, &Matcher::ApprovalRequired).unwrap();
    let request_id = rows[approval].json["request_id"].clone();
    let resolved = find_match(&rows, approval + 1, &Matcher::ApprovalResolved(request_id)).unwrap();
    let interrupted =
        find_match(&rows, resolved + 1, &Matcher::TurnCompleted("interrupted")).unwrap();
    let recovered =
        find_match(&rows, interrupted + 1, &Matcher::TurnCompleted("completed")).unwrap();

    assert!(approval < resolved && resolved < interrupted && interrupted < recovered);
}

#[test]
fn object_key_order_is_irrelevant() {
    let row = Row::parse(
        7,
        br#"{"turn":{"status":"completed","id":"turn-7"},"type":"turn/completed"}"#,
    )
    .unwrap();
    assert!(Matcher::TurnCompleted("completed").matches(&row));
    assert_eq!(row.turn_id(), Some("turn-7"));
    assert_eq!(row.seq, 7);
}

#[test]
fn remaining_live_waiters_match_structural_fields() {
    let input = Row::parse(
        1,
        br#"{"type":"amux.input_result","ok":{},"input_id":[9,8,7]}"#,
    )
    .unwrap();
    let gap = Row::parse(
        2,
        br#"{"reason":"connection_lost","type":"amux.codex_gap"}"#,
    )
    .unwrap();
    let started = Row::parse(
        3,
        br#"{"threadId":"thread-3","turn":{"id":"turn-3"},"type":"turn/started"}"#,
    )
    .unwrap();
    let command = Row::parse(
        4,
        br#"{"item":{"status":"declined","type":"commandExecution"},"type":"item/completed"}"#,
    )
    .unwrap();

    assert!(Matcher::InputOk(vec![9, 8, 7]).matches(&input));
    assert!(Matcher::GapReason("connection_lost").matches(&gap));
    assert!(Matcher::TurnStarted.matches(&started));
    assert_eq!(started.thread_id(), Some("thread-3"));
    assert!(Matcher::CommandCompleted("declined").matches(&command));
}
