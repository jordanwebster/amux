//! Attachments as the feed shows them: a message's words without its
//! attachment elements, one focusable row per attachment, and what the
//! fold chord opens when that row is the focused one.
//!
//! Both native chats share this. An attachment means the same thing in
//! Claude's feed and Codex's, and the element that carries it is the
//! same element, so neither chat gets to decide what an image row says.

use amux_ui::attachments::{AttachmentIndex, AttachmentLine, Mention, MentionKind, Segment};
use amux_ui::{AgentId, Model};

use super::frame::BlockKey;

/// Attachment rows are blocks of their own so the feed can focus one and
/// open it. Their keys sit in a range no feed entry reaches: the top bit
/// tags an attachment row, the next bits name the message it hangs
/// under, and the low byte its position in that message.
const ATTACHMENT_TAG: u64 = 1 << 63;
const OWNER_MASK: u64 = (1 << 55) - 1;

/// Owner ids for the optimistic echoes, counting down from the top of
/// the owner range so a real entry id can never reach them.
const ECHO_OWNER_BASE: u64 = OWNER_MASK - 0xff;

/// The block key for one attachment of one message.
pub(crate) fn attachment_key(owner: u64, index: usize) -> BlockKey {
    BlockKey(ATTACHMENT_TAG | ((owner & OWNER_MASK) << 8) | (index as u64 & 0xff))
}

/// The owner id standing for the nth optimistic echo.
pub(crate) fn echo_owner(index: usize) -> u64 {
    ECHO_OWNER_BASE.saturating_sub(index as u64)
}

/// The words of a message, with every attachment element taken out.
///
/// The element is machinery — the agent reads it, a person reads the row
/// it produced — so it never reaches the markdown renderer.
pub(crate) fn prose(content: &[Segment]) -> String {
    content
        .iter()
        .filter_map(|segment| match segment {
            Segment::Prose(prose) => Some(prose.as_str()),
            Segment::Mention(_) => None,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The attachments a message carries, in the order it carries them.
pub(crate) fn mentions(content: &[Segment]) -> Vec<&Mention> {
    content
        .iter()
        .filter_map(|segment| match segment {
            Segment::Mention(mention) => Some(mention),
            Segment::Prose(_) => None,
        })
        .collect()
}

/// One paintable line per attachment, described from the stream alone.
pub(crate) fn described(index: &AttachmentIndex, content: &[Segment]) -> Vec<AttachmentLine> {
    mentions(content)
        .into_iter()
        .map(|mention| index.describe(mention))
        .collect()
}

/// What the fold chord does with an attachment row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// Fetch the bytes and hand them to the host's viewer.
    External(amux_ui::attachments::ArtifactId),
    /// Read it here: inline text has no file to open.
    Read { title: String, body: String },
    /// Read a review someone sent: the diff it cites is fetched, the
    /// comments came with the message.
    Review {
        header: Box<amux_ui::review::ReviewHeader>,
        comments: Vec<amux_ui::review::ReviewComment>,
    },
}

/// How this attachment opens, or `None` for a kind this build cannot
/// open yet.
pub(crate) fn opening(mention: &Mention) -> Option<Opening> {
    match &mention.kind {
        MentionKind::Image { id } | MentionKind::File { id } => Some(Opening::External(id.clone())),
        MentionKind::Text { body, .. } => Some(Opening::Read {
            title: mention.name.clone(),
            body: body.clone(),
        }),
        MentionKind::Review { header, comments } => Some(Opening::Review {
            header: Box::new(header.clone()),
            comments: comments.clone(),
        }),
    }
}

/// The attachment a focused block stands for, whichever chat it is in.
///
/// The key carries which message and which of its attachments, so the
/// answer is recomputed from the Model rather than remembered: a feed
/// that scrolled, reconciled an echo, or relinked its stream cannot
/// leave a stale attachment behind a live focus.
pub(crate) fn focused_mention(model: &Model, agent: AgentId, focus: BlockKey) -> Option<Mention> {
    if focus.0 & ATTACHMENT_TAG == 0 {
        return None;
    }
    let owner = (focus.0 >> 8) & OWNER_MASK;
    let index = (focus.0 & 0xff) as usize;
    let content = owner_content(model, agent, owner)?;
    mentions(&content)
        .get(index)
        .map(|mention| (*mention).clone())
}

fn owner_content(model: &Model, agent: AgentId, owner: u64) -> Option<Vec<Segment>> {
    if let Some(layer) = model.claude(agent) {
        if let Some(entry) = layer.entries().find(|entry| entry.id == owner) {
            return match &entry.kind {
                amux_ui::claude::FeedEntryKind::Prompt(prompt) => Some(prompt.content.clone()),
                amux_ui::claude::FeedEntryKind::Message(message) => Some(message.content.clone()),
                _ => None,
            };
        }
        let echo = layer
            .pending_echoes()
            .iter()
            .enumerate()
            .find(|(index, _)| echo_owner(*index) == owner)?;
        return Some(layer.attachments().segments(&echo.1.text));
    }
    if let Some(layer) = model.claude_sdk(agent) {
        if let Some(entry) = layer.entries().find(|entry| entry.id == owner) {
            return match &entry.kind {
                amux_ui::claude_sdk::FeedEntryKind::Prompt(prompt) => {
                    Some(layer.attachments().segments(&prompt.text))
                }
                amux_ui::claude_sdk::FeedEntryKind::Message(message) => {
                    Some(layer.attachments().segments(&message.text))
                }
                _ => None,
            };
        }
        let echo = layer.pending_echo().filter(|_| echo_owner(0) == owner)?;
        return Some(layer.attachments().segments(&echo.text));
    }
    let layer = model.codex(agent)?;
    let entry = layer.entries().find(|entry| entry.id == owner)?;
    match &entry.kind {
        amux_ui::codex::FeedEntryKind::Prompt(prompt) => Some(prompt.content.clone()),
        amux_ui::codex::FeedEntryKind::Message(message) => Some(message.content.clone()),
        _ => None,
    }
}
