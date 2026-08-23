//! Chapter 20 — Claude inbound: a message from another agent.
//!
//! amux injects no provenance row anywhere. A message from another agent
//! reaches the recipient inside the recipient's OWN text — the generic
//! `<amux …>` tag when the bracketed-paste carrier delivered it, Claude's
//! `<cross-session-message from="amux:…">` envelope when the inbox socket
//! did — and the layer reads it back out. Both fixtures below are graduated
//! captures of a real Claude 2.1.240 receiving one.
//!
//! The rule the chapter exists to hold: the row renders as the message it
//! is, never as a prompt. A peer may not borrow the human's voice.

use amux_ui::claude::{AgentMessageEntry, ClaudeLayer, FeedEntryKind};
use amux_ui::{AgentMessageKind, Model, Msg};

use crate::harness::*;

const AGENT: &str = "recipient";

/// The live inbox-socket capture: two envelopes, one delivered idle and
/// one mid-turn.
fn socket_sequence() -> Vec<Msg> {
    seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, a2a_rows("socket_delivery"))],
    ])
}

/// The live bracketed-paste capture.
fn pty_sequence() -> Vec<Msg> {
    seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, a2a_rows("pty_delivery"))],
    ])
}

/// A product-shaped envelope of each carrier, formatted by the daemon's own
/// formatter, so the reader is pinned against the writer rather than
/// against a hand-written guess at the format.
fn formatted(
    kind: amux::envelope::EnvelopeKind,
    context: Option<uuid::Uuid>,
) -> amux::envelope::Envelope {
    amux::envelope::Envelope {
        id: uuid::Uuid::from_u128(0xa2a),
        context,
        from: amux::envelope::Sender::Agent(amux::envelope::AgentSender {
            agent_id: uuid::Uuid::from_u128(0xbeef),
            host_id: uuid::Uuid::from_u128(0xcafe),
            name: "lead".to_string(),
            kind: "claude".to_string(),
        }),
        to: amux::AgentParent {
            agent_id: uuid::Uuid::from_u128(0xfeed),
            host_id: uuid::Uuid::from_u128(0xcafe),
        },
        kind,
        text: "review the <patch> & say if it's ok".to_string(),
    }
}

fn user_row(text: &str, meta: bool) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "isMeta": meta,
        "origin": if meta { serde_json::json!({"kind": "peer"}) }
                  else { serde_json::json!({"kind": "human"}) },
        "promptSource": if meta { serde_json::Value::Null }
                        else { serde_json::json!("typed") },
        "message": {"role": "user", "content": text},
    })
}

fn messages(layer: &ClaudeLayer) -> Vec<AgentMessageEntry> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::AgentMessage(message) => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn prompts(layer: &ClaudeLayer) -> Vec<String> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Prompt(prompt) => Some(prompt.text.clone()),
            _ => None,
        })
        .collect()
}

/// The inbox carrier's rows are Claude's own peer envelopes, wrapped in its
/// framing prose. The layer reads the amux address and the body back out of
/// them — including the one delivered while the session was busy.
#[test]
fn a2a_claude_inbound_reads_the_socket_carrier() {
    let model = fold(socket_sequence());
    let layer = claude_layer(&model, AGENT);
    let inbound = messages(layer);
    assert_eq!(inbound.len(), 2, "one idle delivery, one mid-turn");
    for message in &inbound {
        assert_eq!(message.from, "probe/host", "the amux address, unprefixed");
    }
    let bodies: Vec<&str> = inbound.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(
        bodies,
        vec!["A2A_SOCKET_IDLE_21240", "A2A_SOCKET_BUSY_21240"]
    );
    assert!(
        !inbound[0].text.contains("cross-session-message"),
        "the wrapper is carrier framing, not the message"
    );
    assert!(
        !inbound[0].text.contains("Another Claude session"),
        "nor is Claude's own explanatory prose: {:?}",
        inbound[0].text
    );
}

/// The bracketed-paste carrier's row wears the human discriminators the
/// terminal gave it (`origin.kind: human`, `promptSource: typed`) because
/// nothing else can wear them. The layer refuses to be fooled: the tag is a
/// message, and no prompt claims the human typed it.
#[test]
fn a2a_claude_inbound_never_becomes_a_human_prompt() {
    let model = fold(pty_sequence());
    let layer = claude_layer(&model, AGENT);
    let inbound = messages(layer);
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].from, "probe/host");
    assert_eq!(inbound[0].text, "A2A_PTY_IDLE_21240");
    assert!(
        !prompts(layer).iter().any(|text| text.contains("<amux")),
        "an envelope must never render as something the human said: {:?}",
        prompts(layer)
    );
}

/// The recipient IS working once the harness runs a turn on the delivered
/// text, so the turn bookkeeping follows the row's own discriminators. Not
/// a human turn — a turn.
#[test]
fn a2a_claude_inbound_still_opens_the_turn_the_delivery_started() {
    let pasted = fold(seq([
        chat_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![user_row(
                &amux::envelope::format(&formatted(amux::envelope::EnvelopeKind::Message, None)),
                false,
            )],
        )],
    ]));
    assert_eq!(
        amux_ui::claude::phase(&pasted, agent_id(AGENT)),
        amux_ui::claude::ChatPhase::Working,
        "a pasted delivery the terminal ran must not read as idle"
    );

    // The inbox carrier's row states no turn of its own: Claude queues it
    // and its own rows report what follows.
    let posted = fold(seq([
        chat_base(AGENT),
        vec![batch(
            AGENT,
            10,
            vec![user_row(
                &amux::envelope::format_cross_session(
                    &formatted(amux::envelope::EnvelopeKind::Message, None),
                    "prompting",
                )
                .expect("an agent sender formats"),
                true,
            )],
        )],
    ]));
    assert_eq!(messages(claude_layer(&posted, AGENT)).len(), 1);
    assert_ne!(
        amux_ui::claude::phase(&posted, agent_id(AGENT)),
        amux_ui::claude::ChatPhase::Working,
    );
}

/// Reader and writer agree on both carriers: every field the daemon
/// formatted comes back, escaping included. Two derivations of one format
/// exist, so their agreement is asserted rather than each one's
/// correctness.
#[test]
fn a2a_claude_inbound_agrees_with_the_daemon_formatter() {
    let envelope = formatted(
        amux::envelope::EnvelopeKind::Completed,
        Some(uuid::Uuid::from_u128(0xc0)),
    );
    let carriers = [
        user_row(&amux::envelope::format(&envelope), false),
        user_row(
            &amux::envelope::format_cross_session(&envelope, "prompting")
                .expect("an agent sender formats"),
            true,
        ),
    ];
    for row in carriers {
        let model = fold(seq([chat_base(AGENT), vec![batch(AGENT, 10, vec![row])]]));
        let inbound = messages(claude_layer(&model, AGENT));
        assert_eq!(inbound.len(), 1);
        let message = &inbound[0];
        assert_eq!(
            message.id.as_deref(),
            Some(envelope.id.to_string().as_str())
        );
        assert_eq!(
            message.context.as_deref(),
            Some(envelope.context.unwrap().to_string().as_str())
        );
        assert_eq!(message.kind, AgentMessageKind::Completed);
        assert_eq!(
            message.text, envelope.text,
            "the body survives escaping in both directions"
        );
        assert!(
            message.from.starts_with("lead/"),
            "the sender the daemon authored: {}",
            message.from
        );
    }
}

/// A carrier that stated less than the format allows still renders. The
/// live paste capture is exactly this case — a probe tag with no `kind` —
/// and honest absence beats a guess.
#[test]
fn a2a_claude_inbound_degrades_to_what_the_carrier_stated() {
    let model = fold(pty_sequence());
    let inbound = messages(claude_layer(&model, AGENT));
    assert_eq!(inbound[0].kind, AgentMessageKind::Unstated);
    assert_eq!(inbound[0].id.as_deref(), Some("idle"));
    assert_eq!(inbound[0].context, None);
}

/// Claude's peer channel carries messages amux did not send. Only an
/// `amux:` address is one of ours; the rest stays what the layer already
/// made of it.
#[test]
fn a2a_claude_inbound_ignores_a_peer_message_amux_did_not_send() {
    let foreign = "<cross-session-message from=\"other-claude\" from-name=\"other\" \
                   from-mode=\"prompting\">\nhello\n</cross-session-message>";
    let model = fold(seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, vec![user_row(foreign, true)])],
    ]));
    assert!(messages(claude_layer(&model, AGENT)).is_empty());
}

/// A human who writes about an amux tag is a human writing.
#[test]
fn a2a_claude_inbound_leaves_a_quoted_tag_a_prompt() {
    let quoted = "why does <amux id=\"x\" from=\"y\">hi</amux> not parse?";
    let model = fold(seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, vec![user_row(quoted, false)])],
    ]));
    let layer = claude_layer(&model, AGENT);
    assert!(messages(layer).is_empty());
    assert_eq!(prompts(layer), vec![quoted.to_string()]);
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("a2a_claude_inbound::socket", socket_sequence()),
        ("a2a_claude_inbound::pty", pty_sequence()),
    ]
}

/// Every fixture row folds without the layer inventing an unknown shape.
#[test]
fn a2a_claude_inbound_folds_both_captures_coherently() {
    for (_, sequence) in sequences() {
        let model: Model = fold(sequence);
        assert!(model.check_invariants().is_empty());
    }
}
