//! Claude's provider-native facts shared by the PTY and SDK chat layers.
//!
//! This module translates Claude's own tool, question, plan, document, and
//! agent-message envelope vocabulary. It is deliberately not a normalized
//! agent representation: each Claude layer keeps its own lifecycle and feed
//! fold while reading these identical provider facts in one place.
//!
//! Ask-time documents (`docs/CHAT.md` §Diffs and the reader's documents)
//! are also derived here.
//!
//! Claude produces the ask-time preview computed here — the one place a
//! diff is ever computed, because at ask time the transcript
//! states no diff at all: the hook carries only `old_string`/`new_string`
//! (`docs/CHAT.md` §Unified diffs). Absolute line numbers are
//! unavailable at ask time (locating the snippet would require reading the
//! file, and a chat client can be relay-remote), so ask-time documents are
//! numberless and their magnitude is an ESTIMATE — exact for single-site
//! edits, wrong under `replace_all`, which therefore states
//! "replaces every occurrence" instead of counts.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use crate::diff::{Document, Hunk, Numbering};
use crate::model::AgentMessageKind;

/// The tool name Claude records for an amux message send. amux registers
/// its tools with the MCP server named `amux`, and Claude prefixes every
/// MCP tool the same way, so this is the name both Claude carriers expose.
const MCP_SEND_TOOL: &str = "mcp__amux__send";

/// Typed invocation facts per tool family, extracted tolerantly from
/// `tool_use.input` — absent fields are `None`, never an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolInvocation {
    Edit {
        file_path: Option<String>,
        replace_all: bool,
    },
    Write {
        file_path: Option<String>,
    },
    Bash {
        command: Option<String>,
        description: Option<String>,
    },
    Read {
        file_path: Option<String>,
    },
    /// The read/search family beyond `Read`: Grep, Glob, WebSearch,
    /// WebFetch, ToolSearch — one line, one query-ish string.
    Query {
        text: Option<String>,
    },
    /// An amux message sent to another agent (`mcp__amux__send`). The
    /// only amux tool with its own row shape: the rest are ordinary tool
    /// calls, but a message leaving for a named agent is the outbound half
    /// of a conversation and reads as one.
    AmuxSend {
        to: Option<String>,
        text: Option<String>,
    },
    /// Subagent spawn (`Task` / `Agent`).
    Task {
        description: Option<String>,
        subagent_type: Option<String>,
        background: bool,
    },
    /// `AskUserQuestion`; options are `{label, description}` objects.
    Question {
        questions: Vec<QuestionFact>,
    },
    /// `ExitPlanMode`: the plan payload rides `input.plan`.
    Plan {
        plan: Option<String>,
        plan_file_path: Option<String>,
    },
    /// A tool this build does not know: name-only rendering.
    Other,
}

impl ToolInvocation {
    pub(super) fn is_exploration(&self) -> bool {
        matches!(self, Self::Read { .. } | Self::Query { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionFact {
    pub header: Option<String>,
    pub question: Option<String>,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// An accepted plan retained as session state, keyed by tool-use id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedPlan {
    pub tool_use_id: String,
    pub plan: String,
}

/// Read Claude's typed tool input. Unknown tools and missing fields degrade
/// to explicit tolerant facts rather than failing either chat fold.
pub fn invocation(name: &str, input: &Value) -> ToolInvocation {
    match name {
        "Edit" => ToolInvocation::Edit {
            file_path: string_of(input, "file_path"),
            replace_all: bool_of(input, "replace_all"),
        },
        "Write" => ToolInvocation::Write {
            file_path: string_of(input, "file_path"),
        },
        "Bash" => ToolInvocation::Bash {
            command: string_of(input, "command"),
            description: string_of(input, "description"),
        },
        "Read" => ToolInvocation::Read {
            file_path: string_of(input, "file_path"),
        },
        "Grep" | "Glob" => ToolInvocation::Query {
            text: string_of(input, "pattern"),
        },
        "WebSearch" | "ToolSearch" => ToolInvocation::Query {
            text: string_of(input, "query"),
        },
        "WebFetch" => ToolInvocation::Query {
            text: string_of(input, "url"),
        },
        MCP_SEND_TOOL => ToolInvocation::AmuxSend {
            to: string_of(input, "to"),
            text: string_of(input, "text"),
        },
        "Task" | "Agent" => ToolInvocation::Task {
            description: string_of(input, "description"),
            subagent_type: string_of(input, "subagent_type"),
            background: bool_of(input, "run_in_background"),
        },
        "AskUserQuestion" => ToolInvocation::Question {
            questions: input
                .get("questions")
                .and_then(Value::as_array)
                .map(|questions| questions.iter().map(question_fact).collect())
                .unwrap_or_default(),
        },
        "ExitPlanMode" => ToolInvocation::Plan {
            plan: string_of(input, "plan"),
            plan_file_path: string_of(input, "planFilePath"),
        },
        _ => ToolInvocation::Other,
    }
}

fn question_fact(question: &Value) -> QuestionFact {
    QuestionFact {
        header: string_of(question, "header"),
        question: string_of(question, "question"),
        multi_select: bool_of(question, "multiSelect"),
        options: question
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| match option {
                        // Tolerate the older plain-string form too.
                        Value::String(label) => Some(QuestionOption {
                            label: label.clone(),
                            description: None,
                        }),
                        _ => string_of(option, "label").map(|label| QuestionOption {
                            label,
                            description: string_of(option, "description"),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn string_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn bool_of(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

/// How many unchanged lines surround a change in the computed preview —
/// jsdiff's own default, and what every observed `structuredPatch` uses.
const CONTEXT_LINES: usize = 3;

/// The `(+a -r)` counts with their epistemic status (the fact-vs-inferred
/// discipline applies to diffs too, diff-rendering §1.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "magnitude", rename_all = "snake_case")]
pub enum DiffMagnitude {
    /// Prefix counts over `structuredPatch` (FACT).
    Fact { added: u64, removed: u64 },
    /// Counts over the computed snippet diff (INFERRED — exact for
    /// single-site edits).
    Estimated { added: u64, removed: u64 },
    /// `replace_all`: N sites change and the snippet counts would lie —
    /// state the semantics instead.
    ReplacesEveryOccurrence,
}

/// Claude's epistemic statement wrapped around neutral diff facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffDocument {
    pub document: Document,
    pub magnitude: DiffMagnitude,
}

impl DiffDocument {
    /// Total diff body rows across all hunks — the remainder-line
    /// arithmetic's base (`⋮ +K more lines`).
    pub fn line_count(&self) -> usize {
        self.document.line_count()
    }
}

/// The typed body a permission ask carries (C2): computed once at ask
/// creation in the fold and retained WITH the ask — ask-time documents
/// live with their ask (evict bytes, never obligations), and nothing else
/// retains them in V1. Plan asks carry no entry here: the plan markdown
/// already rides `ToolInvocation::Plan`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskDocument {
    /// An Edit ask's mini-diff of `old_string` → `new_string`.
    Diff(DiffDocument),
    /// A Write ask's proposed content. Create-vs-overwrite is unknowable
    /// before the tool runs; the header claims neither.
    NewFile { content: String },
}

/// Derive the document carried by an Edit or Write permission request.
///
/// The raw input is required because the compact invocation facts do not
/// retain source snippets or proposed file contents. Keeping those bytes in
/// the ask document alone preserves the bounded PTY model while giving the
/// SDK layer the same one-time derivation.
pub fn ask_document(tool_name: Option<&str>, input: &Value) -> Option<AskDocument> {
    match tool_name {
        Some("Edit") => {
            let old = input.get("old_string").and_then(Value::as_str)?;
            let new = input.get("new_string").and_then(Value::as_str)?;
            Some(AskDocument::Diff(ask_time_diff(
                old,
                new,
                bool_of(input, "replace_all"),
            )))
        }
        Some("Write") => Some(AskDocument::NewFile {
            content: input.get("content").and_then(Value::as_str)?.to_string(),
        }),
        _ => None,
    }
}

/// The ask-time Edit preview: a line diff of the two snippets, grouped
/// with jsdiff-conventional context, numberless. Edit's uniqueness
/// requirement means `old_string` naturally carries its own context
/// lines, so the small snippet diff reads like a real hunk.
fn ask_time_diff(old: &str, new: &str, replace_all: bool) -> DiffDocument {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();
    let mut added = 0u64;
    let mut removed = 0u64;
    for group in diff.grouped_ops(CONTEXT_LINES) {
        let Some(first) = group.first() else { continue };
        let old_start = first.old_range().start as u32 + 1;
        let new_start = first.new_range().start as u32 + 1;
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => {
                        removed += 1;
                        '-'
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        '+'
                    }
                };
                let text = change.value();
                let text = text.strip_suffix('\n').unwrap_or(text);
                let text = text.strip_suffix('\r').unwrap_or(text);
                lines.push(format!("{sign}{text}"));
                // A ± row that does not end in a newline is a real
                // difference the stripped text can no longer show: an
                // edit adding only a final newline would otherwise render
                // as visually identical -/+ rows. State it the way jsdiff
                // and git do — a marker row the renderer shows dim
                // verbatim (its `\` prefix is outside the sign
                // vocabulary, so it takes no line number and the meta
                // tone). Context rows are exempt: an unchanged EOF
                // missing its newline on both sides states no difference
                // the approval needs.
                if change.missing_newline() && change.tag() != ChangeTag::Equal {
                    lines.push("\\ No newline at end of file".to_string());
                }
            }
        }
        hunks.push(Hunk {
            old_start,
            new_start,
            header: None,
            lines,
        });
    }
    DiffDocument {
        document: Document {
            numbering: Numbering::None,
            hunks,
            truncated: false,
        },
        magnitude: if replace_all {
            DiffMagnitude::ReplacesEveryOccurrence
        } else {
            DiffMagnitude::Estimated { added, removed }
        },
    }
}

/// What a recipient's own row still says about a message sent to it.
/// Every field but the text is what the carrier stated: absent fields stay
/// absent rather than being guessed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub id: Option<String>,
    pub context: Option<String>,
    /// The sender as the carrier addressed it: `name/host`, or `human`.
    pub from: String,
    pub kind: AgentMessageKind,
    pub text: String,
}

/// The sender an envelope names when its carrier omitted one.
const UNKNOWN_SENDER: &str = "unknown";

/// Read a user row's text as an agent message, or decide it is not one.
///
/// Both carriers hold the same rule: the wrapper is the row's own content,
/// never part of a sentence somebody wrote. The generic tag must therefore
/// be the whole row. Claude's native wrapper occupies whole lines inside
/// framing prose that may change between Claude releases.
pub fn inbound_message(text: &str) -> Option<InboundMessage> {
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
        kind: AgentMessageKind::read(attributes.remove("kind").as_deref()),
        text: unescape(body),
    })
}

fn read_cross_session(block: &str) -> Option<InboundMessage> {
    let (opening, body) = split(block, "</cross-session-message>")?;
    let mut attributes = attributes(opening.strip_prefix("<cross-session-message")?);
    // Claude's native peer channel carries messages amux did not send. Only
    // an `amux:` address is one of ours; anything else remains a normal row.
    let from = attributes
        .remove("from")?
        .strip_prefix("amux:")?
        .to_string();

    // amux's fields ride the first body line so one wrapper serves both
    // carriers. A body without that header is still a peer message; it just
    // tells us less.
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
        kind: AgentMessageKind::read(field("kind").as_deref()),
        text: unescape(text),
    })
}

/// The substring from `open` through the end of the first `close` after it,
/// but only when nothing else shares those two lines.
fn enclosed_in_whole_lines<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)?;
    let end = haystack[start..].find(close)? + start + close.len();
    let before = &haystack[..start];
    let before = before.rsplit_once('\n').map_or(before, |(_, last)| last);
    let after = &haystack[end..];
    let after = after.split_once('\n').map_or(after, |(first, _)| first);
    (before.trim().is_empty() && after.trim().is_empty()).then(|| &haystack[start..end])
}

/// An envelope's opening tag and its body, with formatter newlines removed.
fn split<'a>(tag: &'a str, closing: &str) -> Option<(&'a str, &'a str)> {
    let (opening, rest) = tag.split_once('>')?;
    let body = rest.strip_suffix(closing)?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    Some((opening, body))
}

/// Every `key="value"` pair in an opening tag. A malformed remainder ends
/// the scan rather than discarding pairs already read.
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

/// The unquoted, whitespace-separated `key=value` fields in `[amux …]`.
fn header_fields(header: &str) -> Vec<(String, String)> {
    header
        .split_ascii_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_string(), unescape(value)))
        .collect()
}

/// Undo the formatter's XML escaping. Unknown entities remain verbatim.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_json_round_trip(document: AskDocument, expected_keys: &[&str]) {
        let json = serde_json::to_string(&document).expect("ask document serializes");
        assert_eq!(
            json.matches("\"kind\":").count(),
            1,
            "the enum emits exactly one discriminator: {json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON object");
        let object = value.as_object().expect("ask document is an object");
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(keys, expected_keys, "one unambiguous key per field: {json}");
        assert_eq!(
            serde_json::from_str::<AskDocument>(&json).expect("ask document deserializes"),
            document
        );
    }

    #[test]
    fn every_ask_document_variant_round_trips_without_duplicate_json_keys() {
        assert_json_round_trip(
            AskDocument::Diff(ask_time_diff("before\n", "after\n", false)),
            &["document", "kind", "magnitude"],
        );
        assert_json_round_trip(
            AskDocument::NewFile {
                content: "fn main() {}\n".to_string(),
            },
            &["content", "kind"],
        );
    }

    #[test]
    fn a_single_site_edit_computes_one_context_grouped_hunk() {
        let old = "pub struct RetryConfig {\n    pub max_attempts: u8,\n    pub base_delay: Duration,\n}\n";
        let new = "pub struct RetryConfig {\n    pub max_attempts: u8,        // capped at 6\n    pub jitter_ms: u16,\n    pub base_delay: Duration,\n}\n";
        let document = ask_time_diff(old, new, false);
        assert_eq!(document.document.numbering, Numbering::None);
        assert_eq!(
            document.magnitude,
            DiffMagnitude::Estimated {
                added: 2,
                removed: 1
            }
        );
        assert_eq!(document.document.hunks.len(), 1);
        assert_eq!(
            document.document.hunks[0].lines,
            vec![
                " pub struct RetryConfig {",
                "-    pub max_attempts: u8,",
                "+    pub max_attempts: u8,        // capped at 6",
                "+    pub jitter_ms: u16,",
                "     pub base_delay: Duration,",
                " }",
            ],
            "unified rows with the prefix embedded, trailing newlines stripped"
        );
        assert_eq!(document.line_count(), 6);
    }

    #[test]
    fn distant_changes_group_into_separate_hunks() {
        let old: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 1\n", "line 1 changed\n")
            .replace("line 18\n", "line 18 changed\n");
        let document = ask_time_diff(&old, &new, false);
        assert_eq!(
            document.document.hunks.len(),
            2,
            "changes further apart than the context width form two hunks"
        );
        assert_eq!(document.document.hunks[0].old_start, 1);
        assert_eq!(
            document.document.hunks[1].old_start, 16,
            "the second hunk opens at its own context start"
        );
        assert_eq!(
            document.magnitude,
            DiffMagnitude::Estimated {
                added: 2,
                removed: 2
            }
        );
    }

    /// An edit differing only by a final newline must not render as
    /// visually identical -/+ rows: the missing-newline fact becomes the
    /// jsdiff/git marker row, so the user can tell what they are
    /// approving.
    #[test]
    fn a_newline_only_edit_states_the_missing_newline() {
        let document = ask_time_diff("VALUE=1", "VALUE=1\n", false);
        assert_eq!(document.document.hunks.len(), 1);
        assert_eq!(
            document.document.hunks[0].lines,
            vec!["-VALUE=1", "\\ No newline at end of file", "+VALUE=1",]
        );
        assert_eq!(
            document.magnitude,
            DiffMagnitude::Estimated {
                added: 1,
                removed: 1
            },
            "the marker is a statement, not a change row"
        );

        // The other direction: the NEW text is the one missing its
        // newline — the marker follows the added row.
        let document = ask_time_diff("VALUE=1\n", "VALUE=2", false);
        assert_eq!(
            document.document.hunks[0].lines,
            vec!["-VALUE=1", "+VALUE=2", "\\ No newline at end of file",]
        );
    }

    #[test]
    fn replace_all_states_semantics_instead_of_counts() {
        let document = ask_time_diff("a\n", "b\n", true);
        assert_eq!(document.magnitude, DiffMagnitude::ReplacesEveryOccurrence);
        assert_eq!(
            document.document.hunks.len(),
            1,
            "the snippet diff still renders"
        );
    }
}

/// One `permission_suggestions` entry, extracted tolerantly (unknown
/// suggestion kinds keep their tag and render generically).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionFact {
    pub kind: Option<SuggestionKind>,
    pub destination: Option<SuggestionDestination>,
    /// Directories for directory-grant suggestions.
    pub directories: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    AddDirectories,
    Unknown(String),
}

impl SuggestionKind {
    pub(crate) fn from_wire(kind: &str) -> Self {
        match kind {
            "addDirectories" => Self::AddDirectories,
            unknown => Self::Unknown(unknown.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionDestination {
    Session,
    Unknown(String),
}

impl SuggestionDestination {
    pub(crate) fn from_wire(destination: &str) -> Self {
        match destination {
            "session" => Self::Session,
            unknown => Self::Unknown(unknown.to_string()),
        }
    }
}
