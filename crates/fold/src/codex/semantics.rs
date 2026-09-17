//! Durable Codex fold semantics.

use std::mem::size_of;

use chrono::{DateTime, Utc};
use model::{
    AgentMessageKind, AgentPhase, Attention, ContextMeter, ContextMeterSource, StructuredProtocol,
    Summary, SummaryField, Why,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TokenUsage;
use crate::{
    Baseline, Changes, Component, ComponentSource, Components, Entry, EntryKey, FieldPatch, Input,
    JsonBytes, MergeDefect, Mutation, Order, Patch, PostcardSafe, ProviderFold, RestoreRow,
    Revision, SegmentId, TIP_MAX_BYTES, TIP_MAX_OPEN_ENTRIES, VersionedField,
};

const VALUE_MAX: usize = 64 * 1024;
const TEXT_MAX: usize = 64 * 1024;
pub const DELIVERY_KEYED_VARIANTS: &[&str] = &[
    "codex.turn_snapshot_without_turn_id",
    "codex.user_input_without_item_id",
    "codex.mcp_startup",
    "codex.agent_message_without_envelope_id",
    "codex.resumed_boundary",
    "codex.ready_boundary",
    "codex.gap_boundary",
    "codex.error",
    "codex.unrecognized",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexEntryKind {
    Prompt,
    Message,
    Reasoning,
    Work,
    McpStartup,
    AgentMessage,
    Turn,
    Boundary,
    Error,
    #[default]
    Unrecognized,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexBody {
    #[default]
    None,
    Item {
        item_id: String,
        item_type: String,
    },
    Steer {
        input_id: String,
    },
    Turn {
        turn_id: String,
        status: String,
        token_usage: Option<TokenUsage>,
    },
    Snapshot {
        turn_id: Option<String>,
        kind: String,
    },
    AgentMessage {
        id: Option<String>,
        context: Option<String>,
        from: String,
        kind: AgentMessageKind,
        delivery: Option<String>,
    },
    Boundary {
        kind: String,
    },
    Error {
        severity: String,
        will_retry: bool,
    },
    Unrecognized {
        method: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalText {
    pub through: u64,
    pub revision: Revision,
    pub values: Vec<Component<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexPartial {
    pub kind: FieldPatch<CodexEntryKind>,
    pub body: FieldPatch<CodexBody>,
    pub text: FieldPatch<String>,
    pub components: Vec<Component<String>>,
    pub final_text: Option<FinalText>,
    pub finality: FieldPatch<String>,
    pub state: FieldPatch<String>,
    pub details: FieldPatch<JsonBytes>,
    pub clipped: FieldPatch<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexEntry {
    kind: VersionedField<CodexEntryKind>,
    body: VersionedField<CodexBody>,
    text: VersionedField<String>,
    components: Components<String>,
    finality: VersionedField<String>,
    state: VersionedField<String>,
    details: VersionedField<JsonBytes>,
    clipped: VersionedField<bool>,
    rendered_text: String,
}

impl CodexEntry {
    pub fn entry_kind(&self) -> Option<CodexEntryKind> {
        self.kind.value().copied()
    }
    pub fn body(&self) -> Option<&CodexBody> {
        self.body.value()
    }
    pub fn state(&self) -> Option<&str> {
        self.state.value().map(String::as_str)
    }
    pub fn details(&self) -> Option<&JsonBytes> {
        self.details.value()
    }
    pub fn finality(&self) -> Option<&str> {
        self.finality.value().map(String::as_str)
    }
    pub fn is_clipped(&self) -> bool {
        self.clipped.value().copied().unwrap_or(false)
    }
    fn rebuild(&mut self) {
        if self.components.values().is_empty() {
            self.rendered_text = self.text.value().cloned().unwrap_or_default();
        } else {
            let mut values = self.components.values().to_vec();
            values.sort_by_key(|value| value.observed_at);
            self.rendered_text = values.into_iter().map(|value| value.value).collect();
        }
    }
}

impl Entry for CodexEntry {
    type Partial = CodexPartial;
    fn kind(&self) -> &'static str {
        match self.entry_kind() {
            Some(CodexEntryKind::Prompt) => "prompt",
            Some(CodexEntryKind::Message) => "message",
            Some(CodexEntryKind::Reasoning) => "reasoning",
            Some(CodexEntryKind::Work) => "work",
            Some(CodexEntryKind::McpStartup) => "mcp_startup",
            Some(CodexEntryKind::AgentMessage) => "agent_message",
            Some(CodexEntryKind::Turn) => "turn",
            Some(CodexEntryKind::Boundary) => "boundary",
            Some(CodexEntryKind::Error) => "error",
            Some(CodexEntryKind::Unrecognized) | None => "unrecognized",
        }
    }
    fn text(&self) -> Option<&str> {
        (!self.rendered_text.is_empty()).then_some(&self.rendered_text)
    }
    fn merge(&mut self, patch: &Self::Partial) -> Result<(), MergeDefect> {
        self.kind.merge("kind", &patch.kind)?;
        self.body.merge("body", &patch.body)?;
        self.text.merge("text", &patch.text)?;
        self.finality.merge("finality", &patch.finality)?;
        self.state.merge("state", &patch.state)?;
        self.details.merge("details", &patch.details)?;
        self.clipped.merge("clipped", &patch.clipped)?;
        for component in &patch.components {
            self.components.merge(component.clone())?;
        }
        if let Some(final_text) = &patch.final_text {
            self.components.replace_final(
                final_text.through,
                final_text.revision,
                final_text.values.clone(),
            )?;
        }
        self.rebuild();
        Ok(())
    }
    fn from_partial(patch: &Self::Partial) -> Result<Self, MergeDefect> {
        let mut entry = Self::default();
        entry.merge(patch)?;
        Ok(entry)
    }
    fn merge_alias(
        &mut self,
        source: &Self,
        _promotion: Option<crate::Promotion>,
    ) -> Result<(), MergeDefect> {
        self.kind.fill_unknown_from(&source.kind);
        self.body.fill_unknown_from(&source.body);
        self.text.fill_unknown_from(&source.text);
        self.finality.fill_unknown_from(&source.finality);
        self.state.fill_unknown_from(&source.state);
        self.details.fill_unknown_from(&source.details);
        self.clipped.fill_unknown_from(&source.clipped);
        for component in source.components.values() {
            self.components.merge(component.clone())?;
        }
        self.rebuild();
        Ok(())
    }
    fn promote(&mut self, _promotion: Option<crate::Promotion>) -> Result<(), MergeDefect> {
        Ok(())
    }
    fn clip(&mut self, budget: usize) {
        self.components.clip_by(256, budget / 2, |component| {
            component.value.len() + component.after.capacity() * size_of::<ComponentSource>()
        });
        truncate_string(&mut self.text, TEXT_MAX);
        truncate_bytes(&mut self.details, VALUE_MAX);
        self.rebuild();
        if self.bytes() > budget {
            self.rendered_text = clipped(&self.rendered_text, budget / 4);
        }
    }
    fn bytes(&self) -> usize {
        postcard::to_allocvec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexFold {
    segment: SegmentId,
    baseline: Baseline,
    through: u64,
    asks: Vec<PendingAsk>,
    pending_approval_context: Option<RestoreRow>,
    attention: Attention,
    phase: AgentPhase,
    last_activity: Option<DateTime<Utc>>,
    token_usage: Option<TokenUsage>,
    model: Option<String>,
    ready_seen: bool,
    known_attention: bool,
    known_phase: bool,
    known_last_activity: bool,
    known_context: bool,
    known_model: bool,
    known_outstanding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingAsk {
    request_id: String,
    rows: Vec<RestoreRow>,
}

impl Default for CodexFold {
    fn default() -> Self {
        Self {
            segment: 0,
            baseline: Baseline::Start,
            through: 0,
            asks: Vec::new(),
            pending_approval_context: None,
            attention: Attention::Unknown,
            phase: AgentPhase::Running,
            last_activity: None,
            token_usage: None,
            model: None,
            ready_seen: false,
            known_attention: false,
            known_phase: false,
            known_last_activity: false,
            known_context: false,
            known_model: false,
            known_outstanding: true,
        }
    }
}

impl CodexFold {
    pub fn through(&self) -> u64 {
        self.through
    }

    pub fn restored_attention(&self) -> Option<Attention> {
        self.known_attention.then_some(self.attention)
    }

    pub fn restored_outstanding_known(&self) -> bool {
        self.known_outstanding
    }

    pub fn restored_obligations(&self) -> impl Iterator<Item = &RestoreRow> {
        self.asks.iter().flat_map(|ask| ask.rows.iter())
    }

    fn row(
        &mut self,
        seq: u64,
        activity_at: Option<DateTime<Utc>>,
        row: &Value,
    ) -> Vec<Mutation<CodexEntry>> {
        self.through = self.through.max(seq);
        if !self.known_phase {
            self.known_phase = true;
            self.phase = AgentPhase::Running;
        }
        if let Some(at) = activity_at {
            self.last_activity = Some(at);
            self.known_last_activity = true;
        }
        let revision = Revision::row(seq);
        let method = row.get("type").and_then(Value::as_str).unwrap_or("");
        if method != "amux.codex_approval_required"
            && !matches!(
                method,
                "item/commandExecution/requestApproval"
                    | "item/fileChange/requestApproval"
                    | "item/permissions/requestApproval"
                    | "item/tool/call"
                    | "item/tool/requestUserInput"
            )
        {
            self.pending_approval_context = None;
        }
        let mut out = Vec::new();
        match method {
            "amux.attachments"
            | "thread/started"
            | "thread/name/updated"
            | "thread/status/changed"
            | "thread/closed"
            | "thread/archived"
            | "thread/unarchived"
            | "thread/goal/updated"
            | "thread/goal/cleared"
            | "serverRequest/resolved" => {}
            "amux.codex_ready" => {
                let repeated = self.ready_seen;
                self.ready_seen = true;
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
                self.asks.clear();
                if row.get("resumed").and_then(Value::as_bool) == Some(true) {
                    out.push(simple_delivery(
                        seq,
                        revision,
                        CodexEntryKind::Boundary,
                        CodexBody::Boundary {
                            kind: "resumed".into(),
                        },
                        None,
                    ));
                } else if repeated {
                    out.push(simple_delivery(
                        seq,
                        revision,
                        CodexEntryKind::Boundary,
                        CodexBody::Boundary {
                            kind: "ready".into(),
                        },
                        None,
                    ));
                }
                if let Some(model) = session_model(row) {
                    self.model = Some(clipped(model, 512));
                    self.known_model = true;
                }
            }
            "amux.codex_settings" => {
                if let Some(model) = session_model(row) {
                    self.model = Some(clipped(model, 512));
                    self.known_model = true;
                }
            }
            "amux.codex_gap" => {
                self.attention = Attention::Unknown;
                self.known_attention = false;
                self.known_outstanding = false;
                out.push(simple_delivery(
                    seq,
                    revision,
                    CodexEntryKind::Boundary,
                    CodexBody::Boundary { kind: "gap".into() },
                    string(row, "reason"),
                ));
            }
            "amux.codex_reconnect_error" | "error" | "warning" => {
                out.push(error_entry(seq, revision, method, row));
            }
            "turn/started" => {
                self.attention = Attention::Working;
                self.known_attention = true;
            }
            "turn/completed" => {
                let turn = row.get("turn").unwrap_or(&Value::Null);
                let turn_id = id(turn, "id").or_else(|| id(row, "turnId"));
                if let Some(turn_id) = turn_id {
                    let key = native_or_delivery("turn", Some(&turn_id), seq, 0);
                    let status = string(turn, "status").unwrap_or_else(|| "completed".into());
                    out.push(upsert(
                        key,
                        seq,
                        0,
                        revision,
                        partial(
                            CodexEntryKind::Turn,
                            CodexBody::Turn {
                                turn_id,
                                status: status.clone(),
                                token_usage: self.token_usage.clone(),
                            },
                            None,
                            revision,
                            Some(status),
                        ),
                    ));
                    self.attention = Attention::NeedsYou { why: Why::Finished };
                    self.known_attention = true;
                    self.known_outstanding = true;
                    self.asks.clear();
                } else {
                    out.push(unrecognized(seq, revision, "turn/completed without id"));
                }
            }
            "item/started" => self.item(seq, revision, row, false, &mut out),
            "item/completed" => self.item(seq, revision, row, true, &mut out),
            "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/commandExecution/outputDelta"
            | "command/exec/outputDelta"
            | "item/fileChange/outputDelta"
            | "item/fileChange/patchUpdated"
            | "item/plan/delta" => self.delta(seq, revision, method, row, &mut out),
            "turn/plan/updated" | "turn/diff/updated" => {
                self.snapshot(seq, revision, method, row, &mut out)
            }
            "thread/tokenUsage/updated" => self.usage(row),
            "mcpServer/startupStatus/updated" => {
                let mut patch = partial(
                    CodexEntryKind::McpStartup,
                    CodexBody::None,
                    string(row, "name"),
                    revision,
                    None,
                );
                patch.details = Patch::set(JsonBytes(bounded_json(row)), revision);
                out.push(upsert(delivery(seq, 0), seq, 0, revision, patch));
            }
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/call"
            | "item/tool/requestUserInput" => {
                self.pending_approval_context = Some(RestoreRow {
                    seq,
                    payload: JsonBytes(
                        serde_json::to_vec(row).unwrap_or_else(|_| b"null".to_vec()),
                    ),
                });
                self.row_addressed_work(seq, revision, method, row, &mut out)
            }
            "amux.codex_approval_required" => {
                let item_id = id(row, "item_id")
                    .or_else(|| id(row, "itemId"))
                    .or_else(|| {
                        self.pending_approval_context
                            .as_ref()
                            .and_then(|context| {
                                serde_json::from_slice::<Value>(&context.payload.0).ok()
                            })
                            .and_then(|context| {
                                id(&context, "item_id")
                                    .or_else(|| id(&context, "itemId"))
                                    .or_else(|| id(&context, "callId"))
                            })
                    });
                let Some(item_id) = item_id else {
                    out.push(unrecognized(
                        seq,
                        revision,
                        "approval required without item id",
                    ));
                    return out;
                };
                let key = native_or_delivery("item", Some(&item_id), seq, 0);
                let request = compact_id(row.get("request_id").unwrap_or(&Value::Null));
                if !self.asks.iter().any(|ask| ask.request_id == request) {
                    if let Some(context) = self.pending_approval_context.take() {
                        self.asks.push(PendingAsk {
                            request_id: request,
                            rows: vec![
                                context,
                                RestoreRow {
                                    seq,
                                    payload: JsonBytes(
                                        serde_json::to_vec(row)
                                            .unwrap_or_else(|_| b"null".to_vec()),
                                    ),
                                },
                            ],
                        });
                    } else {
                        self.known_outstanding = false;
                    }
                }
                self.attention = Attention::NeedsYou {
                    why: Why::Permission,
                };
                self.known_attention = true;
                let patch = partial(
                    CodexEntryKind::Work,
                    CodexBody::Item {
                        item_id,
                        item_type: "approval".into(),
                    },
                    None,
                    revision,
                    Some("awaiting_approval".into()),
                );
                out.push(upsert(key, seq, 0, revision, patch));
            }
            "amux.codex_approval_resolved" => {
                let request = compact_id(row.get("request_id").unwrap_or(&Value::Null));
                let item_id = id(row, "item_id")
                    .or_else(|| id(row, "itemId"))
                    .or_else(|| {
                        self.asks
                            .iter()
                            .find(|ask| ask.request_id == request)
                            .and_then(|ask| {
                                ask.rows.iter().find_map(|context| {
                                    serde_json::from_slice::<Value>(&context.payload.0)
                                        .ok()
                                        .and_then(|context| {
                                            id(&context, "item_id")
                                                .or_else(|| id(&context, "itemId"))
                                                .or_else(|| id(&context, "callId"))
                                        })
                                })
                            })
                    });
                let Some(item_id) = item_id else {
                    out.push(unrecognized(
                        seq,
                        revision,
                        "approval resolution without item id",
                    ));
                    return out;
                };
                let key = native_or_delivery("item", Some(&item_id), seq, 0);
                self.asks.retain(|ask| ask.request_id != request);
                let resolution = string(row, "resolution")
                    .or_else(|| string(row, "reason"))
                    .unwrap_or_else(|| "resolved".into());
                let state = if matches!(resolution.as_str(), "answered" | "answered_elsewhere") {
                    "running".into()
                } else {
                    resolution
                };
                out.push(upsert(
                    key,
                    seq,
                    0,
                    revision,
                    CodexPartial {
                        state: Patch::set(state, revision),
                        ..CodexPartial::default()
                    },
                ));
                if self.asks.is_empty() {
                    self.attention = Attention::Working;
                    self.known_attention = true;
                }
            }
            "amux.input_result" => {
                if let Some(text) = row.pointer("/ok/text").and_then(Value::as_str) {
                    if let Some(input) = row.get("input_id").filter(|id| !id.is_null()) {
                        let input = compact_id(input);
                        let key = native_or_delivery("input", Some(&input), seq, 0);
                        out.push(upsert(
                            key,
                            seq,
                            0,
                            revision,
                            partial(
                                CodexEntryKind::Prompt,
                                CodexBody::Steer { input_id: input },
                                Some(clipped(text, TEXT_MAX)),
                                revision,
                                Some("complete".into()),
                            ),
                        ));
                    } else {
                        out.push(unrecognized(seq, revision, "steer echo without input id"));
                    }
                }
            }
            "amux.codex_message" => self.agent_message(seq, revision, row, &mut out),
            "thread/compacted" => out.push(simple_delivery(
                seq,
                revision,
                CodexEntryKind::Boundary,
                CodexBody::Boundary {
                    kind: "compacted".into(),
                },
                string(row, "turnId"),
            )),
            _ => out.push(simple_delivery(
                seq,
                revision,
                CodexEntryKind::Unrecognized,
                CodexBody::Unrecognized {
                    method: clipped(method, 512),
                },
                None,
            )),
        }
        while self.asks.len() > TIP_MAX_OPEN_ENTRIES || self.tip_bytes() > TIP_MAX_BYTES {
            if !self.asks.is_empty() {
                self.asks.remove(0);
                self.known_outstanding = false;
            } else if self.pending_approval_context.take().is_some() {
                self.known_outstanding = false;
            } else {
                break;
            }
        }
        out
    }

    fn item(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        complete: bool,
        out: &mut Vec<Mutation<CodexEntry>>,
    ) {
        let item = row.get("item").unwrap_or(&Value::Null);
        let item_id = id(item, "id");
        let Some(item_id) = item_id else {
            out.push(unrecognized(seq, revision, "item without id"));
            return;
        };
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            out.push(unrecognized(seq, revision, "item without type"));
            return;
        };
        let key = native_or_delivery("item", Some(&item_id), seq, 0);
        let kind = match item_type {
            "userMessage" => CodexEntryKind::Prompt,
            "agentMessage" => CodexEntryKind::Message,
            "reasoning" => CodexEntryKind::Reasoning,
            "contextCompaction" => CodexEntryKind::Boundary,
            _ => CodexEntryKind::Work,
        };
        let text = item_text(item, item_type);
        let mut patch = partial(
            kind,
            CodexBody::Item {
                item_id,
                item_type: clipped(item_type, 512),
            },
            None,
            revision,
            Some(if complete { "complete" } else { "open" }.into()),
        );
        patch.details = Patch::set(JsonBytes(bounded_json(item)), revision);
        if complete && !text.is_empty() {
            patch.final_text = Some(FinalText {
                through: seq,
                revision,
                values: vec![Component {
                    source: ComponentSource::Sequence { seq, slot: 0 },
                    observed_at: seq,
                    after: Vec::new(),
                    value: clipped(&text, TEXT_MAX),
                }],
            });
        } else if !text.is_empty() {
            patch.text = Patch::set(clipped(&text, TEXT_MAX), revision);
        }
        out.push(upsert(key, seq, 0, revision, patch));
    }

    fn delta(
        &mut self,
        seq: u64,
        revision: Revision,
        method: &str,
        row: &Value,
        out: &mut Vec<Mutation<CodexEntry>>,
    ) {
        let item_id = id(row, "itemId").or_else(|| id(row, "item_id"));
        let Some(item_id) = item_id else {
            out.push(unrecognized(seq, revision, "item delta without id"));
            return;
        };
        let key = native_or_delivery("item", Some(&item_id), seq, 0);
        let kind = if method.contains("agentMessage") {
            CodexEntryKind::Message
        } else if method.contains("reasoning") {
            CodexEntryKind::Reasoning
        } else {
            CodexEntryKind::Work
        };
        let text = ["delta", "text", "output", "patch"]
            .into_iter()
            .find_map(|field| row.get(field).and_then(Value::as_str))
            .unwrap_or_default();
        let mut patch = partial(
            kind,
            CodexBody::Item {
                item_id,
                item_type: method.into(),
            },
            None,
            revision,
            None,
        );
        if !text.is_empty() {
            patch.components.push(Component {
                source: ComponentSource::Sequence { seq, slot: 0 },
                observed_at: seq,
                after: Vec::new(),
                value: clipped(text, TEXT_MAX),
            });
        }
        if matches!(
            method,
            "item/agentMessage/delta"
                | "item/reasoning/textDelta"
                | "item/reasoning/summaryTextDelta"
                | "item/reasoning/summaryPartAdded"
                | "item/fileChange/patchUpdated"
                | "item/plan/delta"
        ) {
            patch.details = Patch::set(JsonBytes(bounded_json(row)), revision);
        }
        out.push(upsert(key, seq, 0, revision, patch));
    }

    fn snapshot(
        &mut self,
        seq: u64,
        revision: Revision,
        method: &str,
        row: &Value,
        out: &mut Vec<Mutation<CodexEntry>>,
    ) {
        let turn = id(row, "turnId");
        let ns = if method.contains("plan") {
            "plan"
        } else {
            "diff"
        };
        let key = native_or_delivery(ns, turn.as_deref(), seq, 0);
        let mut patch = partial(
            CodexEntryKind::Work,
            CodexBody::Snapshot {
                turn_id: turn,
                kind: ns.into(),
            },
            None,
            revision,
            Some("snapshot".into()),
        );
        patch.details = Patch::set(JsonBytes(bounded_json(row)), revision);
        out.push(upsert(key, seq, 0, revision, patch));
    }

    fn row_addressed_work(
        &mut self,
        seq: u64,
        revision: Revision,
        method: &str,
        row: &Value,
        out: &mut Vec<Mutation<CodexEntry>>,
    ) {
        let item_id = id(row, "itemId")
            .or_else(|| id(row, "item_id"))
            .or_else(|| id(row, "callId"));
        if item_id.is_none() && method != "item/tool/requestUserInput" {
            out.push(unrecognized(seq, revision, "work row without item id"));
            return;
        }
        let key = native_or_delivery("item", item_id.as_deref(), seq, 0);
        let mut patch = partial(
            CodexEntryKind::Work,
            CodexBody::Item {
                item_id: item_id.unwrap_or_default(),
                item_type: method.into(),
            },
            None,
            revision,
            Some(
                if method.ends_with("requestUserInput") {
                    "blocked_unsupported"
                } else {
                    "proposed"
                }
                .into(),
            ),
        );
        patch.details = Patch::set(JsonBytes(bounded_json(row)), revision);
        out.push(upsert(key, seq, 0, revision, patch));
    }

    fn usage(&mut self, row: &Value) {
        let usage = row
            .pointer("/tokenUsage/last")
            .or_else(|| row.pointer("/tokenUsage/total"))
            .unwrap_or(&Value::Null);
        let token_usage = TokenUsage {
            input_tokens: usage.get("inputTokens").and_then(Value::as_u64),
            cached_input_tokens: usage.get("cachedInputTokens").and_then(Value::as_u64),
            cache_write_input_tokens: usage.get("cacheWriteInputTokens").and_then(Value::as_u64),
            output_tokens: usage.get("outputTokens").and_then(Value::as_u64),
            reasoning_output_tokens: usage.get("reasoningOutputTokens").and_then(Value::as_u64),
            total_tokens: usage.get("totalTokens").and_then(Value::as_u64),
            model_context_window: row
                .pointer("/tokenUsage/modelContextWindow")
                .and_then(Value::as_u64),
        };
        if token_usage.total_tokens.is_some() || token_usage.input_tokens.is_some() {
            self.token_usage = Some(token_usage);
            self.known_context = true;
        }
    }

    fn agent_message(
        &mut self,
        seq: u64,
        revision: Revision,
        row: &Value,
        out: &mut Vec<Mutation<CodexEntry>>,
    ) {
        let envelope = row.get("envelope").unwrap_or(row);
        let envelope_id = id(envelope, "id");
        let key = native_or_delivery("env", envelope_id.as_deref(), seq, 0);
        let text = string(envelope, "text").unwrap_or_default();
        let from = envelope
            .get("from")
            .and_then(Value::as_str)
            .map(|v| clipped(v, 512))
            .or_else(|| {
                envelope
                    .pointer("/from/name")
                    .and_then(Value::as_str)
                    .map(|v| clipped(v, 512))
            })
            .unwrap_or_else(|| "unknown".into());
        out.push(upsert(
            key,
            seq,
            0,
            revision,
            partial(
                CodexEntryKind::AgentMessage,
                CodexBody::AgentMessage {
                    id: envelope_id,
                    context: id(envelope, "context"),
                    from,
                    kind: AgentMessageKind::read(envelope.get("kind").and_then(Value::as_str)),
                    delivery: string(row, "delivery"),
                },
                Some(text),
                revision,
                Some("complete".into()),
            ),
        ));
    }
}

impl ProviderFold for CodexFold {
    type Entry = CodexEntry;
    const PROTOCOL: StructuredProtocol = StructuredProtocol::Codex;
    const ENTRY_VERSION: u32 = 3;
    const TIP_VERSION: u32 = 3;
    const TIP_BUDGET: usize = TIP_MAX_BYTES;
    fn begin(&mut self, segment: SegmentId, baseline: Baseline) {
        self.segment = segment;
        self.baseline = baseline;
        self.asks.clear();
        self.pending_approval_context = None;
        self.ready_seen = false;
        if baseline == Baseline::Start {
            *self = Self {
                segment,
                baseline,
                ..Self::default()
            };
        } else {
            self.attention = Attention::Unknown;
            self.known_attention = false;
            self.known_outstanding = false;
        }
    }
    fn apply(&mut self, input: Input<'_>) -> Changes<Self::Entry> {
        let mutations = match input {
            Input::Row {
                seq,
                activity_at,
                payload,
                ..
            } => match serde_json::from_slice(payload) {
                Ok(row) => self.row(seq, activity_at, &row),
                Err(_) => {
                    self.through = self.through.max(seq);
                    vec![unrecognized(seq, Revision::row(seq), "invalid json")]
                }
            },
            Input::ReplayComplete { through, .. } => {
                self.through = self.through.max(through);
                Vec::new()
            }
            Input::ProcessExited { exit_code, .. } => {
                self.phase = AgentPhase::Exited { exit_code };
                self.known_phase = true;
                self.attention = Attention::Idle;
                self.known_attention = true;
                self.known_outstanding = true;
                self.asks.clear();
                Vec::new()
            }
            Input::ObserverLost { .. } => {
                self.attention = Attention::Unknown;
                self.known_attention = false;
                self.known_outstanding = false;
                self.asks.clear();
                Vec::new()
            }
            Input::Tick { .. } => Vec::new(),
        };
        Changes {
            summary: Some(self.summary()),
            through: self.through,
            mutations,
        }
    }
    fn summary(&self) -> Summary {
        let mut unknown = vec![SummaryField::Todo];
        if !self.known_attention {
            unknown.push(SummaryField::Attention);
        }
        if !self.known_phase {
            unknown.push(SummaryField::Phase);
        }
        if !self.known_last_activity {
            unknown.push(SummaryField::LastActivity);
        }
        if !self.known_context {
            unknown.push(SummaryField::Context);
        }
        if !self.known_model {
            unknown.push(SummaryField::Model);
        }
        if !self.known_outstanding {
            unknown.push(SummaryField::Outstanding);
        }
        Summary {
            attention: if self.known_attention {
                self.attention
            } else {
                Attention::Unknown
            },
            phase: self.phase.clone(),
            last_activity: self.last_activity,
            todo: None,
            context: self.token_usage.as_ref().and_then(|usage| {
                usage
                    .total_tokens
                    .or(usage.input_tokens)
                    .map(|used_tokens| ContextMeter {
                        used_tokens,
                        window_tokens: usage.model_context_window,
                        source: ContextMeterSource::ResultUsage,
                    })
            }),
            model: self.model.clone(),
            unknown,
        }
    }
    fn tip_bytes(&self) -> usize {
        size_of::<Self>()
            + self.asks.capacity() * size_of::<PendingAsk>()
            + self
                .asks
                .iter()
                .map(|ask| {
                    ask.request_id.capacity()
                        + ask.rows.capacity() * size_of::<RestoreRow>()
                        + ask
                            .rows
                            .iter()
                            .map(|row| row.payload.0.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self
                .pending_approval_context
                .as_ref()
                .map_or(0, |row| row.payload.0.capacity())
            + self.model.as_ref().map_or(0, String::capacity)
    }
}

fn partial(
    kind: CodexEntryKind,
    body: CodexBody,
    text: Option<String>,
    revision: Revision,
    state: Option<String>,
) -> CodexPartial {
    CodexPartial {
        kind: Patch::set(kind, revision),
        body: Patch::set(body, revision),
        text: text.map_or(Patch::Unchanged, |v| Patch::set(v, revision)),
        state: state.map_or(Patch::Unchanged, |v| Patch::set(v, revision)),
        ..CodexPartial::default()
    }
}
fn upsert(
    key: EntryKey,
    seq: u64,
    slot: u16,
    revision: Revision,
    entry: CodexPartial,
) -> Mutation<CodexEntry> {
    Mutation::Upsert {
        key,
        order: Order::new(seq, slot.min(1023)).expect("bounded Codex slot"),
        revision,
        entry,
    }
}
fn delivery(seq: u64, slot: u16) -> EntryKey {
    EntryKey::new(format!("d:{seq}:{slot}")).unwrap()
}
fn native_or_delivery(ns: &str, id: Option<&str>, seq: u64, slot: u16) -> EntryKey {
    id.and_then(|id| EntryKey::new(format!("{ns}:{id}")).ok())
        .unwrap_or_else(|| delivery(seq, slot))
}
fn simple_delivery(
    seq: u64,
    revision: Revision,
    kind: CodexEntryKind,
    body: CodexBody,
    text: Option<String>,
) -> Mutation<CodexEntry> {
    upsert(
        delivery(seq, 0),
        seq,
        0,
        revision,
        partial(kind, body, text, revision, None),
    )
}
fn unrecognized(seq: u64, revision: Revision, detail: &str) -> Mutation<CodexEntry> {
    simple_delivery(
        seq,
        revision,
        CodexEntryKind::Unrecognized,
        CodexBody::Unrecognized {
            method: clipped(detail, 512),
        },
        None,
    )
}
fn error_entry(seq: u64, revision: Revision, method: &str, row: &Value) -> Mutation<CodexEntry> {
    let text = row
        .pointer("/error/message")
        .or_else(|| row.get("message"))
        .and_then(Value::as_str)
        .map(|v| clipped(v, TEXT_MAX));
    let severity = if method == "warning" {
        "warning"
    } else {
        "error"
    };
    simple_delivery(
        seq,
        revision,
        CodexEntryKind::Error,
        CodexBody::Error {
            severity: severity.into(),
            will_retry: row
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        text,
    )
}
fn id(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
fn string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|v| clipped(v, TEXT_MAX))
}
fn session_model(row: &Value) -> Option<&str> {
    row.pointer("/session/model")
        .or_else(|| row.get("model"))
        .and_then(Value::as_str)
}
fn compact_id(value: &Value) -> String {
    clipped(
        &serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
        512,
    )
}
fn bounded_json(value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    if bytes.len() <= VALUE_MAX {
        bytes
    } else {
        format!("{{\"clipped\":true,\"bytes\":{}}}", bytes.len()).into_bytes()
    }
}
fn clipped(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.into();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… clipped …", &value[..end])
}
fn item_text(item: &Value, item_type: &str) -> String {
    match item_type {
        "agentMessage" => string(item, "text").unwrap_or_default(),
        "reasoning" => item
            .get("text")
            .and_then(Value::as_str)
            .map(|v| clipped(v, TEXT_MAX))
            .unwrap_or_default(),
        "userMessage" => item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
fn truncate_string(field: &mut VersionedField<String>, max: usize) {
    if let Some(value) = field.value_mut() {
        *value = clipped(value, max);
    }
}
fn truncate_bytes(field: &mut VersionedField<JsonBytes>, max: usize) {
    if let Some(value) = field.value_mut()
        && value.0.len() > max
    {
        value.0 = format!("{{\"clipped\":true,\"bytes\":{}}}", value.0.len()).into_bytes();
    }
}

impl crate::private::Sealed for CodexEntryKind {
    fn assert_fields_are_postcard_safe() {
        let _ = |v: CodexEntryKind| match v {
            CodexEntryKind::Prompt
            | CodexEntryKind::Message
            | CodexEntryKind::Reasoning
            | CodexEntryKind::Work
            | CodexEntryKind::McpStartup
            | CodexEntryKind::AgentMessage
            | CodexEntryKind::Turn
            | CodexEntryKind::Boundary
            | CodexEntryKind::Error
            | CodexEntryKind::Unrecognized => {}
        };
    }
}
impl PostcardSafe for CodexEntryKind {}
impl crate::private::Sealed for TokenUsage {
    fn assert_fields_are_postcard_safe() {
        let _ = |TokenUsage {
                     input_tokens,
                     cached_input_tokens,
                     cache_write_input_tokens,
                     output_tokens,
                     reasoning_output_tokens,
                     total_tokens,
                     model_context_window,
                 }: TokenUsage| {
            crate::assert_value_safe(&input_tokens);
            crate::assert_value_safe(&cached_input_tokens);
            crate::assert_value_safe(&cache_write_input_tokens);
            crate::assert_value_safe(&output_tokens);
            crate::assert_value_safe(&reasoning_output_tokens);
            crate::assert_value_safe(&total_tokens);
            crate::assert_value_safe(&model_context_window);
        };
    }
}
impl PostcardSafe for TokenUsage {}
impl crate::private::Sealed for CodexBody {
    fn assert_fields_are_postcard_safe() {
        let _ = |v: CodexBody| match v {
            CodexBody::None => {}
            CodexBody::Item { item_id, item_type } => {
                crate::assert_value_safe(&item_id);
                crate::assert_value_safe(&item_type);
            }
            CodexBody::Steer { input_id } => crate::assert_value_safe(&input_id),
            CodexBody::Turn {
                turn_id,
                status,
                token_usage,
            } => {
                crate::assert_value_safe(&turn_id);
                crate::assert_value_safe(&status);
                crate::assert_value_safe(&token_usage);
            }
            CodexBody::Snapshot { turn_id, kind } => {
                crate::assert_value_safe(&turn_id);
                crate::assert_value_safe(&kind);
            }
            CodexBody::AgentMessage {
                id,
                context,
                from,
                kind,
                delivery,
            } => {
                crate::assert_value_safe(&id);
                crate::assert_value_safe(&context);
                crate::assert_value_safe(&from);
                crate::assert_value_safe(&kind);
                crate::assert_value_safe(&delivery);
            }
            CodexBody::Boundary { kind } => crate::assert_value_safe(&kind),
            CodexBody::Error {
                severity,
                will_retry,
            } => {
                crate::assert_value_safe(&severity);
                crate::assert_value_safe(&will_retry);
            }
            CodexBody::Unrecognized { method } => crate::assert_value_safe(&method),
        };
    }
}
impl PostcardSafe for CodexBody {}
impl crate::private::Sealed for FinalText {
    fn assert_fields_are_postcard_safe() {
        let _ = |FinalText {
                     through,
                     revision,
                     values,
                 }: FinalText| {
            crate::assert_value_safe(&through);
            crate::assert_value_safe(&revision);
            crate::assert_value_safe(&values);
        };
    }
}
impl PostcardSafe for FinalText {}
impl crate::private::Sealed for CodexPartial {
    fn assert_fields_are_postcard_safe() {
        let _ = |CodexPartial {
                     kind,
                     body,
                     text,
                     components,
                     final_text,
                     finality,
                     state,
                     details,
                     clipped,
                 }: CodexPartial| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&final_text);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&state);
            crate::assert_value_safe(&details);
            crate::assert_value_safe(&clipped);
        };
    }
}
impl PostcardSafe for CodexPartial {}
impl crate::private::Sealed for CodexEntry {
    fn assert_fields_are_postcard_safe() {
        let _ = |CodexEntry {
                     kind,
                     body,
                     text,
                     components,
                     finality,
                     state,
                     details,
                     clipped,
                     rendered_text,
                 }: CodexEntry| {
            crate::assert_value_safe(&kind);
            crate::assert_value_safe(&body);
            crate::assert_value_safe(&text);
            crate::assert_value_safe(&components);
            crate::assert_value_safe(&finality);
            crate::assert_value_safe(&state);
            crate::assert_value_safe(&details);
            crate::assert_value_safe(&clipped);
            crate::assert_value_safe(&rendered_text);
        };
    }
}
impl PostcardSafe for CodexEntry {}
impl crate::private::Sealed for PendingAsk {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<Vec<RestoreRow>>();
    }
}
impl PostcardSafe for PendingAsk {}
impl crate::private::Sealed for CodexFold {
    fn assert_fields_are_postcard_safe() {
        let _ = |CodexFold {
                     segment,
                     baseline,
                     through,
                     asks,
                     pending_approval_context,
                     attention,
                     phase,
                     last_activity,
                     token_usage,
                     model,
                     ready_seen,
                     known_attention,
                     known_phase,
                     known_last_activity,
                     known_context,
                     known_model,
                     known_outstanding,
                 }: CodexFold| {
            crate::assert_value_safe(&segment);
            crate::assert_value_safe(&baseline);
            crate::assert_value_safe(&through);
            crate::assert_value_safe(&asks);
            crate::assert_value_safe(&pending_approval_context);
            crate::assert_value_safe(&attention);
            crate::assert_value_safe(&phase);
            crate::assert_value_safe(&last_activity);
            crate::assert_value_safe(&token_usage);
            crate::assert_value_safe(&model);
            crate::assert_value_safe(&ready_seen);
            crate::assert_value_safe(&known_attention);
            crate::assert_value_safe(&known_phase);
            crate::assert_value_safe(&known_last_activity);
            crate::assert_value_safe(&known_context);
            crate::assert_value_safe(&known_model);
            crate::assert_value_safe(&known_outstanding);
        };
    }
}
impl PostcardSafe for CodexFold {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::claude_pty::{ClaudeEntryKind, ClaudeFold};
    use crate::claude_sdk::{ClaudeSdkEntryKind, ClaudeSdkFold};
    use crate::{Entry, MutationOracle};

    const CORPORA: &[(&str, &str)] = &[
        (
            "round_trip",
            include_str!("../../../codex-specs/fixtures/codex/turn_round_trip.rows.jsonl"),
        ),
        (
            "approval",
            include_str!("../../../codex-specs/fixtures/codex/approval_allow.rows.jsonl"),
        ),
        (
            "dynamic",
            include_str!("../../../codex-specs/fixtures/codex/dynamic_tools.rows.jsonl"),
        ),
        (
            "messages",
            include_str!("../../../codex-specs/fixtures/codex/two_assistant_messages.rows.jsonl"),
        ),
    ];
    fn at(seq: u64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_760_000_000 + seq as i64, 0)
            .single()
            .unwrap()
    }
    fn rows(raw: &str) -> Vec<Vec<u8>> {
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.as_bytes().to_vec())
            .collect()
    }
    fn apply_row(
        fold: &mut CodexFold,
        oracle: &mut MutationOracle<CodexEntry>,
        seq: u64,
        payload: &[u8],
    ) {
        let changes = fold.apply(Input::Row {
            seq,
            published_at: at(seq),
            activity_at: Some(at(seq)),
            historical: false,
            payload,
        });
        oracle.apply_changes(&changes).unwrap();
        assert!(fold.tip_bytes() <= CodexFold::TIP_BUDGET);
    }
    fn fold_rows(input: &[Vec<u8>]) -> (CodexFold, MutationOracle<CodexEntry>) {
        let mut fold = CodexFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, row) in input.iter().enumerate() {
            apply_row(&mut fold, &mut oracle, index as u64 + 1, row);
        }
        (fold, oracle)
    }
    fn keys(oracle: &MutationOracle<CodexEntry>) -> Vec<String> {
        oracle
            .entries()
            .into_iter()
            .map(|entry| entry.key.into_string())
            .collect()
    }

    fn body_variant_name(body: &CodexBody) -> &'static str {
        match body {
            CodexBody::None => "none",
            CodexBody::Item { .. } => "item",
            CodexBody::Steer { .. } => "steer",
            CodexBody::Turn { .. } => "turn",
            CodexBody::Snapshot { .. } => "snapshot",
            CodexBody::AgentMessage { .. } => "agent_message",
            CodexBody::Boundary { .. } => "boundary",
            CodexBody::Error { .. } => "error",
            CodexBody::Unrecognized { .. } => "unrecognized",
        }
    }

    fn entry_encoding(body: CodexBody) -> String {
        let entry = CodexEntry::from_partial(&CodexPartial {
            body: Patch::set(body, Revision::row(7)),
            ..CodexPartial::default()
        })
        .unwrap();
        postcard::to_allocvec(&entry)
            .unwrap()
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn codex_entry_version_pins_every_body_variant_encoding() {
        const ENCODING_ENTRY_VERSION: u32 = 3;
        assert_eq!(CodexFold::ENTRY_VERSION, ENCODING_ENTRY_VERSION);

        let bodies = [
            CodexBody::None,
            CodexBody::Item {
                item_id: "i".into(),
                item_type: "commandExecution".into(),
            },
            CodexBody::Steer {
                input_id: "s".into(),
            },
            CodexBody::Turn {
                turn_id: "t".into(),
                status: "done".into(),
                token_usage: None,
            },
            CodexBody::Snapshot {
                turn_id: Some("t".into()),
                kind: "plan".into(),
            },
            CodexBody::AgentMessage {
                id: Some("e".into()),
                context: Some("c".into()),
                from: "worker".into(),
                kind: AgentMessageKind::Message,
                delivery: Some("socket".into()),
            },
            CodexBody::Boundary { kind: "gap".into() },
            CodexBody::Error {
                severity: "error".into(),
                will_retry: true,
            },
            CodexBody::Unrecognized {
                method: "future".into(),
            },
        ];
        let actual = bodies
            .into_iter()
            .map(|body| format!("{}={}", body_variant_name(&body), entry_encoding(body)))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            actual,
            r#"none=0000010700000100000000000000000000000000000000
item=0000010700000101016910636f6d6d616e64457865637574696f6e000000000000000000000000000000
steer=00000107000001020173000000000000000000000000000000
turn=0000010700000103017404646f6e6500000000000000000000000000000000
snapshot=000001070000010401017404706c616e000000000000000000000000000000
agent_message=000001070000010501016501016306776f726b6572000106736f636b6574000000000000000000000000000000
boundary=000001070000010603676170000000000000000000000000000000
error=0000010700000107056572726f7201000000000000000000000000000000
unrecognized=000001070000010806667574757265000000000000000000000000000000"#
        );
    }

    fn fold_pty_delivery_variants(input: &[Vec<u8>], observed: &mut BTreeSet<&'static str>) {
        let mut fold = ClaudeFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().enumerate() {
            let seq = index as u64 + 1;
            let changes = fold.apply(Input::Row {
                seq,
                published_at: at(seq),
                activity_at: Some(at(seq)),
                historical: false,
                payload,
            });
            oracle.apply_changes(&changes).unwrap();
        }
        for stored in oracle
            .entries()
            .into_iter()
            .filter(|stored| stored.key.as_str().starts_with("d:"))
        {
            let variant = match stored.entry.entry_kind() {
                Some(ClaudeEntryKind::Message) => "claude_pty.message_without_message_id",
                Some(ClaudeEntryKind::Unrecognized) => "claude_pty.unrecognized_without_uuid",
                other => panic!(
                    "unnamed Claude PTY delivery-key fallback {:?} at {}",
                    other, stored.key
                ),
            };
            observed.insert(variant);
        }
    }

    fn fold_sdk_delivery_variants(input: &[Vec<u8>], observed: &mut BTreeSet<&'static str>) {
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mut oracle = MutationOracle::default();
        for (index, payload) in input.iter().enumerate() {
            let seq = index as u64 + 1;
            let changes = fold.apply(Input::Row {
                seq,
                published_at: at(seq),
                activity_at: Some(at(seq)),
                historical: false,
                payload,
            });
            oracle.apply_changes(&changes).unwrap();
        }
        for stored in oracle
            .entries()
            .into_iter()
            .filter(|stored| stored.key.as_str().starts_with("d:"))
        {
            let variant = match stored.entry.entry_kind() {
                Some(ClaudeSdkEntryKind::Prompt) => "claude_sdk.prompt_without_uuid",
                Some(ClaudeSdkEntryKind::Message | ClaudeSdkEntryKind::Thinking)
                    if stored.entry.is_incomplete() =>
                {
                    "claude_sdk.stream_block_without_message_start"
                }
                Some(
                    ClaudeSdkEntryKind::Compaction
                    | ClaudeSdkEntryKind::AgentMessage
                    | ClaudeSdkEntryKind::Status
                    | ClaudeSdkEntryKind::Boundary
                    | ClaudeSdkEntryKind::Unrecognized,
                ) => "claude_sdk.occurrence_without_uuid",
                other => panic!(
                    "unnamed Claude SDK delivery-key fallback {:?} at {}",
                    other, stored.key
                ),
            };
            observed.insert(variant);
        }
    }

    fn fold_codex_delivery_variants(input: &[Vec<u8>], observed: &mut BTreeSet<&'static str>) {
        let (_, oracle) = fold_rows(input);
        for stored in oracle
            .entries()
            .into_iter()
            .filter(|stored| stored.key.as_str().starts_with("d:"))
        {
            let variant = match (stored.entry.entry_kind(), stored.entry.body()) {
                (Some(CodexEntryKind::Work), Some(CodexBody::Snapshot { turn_id: None, .. })) => {
                    "codex.turn_snapshot_without_turn_id"
                }
                (Some(CodexEntryKind::Work), Some(CodexBody::Item { item_id, item_type }))
                    if item_id.is_empty() && item_type.ends_with("requestUserInput") =>
                {
                    "codex.user_input_without_item_id"
                }
                (Some(CodexEntryKind::McpStartup), _) => "codex.mcp_startup",
                (
                    Some(CodexEntryKind::AgentMessage),
                    Some(CodexBody::AgentMessage { id: None, .. }),
                ) => "codex.agent_message_without_envelope_id",
                (Some(CodexEntryKind::Boundary), Some(CodexBody::Boundary { kind }))
                    if kind == "resumed" =>
                {
                    "codex.resumed_boundary"
                }
                (Some(CodexEntryKind::Boundary), Some(CodexBody::Boundary { kind }))
                    if kind == "ready" =>
                {
                    "codex.ready_boundary"
                }
                (Some(CodexEntryKind::Boundary), Some(CodexBody::Boundary { kind }))
                    if kind == "gap" =>
                {
                    "codex.gap_boundary"
                }
                (Some(CodexEntryKind::Error), _) => "codex.error",
                (Some(CodexEntryKind::Unrecognized), _) => "codex.unrecognized",
                other => panic!(
                    "unnamed Codex delivery-key fallback {:?} at {}",
                    other, stored.key
                ),
            };
            observed.insert(variant);
        }
    }

    #[test]
    fn codex_continuation_matches_uninterrupted_at_every_corpus_cut() {
        for (name, corpus) in CORPORA {
            let input = rows(corpus);
            let (expected_fold, expected_oracle) = fold_rows(&input);
            let expected_entries = expected_oracle.entries();
            let mut prefix_fold = CodexFold::default();
            prefix_fold.begin(1, Baseline::Start);
            let mut prefix_oracle = MutationOracle::default();
            for cut in 0..=input.len() {
                if cut > 0 {
                    apply_row(
                        &mut prefix_fold,
                        &mut prefix_oracle,
                        cut as u64,
                        &input[cut - 1],
                    );
                }
                let bytes = postcard::to_allocvec(&prefix_fold).unwrap();
                let mut resumed: CodexFold = postcard::from_bytes(&bytes).unwrap();
                let mut oracle = prefix_oracle.clone();
                for (index, row) in input.iter().enumerate().skip(cut) {
                    apply_row(&mut resumed, &mut oracle, index as u64 + 1, row);
                }
                assert_eq!(resumed, expected_fold, "{name} tip at {cut}");
                assert_eq!(
                    resumed.summary(),
                    expected_fold.summary(),
                    "{name} summary at {cut}"
                );
                assert_eq!(
                    oracle.entries(),
                    expected_entries,
                    "{name} entries at {cut}"
                );
            }
        }
    }

    #[test]
    fn codex_completed_item_replay_keeps_one_native_entry() {
        let completed = serde_json::to_vec(&json!({"type":"item/completed","item":{"id":"m1","type":"agentMessage","phase":"final_answer","text":"done"}})).unwrap();
        let (_, oracle) = fold_rows(&[completed.clone(), completed]);
        assert_eq!(keys(&oracle), ["item:m1"]);
        assert_eq!(oracle.entries()[0].entry.text(), Some("done"));
    }

    #[test]
    fn codex_item_creating_and_amending_forms_share_native_identity() {
        let input = rows(
            r#"
{"type":"item/started","item":{"id":"message-1","type":"agentMessage","text":""}}
{"type":"item/agentMessage/delta","itemId":"message-1","delta":"hel"}
{"type":"item/completed","item":{"id":"message-1","type":"agentMessage","text":"hello"}}
{"type":"item/commandExecution/requestApproval","itemId":"command-1","command":"cargo test --workspace","cwd":"/work"}
{"type":"amux.codex_approval_required","item_id":"command-1","request_id":1}
{"type":"amux.codex_approval_resolved","item_id":"command-1","request_id":1,"reason":"answered"}
"#,
        );
        let (_, oracle) = fold_rows(&input);
        assert_eq!(keys(&oracle), ["item:message-1", "item:command-1"]);
        assert_eq!(oracle.entries()[0].entry.text(), Some("hello"));
        let command = &oracle.entries()[1].entry;
        assert_eq!(command.state(), Some("running"));
        let details: Value = serde_json::from_slice(&command.details().unwrap().0).unwrap();
        assert_eq!(details["type"], "item/commandExecution/requestApproval");
        assert_eq!(details["command"], "cargo test --workspace");
    }

    #[test]
    fn codex_enriched_approval_and_steer_are_identical_for_a_second_observer() {
        let input = vec![
            serde_json::to_vec(&json!({"type":"amux.codex_approval_required","item_id":"cmd-1","request_id":7,"availableDecisions":["accept"]})).unwrap(),
            serde_json::to_vec(&json!({"type":"amux.codex_approval_resolved","item_id":"cmd-1","request_id":7,"resolution":"answered"})).unwrap(),
            serde_json::to_vec(&json!({"type":"amux.input_result","input_id":[1,2,3],"ok":{"input_id":[1,2,3],"text":"steer"}})).unwrap(),
        ];
        let (left_fold, left) = fold_rows(&input);
        let (right_fold, right) = fold_rows(&input);
        assert_eq!(left_fold, right_fold);
        assert_eq!(left.entries(), right.entries());
        assert_eq!(keys(&left), ["item:cmd-1", "input:[1,2,3]"]);
    }

    #[test]
    fn codex_identity_and_context_follow_a5_and_the_knowledge_table() {
        let input = rows(
            r#"
{"type":"item/completed","item":{"id":"u","type":"userMessage","content":[{"type":"text","text":"hello"}]}}
{"type":"item/completed","item":{"id":"m","type":"agentMessage","text":"answer"}}
{"type":"turn/plan/updated","turnId":"t","plan":[]}
{"type":"turn/diff/updated","diff":"x"}
{"type":"item/tool/requestUserInput","itemId":"q","questions":[]}
{"type":"mcpServer/startupStatus/updated","name":"x","status":"ready"}
{"type":"turn/completed","turn":{"id":"turn","status":"completed"}}
{"type":"amux.codex_message","envelope":{"id":"env","from":"human","text":"hi"}}
{"type":"thread/tokenUsage/updated","tokenUsage":{"last":{"inputTokens":42},"modelContextWindow":100}}
"#,
        );
        let (fold, oracle) = fold_rows(&input);
        assert_eq!(
            keys(&oracle),
            [
                "item:u",
                "item:m",
                "plan:t",
                "d:4:0",
                "item:q",
                "d:6:0",
                "turn:turn",
                "env:env"
            ]
        );
        assert_eq!(fold.summary().context.as_ref().unwrap().used_tokens, 42);
        assert_eq!(
            fold.summary().context.as_ref().unwrap().window_tokens,
            Some(100)
        );
        assert!(fold.summary().unknown.contains(&SummaryField::Todo));
    }

    #[test]
    fn codex_ready_and_settings_observe_session_model_and_repeated_ready() {
        let input = rows(
            r#"
{"type":"amux.codex_ready","session":{"model":"model-a"}}
{"type":"amux.codex_settings","session":{"model":"model-b"}}
{"type":"amux.codex_ready"}
"#,
        );
        let (fold, oracle) = fold_rows(&input);
        assert_eq!(fold.summary().model.as_deref(), Some("model-b"));
        assert_eq!(keys(&oracle), ["d:3:0"]);
        assert!(matches!(
            oracle.entries()[0].entry.body(),
            Some(CodexBody::Boundary { kind }) if kind == "ready"
        ));
    }

    #[test]
    fn codex_lifecycle_and_tip_are_bounded_and_postcard_safe() {
        let huge = "x".repeat(200_000);
        let input = serde_json::to_vec(
            &json!({"type":"item/started","item":{"id":"m","type":"agentMessage","text":huge}}),
        )
        .unwrap();
        let (mut fold, oracle) = fold_rows(&[input]);
        assert!(fold.tip_bytes() < 64 * 1024);
        let bytes = postcard::to_allocvec(&fold).unwrap();
        let decoded: CodexFold = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, fold);
        let bytes = postcard::to_allocvec(&oracle.entries()[0].entry).unwrap();
        let _: CodexEntry = postcard::from_bytes(&bytes).unwrap();
        fold.apply(Input::ObserverLost { at: at(3) });
        assert!(fold.summary().unknown.contains(&SummaryField::Outstanding));
        fold.apply(Input::ProcessExited {
            exit_code: Some(2),
            at: at(4),
        });
        assert_eq!(
            fold.summary().phase,
            AgentPhase::Exited { exit_code: Some(2) }
        );
    }

    #[test]
    fn codex_tip_closes_gates_when_unresolved_asks_overflow() {
        let input = (0..=TIP_MAX_OPEN_ENTRIES)
            .flat_map(|index| {
                [
                    serde_json::to_vec(&json!({
                        "type":"item/commandExecution/requestApproval",
                        "itemId":format!("item-{index}"),
                        "command":"true"
                    }))
                    .unwrap(),
                    serde_json::to_vec(&json!({
                        "type":"amux.codex_approval_required",
                        "item_id":format!("item-{index}"),
                        "request_id":format!("request-{index}")
                    }))
                    .unwrap(),
                ]
            })
            .collect::<Vec<_>>();
        let (fold, _) = fold_rows(&input);
        assert!(fold.tip_bytes() <= TIP_MAX_BYTES);
        assert_eq!(fold.asks.len(), TIP_MAX_OPEN_ENTRIES);
        assert!(fold.summary().unknown.contains(&SummaryField::Outstanding));
    }

    #[test]
    fn delivery_keyed_variant_list_is_complete() {
        let declared = [
            crate::claude_pty::DELIVERY_KEYED_VARIANTS,
            crate::claude_sdk::DELIVERY_KEYED_VARIANTS,
            DELIVERY_KEYED_VARIANTS,
        ]
        .concat();
        let mut observed = BTreeSet::new();

        let pty_cases = [
            vec![
                json!({"type":"assistant","uuid":"a","message":{"content":[{"type":"text","text":"hello"}]}}),
            ],
            vec![json!({"type":"future"})],
            vec![
                json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}),
            ],
            vec![json!({"type":"system","subtype":"turn_duration","durationMs":1})],
            vec![json!({"type":"system","subtype":"compact_boundary"})],
            vec![json!({"type":"assistant","isApiErrorMessage":true,"message":{"content":[]}})],
            vec![json!({"type":"user","message":{"content":"hello"}})],
            vec![
                json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"why"}]}}),
            ],
            vec![
                json!({"type":"user","uuid":"u","message":{"content":"hello"}}),
                json!({"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"text","text":"answer"},{"type":"thinking","thinking":"why"},{"type":"tool_use","id":"t","name":"Read","input":{}}]}}),
                json!({"type":"user","uuid":"result","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
                json!({"type":"user","uuid":"interrupt","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
                json!({"type":"system","uuid":"turn","subtype":"turn_duration","durationMs":1}),
                json!({"type":"system","uuid":"compact","subtype":"compact_boundary"}),
                json!({"type":"user","uuid":"summary","isCompactSummary":true,"message":{"content":"summary"}}),
                json!({"type":"user","uuid":"task","origin":{"kind":"task-notification"},"message":{"content":"done"}}),
                json!({"type":"user","uuid":"agent","message":{"content":"<agent-message from=\"worker/local\" kind=\"message\">hello</agent-message>"}}),
                json!({"type":"assistant","uuid":"error","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"bad"}]}}),
                json!({"type":"future","uuid":"raw"}),
            ],
        ];
        for rows in pty_cases {
            let rows = rows
                .into_iter()
                .map(|row| serde_json::to_vec(&row).unwrap())
                .collect::<Vec<_>>();
            fold_pty_delivery_variants(&rows, &mut observed);
        }
        for (_, corpus) in [
            (
                "pong",
                include_str!("../../../claude-specs/fixtures/claude-pty/pong.rows.jsonl"),
            ),
            (
                "tools",
                include_str!("../../../claude-specs/fixtures/claude-pty/tools.rows.jsonl"),
            ),
            (
                "interrupt",
                include_str!("../../../claude-specs/fixtures/claude-pty/interrupt.rows.jsonl"),
            ),
            (
                "compact",
                include_str!("../../../claude-specs/fixtures/claude-pty/compact.rows.jsonl"),
            ),
        ] {
            fold_pty_delivery_variants(&rows(corpus), &mut observed);
        }

        let sdk_cases = [
            vec![json!({"type":"user","message":{"content":"hello"}})],
            vec![
                json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"hello"}}}),
            ],
            vec![json!({"type":"future"})],
            vec![json!({"type":"result","subtype":"success","result":"done"})],
            vec![
                json!({"type":"amux.claude_sdk.message","envelope":{"from":{"type":"human"},"text":"hello"}}),
            ],
            vec![
                json!({"type":"user","uuid":"u","message":{"content":"hello"}}),
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"answer"}}}),
                json!({"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"text","text":"answer"},{"type":"thinking","thinking":"why"},{"type":"tool_use","id":"t","name":"Read","input":{}}]}}),
                json!({"type":"user","uuid":"tool-result","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
                json!({"type":"system","subtype":"task_started","task_id":"task","status":"running"}),
                json!({"type":"result","uuid":"r","subtype":"success","result":"done"}),
                json!({"type":"amux.claude_sdk.message","envelope":{"id":"e","from":{"type":"human"},"text":"hello"}}),
                json!({"type":"user","uuid":"agent-user","message":{"content":"<agent-message from=\"worker/local\" kind=\"message\">hello</agent-message>"}}),
                json!({"type":"system","subtype":"compact_boundary","uuid":"compact","compact_metadata":{}}),
                json!({"type":"system","subtype":"status","status":"compacting","uuid":"status"}),
                json!({"type":"amux.claude_sdk.ready","uuid":"ready","session_id":"session"}),
                json!({"type":"future","uuid":"raw"}),
            ],
        ];
        for rows in sdk_cases {
            let rows = rows
                .into_iter()
                .map(|row| serde_json::to_vec(&row).unwrap())
                .collect::<Vec<_>>();
            fold_sdk_delivery_variants(&rows, &mut observed);
        }
        fold_sdk_delivery_variants(
            &rows(include_str!(
                "../../../ui-state/tests/spec/fixtures/claude_sdk/converse.rows.jsonl"
            )),
            &mut observed,
        );

        let codex_cases = [
            vec![json!({"type":"turn/diff/updated","diff":"x"})],
            vec![json!({"type":"item/tool/requestUserInput","questions":[]})],
            vec![json!({"type":"mcpServer/startupStatus/updated","name":"x","status":"ready"})],
            vec![json!({"type":"amux.codex_message","envelope":{"from":"human","text":"hello"}})],
            vec![json!({"type":"amux.codex_ready","resumed":true})],
            vec![
                json!({"type":"amux.codex_ready"}),
                json!({"type":"amux.codex_ready"}),
            ],
            vec![json!({"type":"amux.codex_gap","reason":"gap"})],
            vec![json!({"type":"error","message":"failed"})],
            vec![json!({"type":"future"})],
            vec![
                json!({"type":"item/completed","item":{"id":"u","type":"userMessage","content":[{"type":"text","text":"hello"}]}}),
                json!({"type":"item/completed","item":{"id":"m","type":"agentMessage","text":"answer"}}),
                json!({"type":"item/completed","item":{"id":"reason","type":"reasoning","text":"why"}}),
                json!({"type":"item/completed","item":{"id":"compact","type":"contextCompaction"}}),
                json!({"type":"turn/plan/updated","turnId":"turn","plan":[]}),
                json!({"type":"item/tool/call","callId":"tool","tool":"send","arguments":{}}),
                json!({"type":"amux.codex_approval_required","item_id":"tool","request_id":"request"}),
                json!({"type":"amux.codex_approval_resolved","item_id":"tool","request_id":"request","resolution":"answered"}),
                json!({"type":"amux.input_result","input_id":[1,2,3],"ok":{"text":"steer"}}),
                json!({"type":"turn/completed","turn":{"id":"turn","status":"completed"}}),
                json!({"type":"amux.codex_message","envelope":{"id":"env","from":"human","text":"hello"}}),
            ],
        ];
        for rows in codex_cases {
            let rows = rows
                .into_iter()
                .map(|row| serde_json::to_vec(&row).unwrap())
                .collect::<Vec<_>>();
            fold_codex_delivery_variants(&rows, &mut observed);
        }
        for (_, corpus) in CORPORA {
            fold_codex_delivery_variants(&rows(corpus), &mut observed);
        }

        let declared_set = declared.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(observed, declared_set);
        println!("delivery-keyed variants:\n{}", declared.join("\n"));
    }
}
