//! Chapter 24 — Completions and exits are not ordinary messages.
//!
//! Three things arrive by the same carrier and mean three different things:
//! someone is talking to this agent, someone it started has finished a
//! turn, and someone it started is gone. The envelope kind is amux's own
//! vocabulary, so what each kind makes of itself on screen is decided in
//! the kernel — once, for every chat — rather than twice, differently, in
//! the two layers that happen to render it.

use amux_ui::claude::ClaudeLayer;
use amux_ui::codex::CodexLayer;
use amux_ui::{AgentMessageKind, AgentMessagePresentation, Model, Msg, message_digest};
use serde_json::json;

use crate::harness::*;

const CLAUDE_AGENT: &str = "claude-parent";
const CODEX_AGENT: &str = "codex-parent";

/// A completion body long enough to be worth closing: a child's last
/// assistant message is whatever the child said last.
const REPORT: &str = "migrated 14 call sites\n\nthe two in `legacy/` need a decision:\nthey pass the old shape through a macro.";

/// An envelope of the given kind, formatted by the daemon's own formatter
/// and delivered by the paste carrier.
fn claude_row(kind: amux::envelope::EnvelopeKind, text: &str) -> serde_json::Value {
    let envelope = amux::envelope::Envelope {
        id: uuid::Uuid::from_u128(0xa2a),
        context: None,
        from: amux::envelope::Sender::Agent(amux::envelope::AgentSender {
            agent_id: uuid::Uuid::from_u128(0xbeef),
            host_id: uuid::Uuid::from_u128(0xcafe),
            name: "scribe".to_string(),
            kind: "codex".to_string(),
        }),
        to: amux::AgentParent {
            agent_id: uuid::Uuid::from_u128(0xfeed),
            host_id: uuid::Uuid::from_u128(0xcafe),
        },
        kind,
        text: text.to_string(),
    };
    json!({
        "type": "user",
        "isMeta": false,
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": amux::envelope::format(&envelope)},
    })
}

/// The row the daemon writes into a Codex thread for the same envelope.
fn codex_row(kind: &str, text: &str) -> serde_json::Value {
    json!({
        "type": "amux.codex_message",
        "id": "00000000-0000-0000-0000-0000000000a1",
        "kind": kind,
        "from": "scribe/00000000-0000-0000-0000-0000000000c0",
        "from_id": "00000000-0000-0000-0000-0000000000b0",
        "text": text,
        "delivery": "inject_queued",
    })
}

/// One of each kind, to a Claude parent.
fn claude_sequence() -> Vec<Msg> {
    seq([
        chat_base(CLAUDE_AGENT),
        vec![batch(
            CLAUDE_AGENT,
            10,
            vec![
                claude_row(amux::envelope::EnvelopeKind::Message, "how far along?"),
                claude_row(amux::envelope::EnvelopeKind::Completed, REPORT),
                claude_row(amux::envelope::EnvelopeKind::Exited, ""),
            ],
        )],
    ])
}

/// The same three, to a Codex parent.
fn codex_sequence() -> Vec<Msg> {
    seq([
        codex_base(CODEX_AGENT),
        vec![batch(
            CODEX_AGENT,
            10,
            vec![
                codex_row("message", "how far along?"),
                codex_row("completed", REPORT),
                codex_row("exited", ""),
            ],
        )],
    ])
}

fn claude_messages(layer: &ClaudeLayer) -> Vec<(AgentMessageKind, String)> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            amux_ui::claude::FeedEntryKind::AgentMessage(message) => {
                Some((message.kind.clone(), message.text.clone()))
            }
            _ => None,
        })
        .collect()
}

fn codex_messages(layer: &CodexLayer) -> Vec<(AgentMessageKind, String)> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            amux_ui::codex::FeedEntryKind::AgentMessage(message) => {
                Some((message.kind.clone(), message.text.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A completion is an inbound row wearing a finished mark, over a body
/// that can be closed. It arrives from the child, so it is inbound; it
/// says a turn ended, so it is finished; it carries a whole last message,
/// so the body is worth closing.
#[test]
fn a2a_completed_row_is_an_inbound_row_that_finished() {
    let model = fold(claude_sequence());
    let messages = claude_messages(claude_layer(&model, CLAUDE_AGENT));
    let (kind, text) = &messages[1];
    assert_eq!(*kind, AgentMessageKind::Completed);
    assert_eq!(
        kind.presentation(),
        AgentMessagePresentation::Finished,
        "a finished mark, not the inbound arrow"
    );
    assert_eq!(text, REPORT, "the whole message the child ended on");
    let digest = message_digest(text);
    assert_eq!(digest.head, "migrated 14 call sites");
    assert_eq!(
        digest.hidden_lines, 2,
        "closing it hides the two lines that have anything on them"
    );
}

/// An exit is a notice: the sender is gone, the envelope carries no words,
/// and a row offering to open an empty body would be lying about what is
/// behind it.
#[test]
fn a2a_completed_row_renders_an_exit_as_a_notice() {
    let model = fold(claude_sequence());
    let messages = claude_messages(claude_layer(&model, CLAUDE_AGENT));
    let (kind, text) = &messages[2];
    assert_eq!(*kind, AgentMessageKind::Exited);
    assert_eq!(kind.presentation(), AgentMessagePresentation::Notice);
    assert_eq!(text, "", "the daemon sends an exit with an empty body");
    assert_eq!(message_digest(text).head, "");
    assert_eq!(message_digest(text).hidden_lines, 0);
}

/// An ordinary message stays an ordinary message: sender marker, then
/// everything it said.
#[test]
fn a2a_completed_row_leaves_a_message_alone() {
    let model = fold(claude_sequence());
    let messages = claude_messages(claude_layer(&model, CLAUDE_AGENT));
    let (kind, text) = &messages[0];
    assert_eq!(kind.presentation(), AgentMessagePresentation::Inbound);
    assert_eq!(text, "how far along?");
}

/// The same three envelopes reach a Codex parent by a different carrier
/// and make the same three rows. That is the whole point of deciding it in
/// the kernel: a completion cannot look finished in one chat and ordinary
/// in the other.
#[test]
fn a2a_completed_row_reads_the_same_in_both_layers() {
    let claude = fold(claude_sequence());
    let codex = fold(codex_sequence());
    let from_claude = claude_messages(claude_layer(&claude, CLAUDE_AGENT));
    let from_codex = codex_messages(codex_layer(&codex, CODEX_AGENT));
    assert_eq!(
        from_claude, from_codex,
        "one envelope vocabulary, two carriers"
    );
    for (kind, _) in &from_claude {
        assert_eq!(
            kind.presentation(),
            from_codex
                .iter()
                .find(|(other, _)| other == kind)
                .map(|(other, _)| other.presentation())
                .unwrap()
        );
    }
    assert_eq!(
        from_codex
            .iter()
            .map(|(kind, _)| kind.presentation())
            .collect::<Vec<_>>(),
        vec![
            AgentMessagePresentation::Inbound,
            AgentMessagePresentation::Finished,
            AgentMessagePresentation::Notice,
        ]
    );
}

/// A kind this build does not know, and a carrier that stated none, both
/// render as the message they plainly are — body included. The unknown is
/// in the label, not in the words somebody sent.
#[test]
fn a2a_completed_row_shows_an_unknown_kind_as_a_message() {
    for kind in [
        AgentMessageKind::Other {
            label: "teleported".to_string(),
        },
        AgentMessageKind::Unstated,
    ] {
        assert_eq!(kind.presentation(), AgentMessagePresentation::Inbound);
    }
}

/// Closing a body picks the first line with anything on it and says how
/// much stays behind. A one-line message hides nothing, so a renderer can
/// tell there is nothing to open.
#[test]
fn a2a_completed_row_closes_a_body_to_one_line() {
    let single = message_digest("done");
    assert_eq!(single.head, "done");
    assert_eq!(single.hidden_lines, 0, "nothing to open");

    let padded = message_digest("\n\n  done  \n\n\n");
    assert_eq!(
        padded.head, "  done",
        "leading blank lines are not the message; the indent is"
    );
    assert_eq!(
        padded.hidden_lines, 0,
        "nor is trailing whitespace something to open"
    );

    let spaced = message_digest("first\n\nsecond\n\nthird\n");
    assert_eq!(spaced.head, "first");
    assert_eq!(spaced.hidden_lines, 2, "blank separators are not lines");

    assert_eq!(message_digest("").head, "");
    assert_eq!(message_digest("   \n  ").head, "");
}

/// Neither a completion nor an exit asks anything of the human. They are
/// the child reporting, and only a state that needs a person raises
/// attention — being told your child finished is not the same as being
/// wanted.
#[test]
fn a2a_completed_row_raises_no_attention() {
    let quiet = fold(codex_base(CODEX_AGENT));
    let told = fold(codex_sequence());
    let attention = |model: &Model, agent: &str| model.agent(agent_id(agent)).unwrap().attention;
    assert_eq!(
        attention(&told, CODEX_AGENT),
        attention(&quiet, CODEX_AGENT),
        "three deliveries changed nothing about what the recipient needs"
    );
    // The Claude carrier delivers into the recipient's own transcript, so
    // the row it arrives on still does that row's turn bookkeeping. What
    // it may never do is claim a person is wanted.
    assert!(!matches!(
        attention(&fold(claude_sequence()), CLAUDE_AGENT),
        amux_ui::Attention::NeedsYou { .. }
    ));
    assert!(told.check_invariants().is_empty());
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("a2a_completed_row::claude", claude_sequence()),
        ("a2a_completed_row::codex", codex_sequence()),
    ]
}
