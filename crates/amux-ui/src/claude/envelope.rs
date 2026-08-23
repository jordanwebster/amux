//! Reading an agent message out of a recipient's own transcript.
//!
//! amux never injects a synthetic provenance row: a message from another
//! agent arrives inside the text Claude records, wearing either the generic
//! `<amux …>` tag (bracketed-paste carrier) or Claude's own
//! `<cross-session-message from="amux:…">` envelope with an `[amux …]`
//! header line (inbox-socket carrier). Both are read here.
//!
//! Deliberately lenient, and one reader for both shapes. What reaches a
//! transcript has passed through a terminal, a queue and a harness whose
//! versions move weekly; a reader that answered "not an envelope" to
//! anything short of a perfect tag would launder a peer's message into
//! something that looks like the human speaking, which is the one outcome
//! the carrier design exists to prevent. So every field but the sender is
//! recovered where present and honestly absent where not. The reader's
//! agreement with `amux::envelope`'s formatter is a spec assertion, not an
//! assumption.

/// What a recipient's own row still says about a message sent to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboundMessage {
    pub id: Option<String>,
    pub context: Option<String>,
    /// The sender as the carrier addressed it: `name/host`, or `human`.
    pub from: String,
    pub kind: super::AgentMessageKind,
    pub text: String,
}

/// The sender an envelope names when its carrier omitted one.
const UNKNOWN_SENDER: &str = "unknown";

/// Read a user row's text as an agent message, or decide it is not one.
pub(crate) fn read(text: &str) -> Option<InboundMessage> {
    // Both carriers hold the same rule: the wrapper is the row's own
    // content, never part of a sentence somebody wrote. A human quoting
    // an envelope mid-sentence is a human speaking, and reading it as a
    // delivery would put another agent's name on what they said.
    //
    // The generic tag has nothing around it, so it must BE the row.
    // Claude's own wrapper arrives inside framing prose Claude adds — a
    // lead-in line above it and a paragraph of peer instructions below —
    // which is why the wrapper is looked for anywhere in the row rather
    // than matched against the whole of it. What holds instead is the
    // shape that prose has always had and a quoting sentence cannot: the
    // wrapper occupies whole lines. Pinning the prose itself would be
    // stricter and worse — it is Claude's, it changes with Claude's
    // releases, and a version that reworded it would make this reader
    // answer "not an envelope" to a real delivery, which is the one
    // failure the carrier design exists to prevent.
    if let Some(block) =
        enclosed_in_whole_lines(text, "<cross-session-message ", "</cross-session-message>")
    {
        return read_cross_session(block);
    }
    let trimmed = text.trim();
    trimmed
        .starts_with("<amux ")
        .then(|| read_amux(trimmed))
        .flatten()
}

fn read_amux(tag: &str) -> Option<InboundMessage> {
    let (opening, body) = split(tag, "</amux>")?;
    let mut attributes = attributes(opening.strip_prefix("<amux")?);
    Some(InboundMessage {
        id: attributes.remove("id"),
        context: attributes.remove("context"),
        from: attributes
            .remove("from")
            .unwrap_or_else(|| UNKNOWN_SENDER.to_string()),
        kind: super::AgentMessageKind::read(attributes.remove("kind").as_deref()),
        text: unescape(body),
    })
}

fn read_cross_session(block: &str) -> Option<InboundMessage> {
    let (opening, body) = split(block, "</cross-session-message>")?;
    let mut attributes = attributes(opening.strip_prefix("<cross-session-message")?);
    // Claude's native peer channel carries messages amux did not send.
    // Only an `amux:` address is one of ours; anything else stays whatever
    // the layer already made of it.
    let from = attributes
        .remove("from")?
        .strip_prefix("amux:")?
        .to_string();

    // amux's own fields ride the first body line so one wrapper serves
    // both carriers. A body without that header is still a peer message —
    // it just tells us less.
    let (header, text) = match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("[amux ") && first.ends_with(']') => {
            (header_fields(&first[6..first.len() - 1]), rest)
        }
        _ => (Vec::new(), body),
    };
    let field = |name: &str| {
        header
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    Some(InboundMessage {
        id: field("id"),
        context: field("context"),
        from,
        kind: super::AgentMessageKind::read(field("kind").as_deref()),
        text: unescape(text),
    })
}

/// The substring from `open` through the end of the first `close` after
/// it — but only when nothing else shares those two lines. Prose above and
/// below is the carrier's framing and is dropped; prose beside it means a
/// person wrote a sentence containing the tag, which is not a delivery.
fn enclosed_in_whole_lines<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)?;
    let end = haystack[start..].find(close)? + start + close.len();
    let before = &haystack[..start];
    let before = before.rsplit_once('\n').map_or(before, |(_, last)| last);
    let after = &haystack[end..];
    let after = after.split_once('\n').map_or(after, |(first, _)| first);
    (before.trim().is_empty() && after.trim().is_empty()).then(|| &haystack[start..end])
}

/// An envelope's opening tag and its body, with the newlines the formatter
/// puts around the body removed.
fn split<'a>(tag: &'a str, closing: &str) -> Option<(&'a str, &'a str)> {
    let (opening, rest) = tag.split_once('>')?;
    let body = rest.strip_suffix(closing)?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    Some((opening, body))
}

/// Every `key="value"` pair in an opening tag. A malformed remainder ends
/// the scan rather than discarding the pairs already read.
fn attributes(opening: &str) -> std::collections::BTreeMap<String, String> {
    let mut found = std::collections::BTreeMap::new();
    let mut rest = opening.trim_start();
    while let Some((key, remainder)) = rest.split_once('=') {
        let key = key.trim();
        let Some(quoted) = remainder.strip_prefix('"') else {
            break;
        };
        let Some(end) = quoted.find('"') else { break };
        if !key.is_empty() && !key.contains(char::is_whitespace) {
            found.insert(key.to_string(), unescape(&quoted[..end]));
        }
        rest = quoted[end + 1..].trim_start();
    }
    found
}

/// The `key=value` fields of the `[amux …]` header line, unquoted and
/// whitespace-separated.
fn header_fields(header: &str) -> Vec<(String, String)> {
    header
        .split_ascii_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_string(), unescape(value)))
        .collect()
}

/// Undo the formatter's XML escaping. An entity this build does not know
/// stays verbatim — losing a character is worse than showing an ampersand.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}
