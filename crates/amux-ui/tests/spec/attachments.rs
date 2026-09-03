//! Attachment metadata is replayable stream state. Both native chat layers
//! consume the same row and mention grammar, so prompts and replies carry
//! typed segments without fetching artifact bytes.

use amux_artifacts::id_of;
use amux_ui::attachments::{AttachmentIndex, AttachmentKind, Mention, MentionKind, Segment};
use amux_ui::claude::FeedEntryKind as ClaudeEntry;
use amux_ui::codex::FeedEntryKind as CodexEntry;
use amux_ui::{
    ArtifactKind, ArtifactRef, Command, Effect, InputPayload, Msg, OpError, OpOutcome, Recorder,
    StreamMsg, format_mention,
};
use serde_json::{Value, json};

use crate::harness::*;

const CLAUDE: &str = "attachments-claude";
const CODEX: &str = "attachments-codex";
const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
const SESSION_B: &str = "22222222-2222-4222-8222-222222222222";

fn image_ref() -> ArtifactRef {
    ArtifactRef {
        id: id_of(b"image bytes"),
        kind: ArtifactKind::Image,
        name: "authoritative-shot.png".into(),
        mime: "image/png".into(),
        size: 12_043,
    }
}

fn attachment_row() -> Value {
    json!({
        "type": "amux.attachments",
        "input_id": "0123",
        "refs": [image_ref()],
    })
}

fn image_element() -> String {
    format_mention(&Mention {
        kind: MentionKind::Image { id: image_ref().id },
        // The refs row is authoritative over element metadata.
        name: "stale-name.png".into(),
        size: Some(1),
        path: Some("/agent/artifacts/image".into()),
    })
}

fn review_element() -> String {
    format_mention(&Mention {
        kind: MentionKind::Review {
            header: amux_ui::review::ReviewHeader {
                diff: id_of(b"patch"),
                base: "working-tree".into(),
                head: "abc123".into(),
                merge_base: None,
                blobs: vec![("src/main.rs".into(), "blob123".into())],
            },
            comments: vec![
                amux_ui::review::ReviewComment {
                    path: "src/main.rs".into(),
                    start_side: amux_ui::review::Side::Old,
                    start_line: 3,
                    side: amux_ui::review::Side::New,
                    line: 4,
                    quoted: vec!["-old".into(), "+new".into()],
                    text: "Keep the invariant.".into(),
                },
                amux_ui::review::ReviewComment {
                    path: "src/lib.rs".into(),
                    start_side: amux_ui::review::Side::New,
                    start_line: 9,
                    side: amux_ui::review::Side::New,
                    line: 9,
                    quoted: vec!["+added".into()],
                    text: "Please cover this.".into(),
                },
            ],
        },
        name: String::new(),
        size: None,
        path: None,
    })
}

fn claude_prompt(session: &str, text: &str, uuid: u128) -> Value {
    json!({
        "type": "user",
        "uuid": uuid::Uuid::from_u128(uuid).to_string(),
        "sessionId": session,
        "timestamp": "2026-09-03T10:00:00.000Z",
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": text},
    })
}

fn claude_reply(session: &str, text: &str, uuid: u128) -> Value {
    json!({
        "type": "assistant",
        "uuid": uuid::Uuid::from_u128(uuid).to_string(),
        "sessionId": session,
        "timestamp": "2026-09-03T10:00:01.000Z",
        "message": {
            "id": format!("msg-{uuid}"),
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}],
        },
    })
}

fn codex_prompt(text: &str) -> Value {
    json!({
        "type": "item/completed",
        "item": {
            "id": "user-attachment",
            "type": "userMessage",
            "content": [{"type": "text", "text": text}],
        },
    })
}

fn codex_reply(text: &str) -> Value {
    json!({
        "type": "item/completed",
        "item": {
            "id": "reply-attachment",
            "type": "agentMessage",
            "phase": "final_answer",
            "text": text,
        },
    })
}

fn assert_image_segment(index: &AttachmentIndex, content: &[Segment]) {
    let mention = content
        .iter()
        .find_map(|segment| match segment {
            Segment::Mention(mention) => Some(mention),
            Segment::Prose(_) => None,
        })
        .expect("attachment segment");
    assert_eq!(mention.name, "authoritative-shot.png");
    assert_eq!(mention.size, Some(12_043));
    assert!(matches!(mention.kind, MentionKind::Image { .. }));
    assert_eq!(
        index.describe(mention),
        amux_ui::AttachmentLine {
            kind: AttachmentKind::Image,
            name: "authoritative-shot.png".into(),
            size: Some(12_043),
            lines: None,
            comments: None,
            files: None,
        }
    );
}

fn draft_attachments() -> Vec<amux_ui::DraftAttachment> {
    vec![
        amux_ui::DraftAttachment::from_bytes(
            ArtifactKind::Image,
            "shot.png",
            "image/png",
            b"recorder must not keep these image bytes".to_vec(),
        ),
        amux_ui::DraftAttachment {
            id: id_of(b"review patch"),
            kind: ArtifactKind::Diff,
            name: "working-tree.diff".into(),
            mime: "text/x-diff".into(),
            size: 12,
            bytes: None,
        },
    ]
}

#[test]
fn attachments_claude_prompt_review_and_reply_fold_from_stream_rows() {
    let prompt_text = format!(
        "Please inspect {} and {}",
        image_element(),
        review_element()
    );
    let reply_text = format!("Attached result: {}", image_element());
    let rows = vec![
        attachment_row(),
        claude_prompt(SESSION_A, &prompt_text, 0x1301),
        claude_reply(SESSION_A, &reply_text, 0x1302),
    ];
    let model = fold(seq([chat_base(CLAUDE), vec![batch(CLAUDE, 10, rows)]]));
    let layer = claude_layer(&model, CLAUDE);

    let prompt = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            ClaudeEntry::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .expect("prompt entry");
    assert_image_segment(layer.attachments(), &prompt.content);
    let review = prompt
        .content
        .iter()
        .find_map(|segment| match segment {
            Segment::Mention(
                mention @ Mention {
                    kind: MentionKind::Review { .. },
                    ..
                },
            ) => Some(mention),
            _ => None,
        })
        .expect("review segment");
    assert_eq!(layer.attachments().describe(review).comments, Some(2));

    let reply = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            ClaudeEntry::Message(message) => Some(message),
            _ => None,
        })
        .expect("reply entry");
    assert_image_segment(layer.attachments(), &reply.content);
}

#[test]
fn attachments_codex_prompt_and_reply_use_the_same_segments() {
    let rows = vec![
        attachment_row(),
        codex_prompt(&format!("Inspect {}", image_element())),
        codex_reply(&format!("Here it is: {}", image_element())),
    ];
    let model = fold(seq([codex_base(CODEX), vec![batch(CODEX, 10, rows)]]));
    let layer = codex_layer(&model, CODEX);

    let prompt = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            CodexEntry::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .expect("prompt entry");
    assert_image_segment(layer.attachments(), &prompt.content);
    let reply = layer
        .entries()
        .find_map(|entry| match &entry.kind {
            CodexEntry::Message(message) => Some(message),
            _ => None,
        })
        .expect("reply entry");
    assert_image_segment(layer.attachments(), &reply.content);
}

#[test]
fn attachments_duplicate_rows_are_noops_and_relinks_refold_identically() {
    let initial = fold(seq([
        chat_base(CLAUDE),
        vec![batch(CLAUDE, 10, vec![attachment_row()])],
    ]));
    let before = claude_layer(&initial, CLAUDE).attachments().clone();
    let duplicate = fold(seq([
        chat_base(CLAUDE),
        vec![batch(CLAUDE, 10, vec![attachment_row(), attachment_row()])],
    ]));
    assert_eq!(before, *claude_layer(&duplicate, CLAUDE).attachments());

    let text = format!("After relink {}", image_element());
    let relinked = fold(seq([
        chat_base(CLAUDE),
        vec![batch(
            CLAUDE,
            10,
            vec![
                claude_prompt(SESSION_A, "old epoch", 0x1310),
                attachment_row(),
                claude_prompt(SESSION_B, &text, 0x1311),
            ],
        )],
    ]));
    let direct = fold(seq([
        chat_base(CLAUDE),
        vec![batch(
            CLAUDE,
            10,
            vec![attachment_row(), claude_prompt(SESSION_B, &text, 0x1311)],
        )],
    ]));
    assert_eq!(
        claude_layer(&relinked, CLAUDE).attachments(),
        claude_layer(&direct, CLAUDE).attachments()
    );
    let relinked_prompt = claude_layer(&relinked, CLAUDE)
        .entries()
        .find_map(|entry| match &entry.kind {
            ClaudeEntry::Prompt(prompt) => Some(&prompt.content),
            _ => None,
        })
        .expect("relinked prompt");
    let direct_prompt = claude_layer(&direct, CLAUDE)
        .entries()
        .find_map(|entry| match &entry.kind {
            ClaudeEntry::Prompt(prompt) => Some(&prompt.content),
            _ => None,
        })
        .expect("direct prompt");
    assert_eq!(relinked_prompt, direct_prompt);

    let reopened = fold(seq([
        codex_base(CODEX),
        vec![batch(
            CODEX,
            10,
            vec![attachment_row(), codex_prompt(&text)],
        )],
        vec![
            stream(CODEX, StreamMsg::Opened { truncated: false }),
            batch(CODEX, 20, vec![attachment_row(), codex_prompt(&text)]),
            stream(CODEX, StreamMsg::ReplayComplete),
        ],
    ]));
    let direct_codex = fold(seq([
        codex_base(CODEX),
        vec![batch(
            CODEX,
            20,
            vec![attachment_row(), codex_prompt(&text)],
        )],
    ]));
    assert_eq!(
        codex_layer(&reopened, CODEX),
        codex_layer(&direct_codex, CODEX),
        "a new observation window rebuilds the same attachment state"
    );
}

#[test]
fn attachments_fetched_diffs_have_an_independent_count_bound() {
    let mut index = AttachmentIndex::default();
    let ids: Vec<_> = (0..=amux_ui::attachments::FETCHED_DIFFS_RETAINED)
        .map(|n| id_of(format!("patch {n}").as_bytes()))
        .collect();
    for (n, id) in ids.iter().enumerate() {
        assert!(index.insert_diff(id.clone(), format!("patch {n}")));
    }
    assert!(index.diff(&ids[0]).is_none(), "oldest patch is evicted");
    assert_eq!(index.diff(ids.last().unwrap()), Some("patch 8"));
    assert!(
        !index.insert_diff(ids.last().unwrap().clone(), "patch 8".into()),
        "an identical fetch is idempotent"
    );
}

#[test]
fn attachments_send_is_one_put_then_send_with_every_pin_and_one_echo() {
    let attachments = draft_attachments();
    let command = Command::SendPromptWithAttachments {
        agent: agent_id(CLAUDE),
        text: "Review the image and patch.".into(),
        attachments: attachments.clone(),
    };
    let (model, effects) = fold_with_effects(seq([
        chat_base(CLAUDE),
        vec![crate::harness::command(op(13), command)],
    ]));
    let sends: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::PutThenSend {
                op,
                agent,
                puts,
                input,
                pin,
            } => Some((op, agent, puts, input, pin)),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 1, "one reducer send owns puts and delivery");
    let (effect_op, effect_agent, puts, input, pin) = sends[0];
    assert_eq!(*effect_op, op(13));
    assert_eq!(*effect_agent, agent_id(CLAUDE));
    assert_eq!(puts, &attachments);
    assert_eq!(
        pin,
        &attachments
            .iter()
            .map(|attachment| attachment.id.clone())
            .collect::<Vec<_>>(),
        "the existing review diff is pinned even though it has no bytes to put"
    );
    assert!(matches!(input, InputPayload::Claude { .. }));
    assert_eq!(
        claude_layer(&model, CLAUDE).pending_echoes().len(),
        1,
        "the provider's ordinary optimistic prompt semantics are reused"
    );
    let pending = model
        .pending_ops()
        .find(|pending| pending.op == op(13))
        .expect("send remains pending");
    let Command::SendPromptWithAttachments { attachments, .. } = &pending.command else {
        panic!("pending command retains its public kind");
    };
    assert!(
        attachments
            .iter()
            .all(|attachment| attachment.bytes.is_none()),
        "reducer state retains metadata, never payload bytes"
    );
}

#[test]
fn attachments_typed_send_failure_names_the_attachment_and_withdraws_echo() {
    let attachments = draft_attachments();
    let missing = attachments[0].clone();
    let model = fold(seq([
        chat_base(CLAUDE),
        vec![
            crate::harness::command(
                op(14),
                Command::SendPromptWithAttachments {
                    agent: agent_id(CLAUDE),
                    text: "Inspect this.".into(),
                    attachments,
                },
            ),
            op_result(
                op(14),
                OpOutcome::Error {
                    error: OpError::AttachmentMissing {
                        id: missing.id.clone(),
                        name: missing.name.clone(),
                    },
                },
            ),
        ],
    ]));
    assert!(claude_layer(&model, CLAUDE).pending_echoes().is_empty());
    let failure = model.finished_op(op(14)).expect("typed failure retained");
    let OpOutcome::Error {
        error: OpError::AttachmentMissing { id, name },
    } = &failure.outcome
    else {
        panic!("attachment failure stays typed");
    };
    assert_eq!(id, &missing.id);
    assert_eq!(name, "shot.png");
    assert!(failure.outcome.is_error());
}

#[test]
fn attachments_fetched_diff_outcome_lands_in_the_requesting_layer() {
    let id = id_of(b"fetched review patch");
    let model = fold(seq([
        codex_base(CODEX),
        vec![
            crate::harness::command(
                op(15),
                Command::FetchDiff {
                    agent: agent_id(CODEX),
                    id: id.clone(),
                },
            ),
            op_result(
                op(15),
                OpOutcome::DiffFetched {
                    id: id.clone(),
                    patch: "diff --git a/a b/a\n+new\n".into(),
                },
            ),
        ],
    ]));
    assert_eq!(
        codex_layer(&model, CODEX).attachments().diff(&id),
        Some("diff --git a/a b/a\n+new\n")
    );
}

#[test]
fn attachments_recorder_keeps_identity_and_size_but_redacts_bytes() {
    let attachments = draft_attachments();
    let command = Command::SendPromptWithAttachments {
        agent: agent_id(CLAUDE),
        text: "Inspect this.".into(),
        attachments: attachments.clone(),
    };
    let msg = crate::harness::command(op(16), command);
    let mut recorder = Recorder::new(4, &amux_ui::Model::default());
    recorder.record(&msg);
    let snapshot = recorder.snapshot();
    let recorded: Value = serde_json::from_str(&snapshot.msgs[0]).expect("recorded JSON");
    let first = &recorded["command"]["attachments"][0];
    assert_eq!(first["id"], attachments[0].id.to_string());
    assert_eq!(first["size"], attachments[0].size);
    assert!(first.get("bytes").is_none());
    assert!(
        !snapshot.msgs[0].contains("recorder must not keep these image bytes"),
        "payload bytes never enter the recorder line"
    );
    let replayed: Msg = serde_json::from_str(&snapshot.msgs[0]).expect("redacted Msg replays");
    let Msg::Command {
        command: Command::SendPromptWithAttachments { attachments, .. },
        ..
    } = replayed
    else {
        panic!("recorded attachment command shape");
    };
    assert_eq!(
        attachments[0].id,
        first["id"].as_str().unwrap().parse().unwrap()
    );
    assert_eq!(attachments[0].size, first["size"]);
    assert!(attachments[0].bytes.is_none());

    let disconnected = fold([msg]);
    let failure = disconnected.finished_op(op(16)).expect("failed fast");
    let Command::SendPromptWithAttachments { attachments, .. } = &failure.command else {
        panic!("failed command shape");
    };
    assert!(
        attachments
            .iter()
            .all(|attachment| attachment.bytes.is_none()),
        "even a synchronous refusal keeps reducer state byte-free"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let text = format!("Inspect {}", image_element());
    vec![
        (
            "Claude attachment metadata and mention fold",
            seq([
                chat_base(CLAUDE),
                vec![batch(
                    CLAUDE,
                    10,
                    vec![attachment_row(), claude_prompt(SESSION_A, &text, 0x1320)],
                )],
            ]),
        ),
        (
            "Codex attachment metadata and mention fold",
            seq([
                codex_base(CODEX),
                vec![batch(
                    CODEX,
                    10,
                    vec![attachment_row(), codex_prompt(&text)],
                )],
            ]),
        ),
        (
            "Attachment send state is recorder-stable",
            seq([
                chat_base(CLAUDE),
                vec![crate::harness::command(
                    op(17),
                    Command::SendPromptWithAttachments {
                        agent: agent_id(CLAUDE),
                        text: "Inspect this.".into(),
                        attachments: draft_attachments(),
                    },
                )],
            ]),
        ),
    ]
}
