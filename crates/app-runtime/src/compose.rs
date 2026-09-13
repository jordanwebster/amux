//! What a composer asks without a running runtime: how an attachment, a
//! review or a paste is spelled in a message, and what a picked file becomes.
//!
//! Spelled here rather than on each client for one reason: the element
//! escapes what would otherwise close it early, frames comment text by byte
//! length, and routes a paste by size, and a second spelling of any of that
//! would be a second thing to keep right.

use std::path::Path;

use model::RelayConnection;
use serde::Deserialize;
use serde_json::{Value, json};
use ui_state::{
    AgentId, ArtifactId, ArtifactKind, Command, DraftAttachment, Mention, MentionKind, PASTED_NAME,
    Pasted, Segment, format_mention, paste as route_paste, review, review_mention, split_mentions,
    text_mention,
};

use crate::projection::Projection;

/// What a review page hands the composer: the frozen document, the artifact it
/// is, and the comments written on it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewToken {
    pub diff: ArtifactId,
    pub document: review::ReviewDocument,
    pub comments: Vec<review::ReviewComment>,
}

/// Turns a written review into the token a composer holds: the canonical
/// attachment element to put in the message, and the artifact reference that
/// pins its frozen patch for whoever reads it. Answers
/// `{"element":"…","attachment":{…}}`.
pub fn review_element(token: ReviewToken) -> Value {
    let review = review::Review::with_comments(token.document, token.diff, token.comments);
    let (mention, attachment) = review_mention(&review);
    json!({
        "element": format_mention(&mention),
        "attachment": attachment,
    })
}

/// What a composer is attaching: an artifact already stored, named by its
/// identity, kind, display name and size.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentRequest {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub name: String,
    pub size: u64,
}

/// Spells one artifact attachment as the canonical element a message carries
/// it in.
pub fn attachment_element(request: AttachmentRequest) -> String {
    let kind = match request.kind {
        ArtifactKind::Image => MentionKind::Image { id: request.id },
        _ => MentionKind::File { id: request.id },
    };
    format_mention(&Mention {
        kind,
        name: request.name,
        size: Some(request.size),
        path: None,
    })
}

/// Routes a paste by size. `{"prose":"…"}` is text short enough to read in
/// place; `{"element":"…","lines":n,"name":"…"}` is a paste long enough to
/// bury the sentence around it, which becomes one atomic Text attachment.
pub fn paste(text: &str) -> Value {
    match route_paste(text) {
        Pasted::Prose(body) => json!({ "prose": body }),
        Pasted::Text { body, lines } => json!({
            "element": format_mention(&text_mention(body, lines)),
            "lines": lines,
            "name": PASTED_NAME,
        }),
    }
}

/// Splits message text into prose and the attachment elements it carries.
/// This is the parser the whole system reads attachments with, so what a
/// composer draws as a token is what will be sent.
pub fn attachments(text: &str) -> Vec<Segment> {
    split_mentions(text)
}

/// What the client knows about a file it just picked, before anything is
/// stored. Its identity is not among them: content identity is computed here
/// from the bytes, so a client can never name an artifact by an identity it
/// made up.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickedAttachment {
    pub agent: AgentId,
    pub kind: ArtifactKind,
    pub name: String,
    pub mime: String,
}

/// The command a picked file becomes.
pub fn picked_command(picked: PickedAttachment, bytes: Vec<u8>) -> Command {
    Command::PutAttachment {
        agent: picked.agent,
        attachment: DraftAttachment::from_bytes(picked.kind, picked.name, picked.mime, bytes),
    }
}

/// Folds a report's `msgs.jsonl` into the model it recorded and projects that
/// model as the event batch a running runtime would have delivered.
///
/// Nothing is connected, nothing is started and no effect the recording asked
/// for is carried out: the reducer folds the recorded messages and the
/// projection reads the result. The connection is not part of a recording, so
/// the projection is told the relay is connected and reconciliation follows
/// what the recorded model itself synchronized to. Every agent in the model is
/// subscribed, so a replay carries every conversation the recording held
/// rather than only the fleet.
pub fn replay_report(path: &Path) -> Result<Vec<crate::projection::Event>, String> {
    let model = ui_runtime::replay_msgs(path).map_err(|error| error.to_string())?;
    let mut projection = Projection::default();
    for card in model.agents() {
        projection.subscribe(card.agent.id);
    }
    let mut events = Vec::new();
    projection.outcomes(&model, &mut events);
    projection.collect(&model, &RelayConnection::Connected, &mut events);
    Ok(events)
}
