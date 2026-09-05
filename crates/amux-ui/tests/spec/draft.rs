//! What a draft becomes when it is sent.
//!
//! A draft itself is client state — the terminal owns its text, its
//! tokens and its cursor — but everything the draft *promises* is settled
//! here: which attachments become puts, which merely get pinned, what the
//! pin list of one send may contain, and when a send is allowed at all.
//! The composer's own rules (a token deleted by one backspace, a kill that
//! takes text and tokens together) are proved where the composer lives;
//! this chapter proves that nothing the composer did can leak into a
//! later send.

use amux_artifacts::id_of;
use amux_ui::attachments::{Mention, MentionKind};
use amux_ui::{ArtifactKind, Command, DraftAttachment, Effect, Msg, OpId, format_mention};
use serde_json::json;

use crate::harness::*;

const AGENT: &str = "draft-claude";

fn image() -> DraftAttachment {
    DraftAttachment::from_bytes(
        ArtifactKind::Image,
        "screenshot.png",
        "image/png",
        b"pretend png bytes".to_vec(),
    )
}

fn logfile() -> DraftAttachment {
    DraftAttachment::from_bytes(
        ArtifactKind::File,
        "trace.log",
        "text/plain",
        b"pretend log bytes".to_vec(),
    )
}

fn artifact_element(attachment: &DraftAttachment) -> String {
    let kind = match attachment.kind {
        ArtifactKind::Image => MentionKind::Image {
            id: attachment.id.clone(),
        },
        _ => MentionKind::File {
            id: attachment.id.clone(),
        },
    };
    format_mention(&Mention {
        kind,
        name: attachment.name.clone(),
        size: Some(attachment.size),
        path: None,
    })
}

fn pasted_element() -> String {
    format_mention(&Mention {
        kind: MentionKind::Text {
            lines: 240,
            body: (1..=240).map(|n| format!("stack frame {n}\n")).collect(),
        },
        name: "pasted-1".into(),
        size: None,
        path: None,
    })
}

/// What the terminal hands the reducer once the person presses Enter:
/// the draft's text with every token replaced by its canonical element,
/// plus the artifact-backed attachments in draft order.
fn send(op_id: OpId, text: String, attachments: Vec<DraftAttachment>) -> Msg {
    crate::harness::command(
        op_id,
        Command::SendPromptWithAttachments {
            agent: agent_id(AGENT),
            text,
            attachments,
        },
    )
}

fn put_then_send(effects: &[Effect]) -> (&Vec<DraftAttachment>, &Vec<amux_ui::ArtifactId>) {
    let mut found = effects.iter().filter_map(|effect| match effect {
        Effect::PutThenSend { puts, pin, .. } => Some((puts, pin)),
        _ => None,
    });
    let first = found.next().expect("a draft with attachments sends once");
    assert!(found.next().is_none(), "one draft is one send");
    first
}

/// A draft may hold attachments of different kinds at once, and the kind
/// decides how each one travels: an image is bytes the daemon does not
/// have yet, so it is put and then pinned; pasted text is already in the
/// prompt, so it rides the words and is never stored.
#[test]
fn draft_puts_artifact_attachments_and_carries_inline_ones_in_the_text() {
    let image = image();
    let text = format!(
        "compare {} against the trace {}",
        artifact_element(&image),
        pasted_element(),
    );
    let (_, effects) = fold_with_effects(seq([
        chat_base(AGENT),
        vec![send(op(20), text.clone(), vec![image.clone()])],
    ]));

    let (puts, pin) = put_then_send(&effects);
    assert_eq!(puts, &vec![image.clone()], "only bytes the daemon lacks");
    assert_eq!(pin, &vec![image.id.clone()]);
    assert!(
        text.contains("kind=\"text\""),
        "the pasted text travels as an element inside the prompt"
    );
}

/// Deleting a token before sending drops that attachment and nothing
/// else. The pin list is derived from the command the draft exported, so
/// a send can never carry an attachment an earlier draft held: there is
/// no reducer state for a deletion to have to undo.
#[test]
fn draft_send_pins_exactly_its_own_attachments() {
    let image = image();
    let logfile = logfile();
    let both = format!(
        "both: {} {}",
        artifact_element(&image),
        artifact_element(&logfile)
    );
    let (_, effects) = fold_with_effects(seq([
        chat_base(AGENT),
        vec![send(op(21), both, vec![image.clone(), logfile.clone()])],
    ]));
    let (_, pin) = put_then_send(&effects);
    assert_eq!(pin, &vec![image.id.clone(), logfile.id.clone()]);

    // The person deleted the image's token and sent what was left.
    let (_, effects) = fold_with_effects(seq([
        chat_base(AGENT),
        vec![send(
            op(22),
            format!("just the log: {}", artifact_element(&logfile)),
            vec![logfile.clone()],
        )],
    ]));
    let (puts, pin) = put_then_send(&effects);
    assert_eq!(puts, &vec![logfile.clone()], "the deleted image is gone");
    assert_eq!(pin, &vec![logfile.id.clone()]);
}

/// Attaching costs nothing until Enter. An agent can be attaching things
/// of its own while a person builds a draft, and not one byte of that
/// draft leaves the client until the person sends it.
#[test]
fn draft_attachments_reach_no_effect_before_the_send() {
    let refs_row = json!({
        "type": "amux.attachments",
        "input_id": null,
        "refs": [{
            "id": id_of(b"agent side bytes"),
            "kind": "file",
            "name": "coverage.html",
            "mime": "text/html",
            "size": 20_000,
        }],
    });
    let (_, effects) = fold_with_effects(seq([
        chat_base(AGENT),
        vec![batch(AGENT, 10, vec![refs_row])],
    ]));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::PutThenSend { .. })),
        "nothing is put until a draft is sent"
    );
}

/// A takeover and the return from it. While the agent's own question
/// owns the screen the gate names that question, so a draft holding
/// attachments simply waits; once the question is answered the question
/// no longer gates anything, and the draft that waited sends with its
/// whole pin list intact.
#[test]
fn draft_waits_for_a_takeover_and_sends_unchanged_after_it() {
    let image = image();
    let taken_over = fold(chat_feed_through(
        AGENT,
        "question_mixed",
        ChatAnchor::PermissionRequest(0),
    ));
    assert_eq!(
        amux_ui::claude::send_gate(&taken_over, agent_id(AGENT)).refusal(),
        Some("send gated — answer the pending ask"),
        "an open ask owns the screen, so the draft cannot send yet"
    );

    let returned = fold(chat_feed(AGENT, "question_mixed"));
    assert_eq!(
        claude_layer(&returned, AGENT).ask_count(),
        0,
        "the answered question hands the screen back"
    );
    assert_eq!(
        amux_ui::claude::send_gate(&returned, agent_id(AGENT)).refusal(),
        None,
        "with the question answered the draft may send again"
    );

    let (_, effects) = fold_with_effects(seq([
        chat_feed(AGENT, "question_mixed"),
        vec![send(
            op(23),
            format!("now look: {}", artifact_element(&image)),
            vec![image.clone()],
        )],
    ]));
    let (puts, pin) = put_then_send(&effects);
    assert_eq!(puts, &vec![image.clone()]);
    assert_eq!(pin, &vec![image.id]);
}

/// A refused send loses nothing. When the session cannot take input the
/// reducer refuses before a single byte leaves: no put, no half-delivered
/// prompt, so the terminal can put the whole draft back — its words and
/// its tokens — exactly as the person left it.
#[test]
fn draft_refused_by_the_gate_puts_nothing() {
    let image = image();
    let (model, effects) = fold_with_effects(seq([
        // This fixture leaves the session working, which refuses input.
        chat_feed(AGENT, "permission"),
        vec![send(
            op(25),
            format!("look at this {}", artifact_element(&image)),
            vec![image],
        )],
    ]));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::PutThenSend { .. })),
        "a refused send puts nothing"
    );
    assert!(
        model
            .finished_op(op(25))
            .expect("the refusal is recorded")
            .outcome
            .is_error(),
        "the draft learns why, rather than disappearing"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let image = image();
    vec![(
        "a draft of two kinds sent as one input",
        seq([
            chat_base(AGENT),
            vec![send(
                op(24),
                format!(
                    "compare {} against {}",
                    artifact_element(&image),
                    pasted_element()
                ),
                vec![image],
            )],
        ]),
    )]
}
