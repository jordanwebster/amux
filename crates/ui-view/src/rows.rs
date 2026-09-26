//! Chat rows: one row per item, the row id the item key. A run is an
//! attribute on its members; the newest member is the visible summary.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use serde_json::Value;
use ui_state::{Held, ItemBody, Key, SessionState};
use wire::{
    Attachment, BlobRef, BoundaryKind, DecisionOutcome, EnvelopeKind, FileChangeKind, ToolCall,
    ToolState, TurnOutcome,
};

use crate::ask::{QuestionView, question_view};
use crate::segments::{Segment, segments};

/// How many lines of a command's output a row carries.
pub const OUTPUT_HEAD_LINES: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: Key,
    pub order: u64,
    pub at_ms: i64,
    pub kind: RowKind,
    pub run: Option<RunInfo>,
    /// The client skips a collapsed row: an older run member, or a tool row
    /// when tool rows are hidden, or a row that carries nothing to draw.
    pub collapsed: bool,
    /// A permission decision, meta on the tool call's own row.
    pub decision: Option<Decision>,
    /// Drawn in the accent: an open ask points at it, or it failed.
    pub attention: bool,
    /// A subagent's own step: collapsed under the subagent's row, which
    /// opens to it.
    pub parent: Option<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub newest: Key,
    pub oldest: Key,
    pub reads: u32,
    pub searches: u32,
    pub len: u32,
    /// What the summary names: the newest member's subject.
    pub anchor: String,
    pub is_summary: bool,
    /// The run starts at the oldest held row and older history exists, so it
    /// may continue below: the summary reads "40+".
    pub open_below: bool,
}

/// Whether tool rows show, hide, or collapse into their runs.
#[derive(Clone, Copy, Debug)]
pub enum ToolRows<'a> {
    ShowAll,
    Hide,
    /// A run is expanded if any member key is in the set.
    CollapseRuns {
        expanded: &'a HashSet<Key>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ChatOptions<'a> {
    pub tools: ToolRows<'a>,
}

impl Default for ChatOptions<'_> {
    fn default() -> Self {
        ChatOptions {
            tools: ToolRows::ShowAll,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    Prompt {
        text: Vec<Segment>,
        steered: bool,
    },
    Prose {
        text: Vec<Segment>,
        streaming: bool,
        working_note: bool,
    },
    /// "Thought for 8s"; the text when the provider records it.
    Thinking {
        text: String,
        open: bool,
        duration_ms: Option<i64>,
    },
    ToolCall {
        server: String,
        tool: String,
        fact: String,
        state: ToolStateView,
        result: String,
    },
    FileChange {
        files: Vec<FileRow>,
        state: ToolStateView,
    },
    Command {
        command: String,
        state: ToolStateView,
        exit_code: Option<i32>,
        output_head: Vec<String>,
        more_lines: usize,
        duration_ms: Option<i64>,
    },
    Explore {
        verb: ExploreVerb,
        subject: String,
        state: ToolStateView,
    },
    Subagent {
        description: String,
        running: bool,
        tool_count: u32,
        last_tool: String,
        answer: String,
        duration_ms: Option<i64>,
    },
    Background {
        command: String,
        running: bool,
    },
    Image {
        image: Option<BlobRef>,
        path: String,
        generated: bool,
    },
    SlashOutput {
        command: String,
        args: String,
        output: String,
    },
    /// An ask that is the work: resolves in place.
    Ask(AskRow),
    TurnEnd {
        duration_ms: Option<i64>,
        cost_usd: Option<f64>,
        failed: bool,
    },
    Stopped,
    Compaction {
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        automatic: bool,
    },
    Error {
        error_kind: String,
        message: String,
        attempts: u32,
        gave_up: bool,
    },
    ModelSwitch {
        from: String,
        to: String,
        reason: String,
    },
    Boundary {
        kind: BoundaryKind,
        cause: String,
    },
    AgentMessage {
        from: String,
        kind: EnvelopeKind,
        text: String,
    },
    AutoReview {
        decision: String,
        risk: String,
        rationale: String,
        subject: Key,
    },
    Unrecognized {
        what: String,
        summary: String,
    },
    /// An item that belongs to the strip or the activity line, never a row.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AskRow {
    Question {
        questions: Vec<QuestionView>,
        answer: String,
        answered: bool,
    },
    Plan {
        plan: String,
        verdict: PlanVerdict,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanVerdict {
    Open,
    Approved,
    SentBack,
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStateView {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExploreVerb {
    Read,
    Search,
    List,
    Fetch,
    WebSearch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub change: FileChangeView,
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChangeView {
    Edited,
    Created { lines: u32 },
    Deleted,
    Moved { to: String },
}

/// A permission decision: allowed or denied, with scope and note when the
/// provider says them, and where it was answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub outcome: DecisionView,
    pub scope: Option<String>,
    pub note: Option<String>,
    /// Answered in the provider's own interface.
    pub elsewhere: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionView {
    Allowed,
    Denied,
    AutoApproved,
    /// Closed by a fact that did not say how.
    Dismissed,
}

/// Rows for the held items whose order falls in `range`, extended outward
/// across a run at either edge so its summary is always included: the
/// terminal's call, for what is on screen.
pub fn chat_rows(state: &SessionState, range: RangeInclusive<u64>, opts: &ChatOptions) -> Vec<Row> {
    let transcript = state.transcript();
    let mut lo = *range.start();
    let mut hi = *range.end();
    if let Some(run) = transcript.run_at(lo) {
        lo = lo.min(run.oldest);
    }
    if let Some(run) = transcript.run_at(hi) {
        hi = hi.max(run.newest);
    }
    if lo > hi {
        return Vec::new();
    }
    transcript
        .range(lo..=hi)
        .map(|held| row(state, held, opts))
        .collect()
}

/// Rows for these keys, in order, skipping keys not held: the phone's call,
/// for the cells an update changed.
pub fn chat_rows_for(state: &SessionState, keys: &[Key], opts: &ChatOptions) -> Vec<Row> {
    let transcript = state.transcript();
    let mut held: Vec<&Held> = keys.iter().filter_map(|key| transcript.get(key)).collect();
    held.sort_by_key(|held| held.item.order);
    held.dedup_by_key(|held| held.item.order);
    held.into_iter()
        .map(|held| row(state, held, opts))
        .collect()
}

fn row(state: &SessionState, held: &Held, opts: &ChatOptions) -> Row {
    let item = &held.item;
    let (kind, decision, failed) = kind_of(state, held);
    let transcript = state.transcript();
    let run = transcript.run_at(item.order).map(|run| RunInfo {
        is_summary: run.newest == item.order,
        anchor: transcript
            .at(run.newest)
            .map(subject_of)
            .unwrap_or_default(),
        newest: run.newest_key,
        oldest: run.oldest_key,
        reads: run.reads,
        searches: run.searches,
        len: run.len,
        open_below: run.open_below,
    });
    let is_tool = matches!(
        kind,
        RowKind::Explore { .. }
            | RowKind::Command { .. }
            | RowKind::ToolCall { .. }
            | RowKind::FileChange { .. }
            | RowKind::Background { .. }
    );
    let collapsed = matches!(kind, RowKind::Hidden)
        || match opts.tools {
            ToolRows::ShowAll => false,
            ToolRows::Hide => is_tool,
            ToolRows::CollapseRuns { expanded } => run
                .as_ref()
                .is_some_and(|run| !run.is_summary && !run_expanded(state, item.order, expanded)),
        };
    let asked = state
        .open_asks()
        .iter()
        .any(|ask| ask.item_key() == item.key);
    let parent = match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
            if !tool.parent_key.is_empty() =>
        {
            Some(tool.parent_key.clone())
        }
        _ => None,
    };
    let collapsed = collapsed || parent.is_some();
    Row {
        id: item.key.clone(),
        order: item.order,
        at_ms: item.at_ms,
        kind,
        run,
        collapsed,
        decision,
        attention: asked || failed,
        parent,
    }
}

fn run_expanded(state: &SessionState, order: u64, expanded: &HashSet<Key>) -> bool {
    let transcript = state.transcript();
    let Some(run) = transcript.run_at(order) else {
        return false;
    };
    transcript
        .range(run.oldest..=run.newest)
        .any(|held| expanded.contains(&held.item.key))
}

/// What a run's summary names: the newest member's subject.
fn subject_of(held: &Held) -> String {
    match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool)) => claude_subject(tool),
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => match &work.of {
            Some(wire::work::Of::Command(command)) => command.command.clone(),
            Some(wire::work::Of::WebSearch(search)) => search.query.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn state_view(state: i32) -> ToolStateView {
    match ToolState::try_from(state).unwrap_or(ToolState::Unspecified) {
        ToolState::Unspecified | ToolState::Pending => ToolStateView::Pending,
        ToolState::Running => ToolStateView::Running,
        ToolState::Succeeded => ToolStateView::Succeeded,
        ToolState::Failed => ToolStateView::Failed,
        ToolState::Denied => ToolStateView::Denied,
        ToolState::Cancelled => ToolStateView::Cancelled,
    }
}

fn decision_view(decision: Option<&wire::ToolDecision>) -> Option<Decision> {
    let decision = decision?;
    let outcome = match DecisionOutcome::try_from(decision.outcome).ok()? {
        DecisionOutcome::None => return None,
        DecisionOutcome::Allowed => DecisionView::Allowed,
        DecisionOutcome::Denied => DecisionView::Denied,
        DecisionOutcome::AutoApproved => DecisionView::AutoApproved,
        DecisionOutcome::Unknown => DecisionView::Dismissed,
    };
    let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    Some(Decision {
        outcome,
        scope: text(&decision.scope),
        note: text(&decision.note),
        elsewhere: decision.elsewhere,
    })
}

fn duration(held: &Held, ended: Option<i64>) -> Option<i64> {
    ended.map(|ended| (ended - held.item.at_ms).max(0))
}

/// The row kind, the decision meta, and whether it failed.
fn kind_of(state: &SessionState, held: &Held) -> (RowKind, Option<Decision>, bool) {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    let item = &held.item;
    let text = || segments(&item.text, &item.attachments);
    let plain = |kind| (kind, None, false);
    match &held.body {
        ItemBody::ClaudePty(kind) => match kind {
            Pty::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Pty::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Pty::Message(m) => plain(prose(held, m.complete, false)),
            Pty::Thinking(t) => plain(thinking(state, held, t.complete)),
            Pty::Tool(tool) => claude_tool(state, held, tool),
            Pty::Turn(turn) => plain(turn_row(held, turn)),
            Pty::Compaction(c) => plain(compaction(c)),
            Pty::CompactSummary(_) | Pty::Task(_) | Pty::Interruption(_) => plain(RowKind::Hidden),
            Pty::AgentMessage(m) => plain(agent_message(held, m)),
            Pty::ApiError(e) => error(e),
            Pty::Boundary(b) => plain(boundary(b)),
            Pty::Slash(s) => plain(slash(held, s)),
            Pty::Unrecognized(u) => plain(unrecognized(u)),
        },
        ItemBody::ClaudeSdk(kind) => match kind {
            Sdk::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Sdk::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Sdk::Message(m) => plain(prose(held, m.complete, false)),
            Sdk::Thinking(t) => plain(thinking(state, held, t.complete)),
            Sdk::Tool(tool) => claude_tool(state, held, tool),
            // A task is drawn on the Agent call that started it.
            Sdk::Task(task) if state.transcript().get(&task.tool_key).is_some() => {
                plain(RowKind::Hidden)
            }
            Sdk::Task(task) => plain(RowKind::Subagent {
                description: task.description.clone(),
                running: task.state() == wire::TaskState::Running,
                tool_count: task.tool_count,
                last_tool: task.last_tool.clone(),
                answer: String::new(),
                duration_ms: None,
            }),
            Sdk::Turn(turn) => plain(turn_row(held, turn)),
            Sdk::Status(_) => plain(RowKind::Hidden),
            Sdk::Boundary(b) => plain(boundary(b)),
            Sdk::AgentMessage(m) => plain(agent_message(held, m)),
            Sdk::ApiError(e) => error(e),
            Sdk::Slash(s) => plain(slash(held, s)),
            Sdk::Unrecognized(u) => plain(unrecognized(u)),
            Sdk::ModelSwitch(m) => plain(model_switch(m)),
            Sdk::Compaction(c) => plain(compaction(c)),
        },
        ItemBody::Codex(kind) => match kind {
            Codex::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Codex::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Codex::Message(m) => plain(prose(held, m.complete, false)),
            Codex::WorkingNote(m) => plain(prose(held, m.complete, true)),
            Codex::Reasoning(r) => {
                let text = if item.text.is_empty() {
                    r.summary.join("\n")
                } else {
                    item.text.clone()
                };
                plain(RowKind::Thinking {
                    text,
                    open: !r.complete,
                    duration_ms: thinking_duration(state, held, r.complete),
                })
            }
            Codex::Work(work) => codex_work(held, work),
            Codex::McpStartup(_) | Codex::TurnDiff(_) => plain(RowKind::Hidden),
            Codex::Turn(turn) => plain(turn_row(held, turn)),
            Codex::Boundary(b) => plain(boundary(b)),
            Codex::AgentMessage(m) => plain(agent_message(held, m)),
            Codex::Error(e) => error(e),
            Codex::Reroute(m) => plain(model_switch(m)),
            Codex::Unrecognized(u) => plain(unrecognized(u)),
            Codex::Verdict(v) => plain(RowKind::AutoReview {
                decision: v.decision.clone(),
                risk: v.risk.clone(),
                rationale: v.rationale.clone(),
                subject: v.item_key.clone(),
            }),
        },
        ItemBody::Undecodable => plain(RowKind::Unrecognized {
            what: item.kind.clone(),
            summary: String::new(),
        }),
    }
}

fn prose(held: &Held, complete: bool, working_note: bool) -> RowKind {
    RowKind::Prose {
        text: segments(&held.item.text, &held.item.attachments),
        streaming: !complete,
        working_note,
    }
}

fn thinking(state: &SessionState, held: &Held, complete: bool) -> RowKind {
    RowKind::Thinking {
        text: held.item.text.clone(),
        open: !complete,
        duration_ms: thinking_duration(state, held, complete),
    }
}

/// "Thought for 8s": the thinking item's time minus the item before it.
fn thinking_duration(state: &SessionState, held: &Held, complete: bool) -> Option<i64> {
    if !complete {
        return None;
    }
    let before = state
        .transcript()
        .range(0..=held.item.order.saturating_sub(1))
        .next_back()?;
    Some((held.item.at_ms - before.item.at_ms).max(0))
}

fn turn_row(held: &Held, turn: &wire::Turn) -> RowKind {
    let duration_ms =
        (turn.started_at_ms > 0).then(|| (held.item.at_ms - turn.started_at_ms).max(0));
    match TurnOutcome::try_from(turn.outcome).unwrap_or(TurnOutcome::Unspecified) {
        TurnOutcome::Interrupted => RowKind::Stopped,
        TurnOutcome::Failed => RowKind::TurnEnd {
            duration_ms,
            cost_usd: turn.cost_usd,
            failed: true,
        },
        TurnOutcome::Completed | TurnOutcome::Unspecified => RowKind::TurnEnd {
            duration_ms,
            cost_usd: turn.cost_usd,
            failed: false,
        },
    }
}

fn compaction(c: &wire::Compaction) -> RowKind {
    RowKind::Compaction {
        tokens_before: c.tokens_before,
        tokens_after: c.tokens_after,
        automatic: c.automatic,
    }
}

fn error(e: &wire::ApiError) -> (RowKind, Option<Decision>, bool) {
    let gave_up = !e.will_retry;
    (
        RowKind::Error {
            error_kind: e.error_kind.clone(),
            message: e.message.clone(),
            attempts: e.attempt,
            gave_up,
        },
        None,
        gave_up,
    )
}

fn boundary(b: &wire::Boundary) -> RowKind {
    RowKind::Boundary {
        kind: b.kind(),
        cause: b.cause.clone(),
    }
}

fn agent_message(held: &Held, m: &wire::AgentMessage) -> RowKind {
    let from = match m.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(wire::sender::Value::Agent(agent)) => agent.name.clone(),
        Some(wire::sender::Value::Human(_)) | None => String::new(),
    };
    RowKind::AgentMessage {
        from,
        kind: m.kind(),
        text: held.item.text.clone(),
    }
}

fn slash(held: &Held, s: &wire::SlashOutput) -> RowKind {
    RowKind::SlashOutput {
        command: s.command.clone(),
        args: s.args.clone(),
        output: held.item.text.clone(),
    }
}

fn model_switch(m: &wire::ModelSwitch) -> RowKind {
    RowKind::ModelSwitch {
        from: m.from.clone(),
        to: m.to.clone(),
        reason: m.reason.clone(),
    }
}

fn unrecognized(u: &wire::Unrecognized) -> RowKind {
    RowKind::Unrecognized {
        what: u.fact_type.clone(),
        summary: u.summary.clone(),
    }
}

fn input(tool: &ToolCall) -> Value {
    serde_json::from_slice(&tool.input_json).unwrap_or(Value::Null)
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn lines(text: &str) -> u32 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u32
    }
}

/// What a Claude call is about, for its row and a run's summary.
fn claude_subject(tool: &ToolCall) -> String {
    let input = input(tool);
    for name in [
        "file_path",
        "pattern",
        "url",
        "query",
        "path",
        "command",
        "description",
        "notebook_path",
    ] {
        let value = field(&input, name);
        if !value.is_empty() {
            return value;
        }
    }
    String::new()
}

fn output_head(text: &str) -> (Vec<String>, usize) {
    let all: Vec<&str> = text.lines().collect();
    let head = all
        .iter()
        .take(OUTPUT_HEAD_LINES)
        .map(|line| (*line).to_owned())
        .collect();
    (head, all.len().saturating_sub(OUTPUT_HEAD_LINES))
}

fn image_of(attachments: &[Attachment]) -> Option<BlobRef> {
    attachments
        .iter()
        .find_map(|attachment| match &attachment.of {
            Some(wire::attachment::Of::Image(blob)) => Some(blob.clone()),
            _ => None,
        })
}

fn claude_tool(
    state: &SessionState,
    held: &Held,
    tool: &ToolCall,
) -> (RowKind, Option<Decision>, bool) {
    let decision = decision_view(tool.decision.as_ref());
    let view = state_view(tool.state);
    let failed = view == ToolStateView::Failed;
    let input = input(tool);
    let subject = claude_subject(tool);
    let duration_ms = duration(held, tool.ended_at_ms);
    let kind = if let Some(image) = image_of(&tool.attachments) {
        RowKind::Image {
            image: Some(image),
            path: subject,
            generated: false,
        }
    } else if !tool.server.is_empty() {
        RowKind::ToolCall {
            server: tool.server.clone(),
            tool: tool.name.clone(),
            fact: first_fact(&input),
            state: view,
            result: tool.outcome_text.clone(),
        }
    } else {
        match tool.name.as_str() {
            "Read" => RowKind::Explore {
                verb: ExploreVerb::Read,
                subject,
                state: view,
            },
            "Grep" | "Glob" | "ToolSearch" => RowKind::Explore {
                verb: ExploreVerb::Search,
                subject,
                state: view,
            },
            "LS" => RowKind::Explore {
                verb: ExploreVerb::List,
                subject,
                state: view,
            },
            "WebFetch" => RowKind::Explore {
                verb: ExploreVerb::Fetch,
                subject,
                state: view,
            },
            "WebSearch" => RowKind::Explore {
                verb: ExploreVerb::WebSearch,
                subject,
                state: view,
            },
            "Bash" if tool.background => RowKind::Background {
                command: field(&input, "command"),
                running: in_flight(view),
            },
            "Bash" => {
                let (output_head, more_lines) = output_head(&tool.outcome_text);
                RowKind::Command {
                    command: field(&input, "command"),
                    state: view,
                    exit_code: tool.exit_code,
                    output_head,
                    more_lines,
                    duration_ms,
                }
            }
            "Edit" | "MultiEdit" | "NotebookEdit" => {
                let (added, removed) = claude_counts(tool, &input);
                RowKind::FileChange {
                    files: vec![FileRow {
                        path: subject,
                        change: FileChangeView::Edited,
                        added,
                        removed,
                    }],
                    state: view,
                }
            }
            "Write" => {
                let content = field(&input, "content");
                let updated = serde_json::from_slice::<Value>(&tool.outcome_json)
                    .ok()
                    .is_some_and(|out| out.get("type").and_then(Value::as_str) == Some("update"));
                let change = if updated {
                    FileChangeView::Edited
                } else {
                    FileChangeView::Created {
                        lines: lines(&content),
                    }
                };
                let (added, removed) = if updated {
                    claude_counts(tool, &input)
                } else {
                    (lines(&content), 0)
                };
                RowKind::FileChange {
                    files: vec![FileRow {
                        path: subject,
                        change,
                        added,
                        removed,
                    }],
                    state: view,
                }
            }
            "Agent" | "Task" => subagent(state, held, tool, &input),
            "AskUserQuestion" => {
                let questions = question_view(&input);
                let answered = matches!(view, ToolStateView::Succeeded);
                RowKind::Ask(AskRow::Question {
                    questions,
                    answer: tool.outcome_text.clone(),
                    answered,
                })
            }
            "ExitPlanMode" => {
                let verdict = match (view, decision.as_ref().map(|d| d.outcome)) {
                    (_, Some(DecisionView::Allowed | DecisionView::AutoApproved)) => {
                        PlanVerdict::Approved
                    }
                    (_, Some(DecisionView::Denied)) => PlanVerdict::SentBack,
                    (_, Some(DecisionView::Dismissed)) => PlanVerdict::Dismissed,
                    (ToolStateView::Succeeded, None) => PlanVerdict::Approved,
                    (ToolStateView::Denied | ToolStateView::Failed, None) => PlanVerdict::SentBack,
                    _ => PlanVerdict::Open,
                };
                return (
                    RowKind::Ask(AskRow::Plan {
                        plan: field(&input, "plan"),
                        verdict,
                    }),
                    None,
                    false,
                );
            }
            name if is_task_tool(name) => RowKind::Hidden,
            name => RowKind::ToolCall {
                server: String::new(),
                tool: name.to_owned(),
                fact: first_fact(&input),
                state: view,
                result: tool.outcome_text.clone(),
            },
        }
    };
    (kind, decision, failed)
}

/// The task-list tools feed the strip, not the chat.
fn is_task_tool(name: &str) -> bool {
    matches!(
        name,
        "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet"
    )
}

fn in_flight(view: ToolStateView) -> bool {
    matches!(view, ToolStateView::Pending | ToolStateView::Running)
}

/// The first string argument, as the one fact a generic tool row shows.
fn first_fact(input: &Value) -> String {
    input
        .as_object()
        .and_then(|object| object.values().find_map(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// Added and removed lines: from the provider's patch when it gave one,
/// else from the replaced and replacing text.
fn claude_counts(tool: &ToolCall, input: &Value) -> (u32, u32) {
    let out: Value = serde_json::from_slice(&tool.outcome_json).unwrap_or(Value::Null);
    if let Some(hunks) = out.get("structuredPatch").and_then(Value::as_array) {
        let mut added = 0;
        let mut removed = 0;
        for line in hunks
            .iter()
            .filter_map(|hunk| hunk.get("lines")?.as_array())
            .flatten()
        {
            match line.as_str().and_then(|line| line.chars().next()) {
                Some('+') => added += 1,
                Some('-') => removed += 1,
                _ => {}
            }
        }
        return (added, removed);
    }
    let edits = input
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![input.clone()]);
    edits.iter().fold((0, 0), |(added, removed), edit| {
        (
            added + lines(&field(edit, "new_string")),
            removed + lines(&field(edit, "old_string")),
        )
    })
}

fn subagent(state: &SessionState, held: &Held, tool: &ToolCall, input: &Value) -> RowKind {
    // Headless Claude reports a subagent's progress as a task item that
    // names this call; terminal Claude reports it on the call itself.
    let task = state
        .transcript()
        .referrer(&held.item.key)
        .and_then(|referrer| match &referrer.body {
            ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Task(task)) => Some(task.clone()),
            _ => None,
        });
    let progress = tool.subagent.clone().unwrap_or_default();
    let view = state_view(tool.state);
    match task {
        Some(task) => RowKind::Subagent {
            description: task.description,
            running: task.state == wire::TaskState::Running as i32,
            tool_count: task.tool_count,
            last_tool: task.last_tool,
            answer: String::new(),
            duration_ms: None,
        },
        None => RowKind::Subagent {
            description: field(input, "description"),
            running: in_flight(view) || (tool.background && !progress.finished),
            tool_count: progress.tool_count,
            last_tool: progress.last_tool,
            answer: tool.outcome_text.clone(),
            duration_ms: duration(held, tool.ended_at_ms),
        },
    }
}

fn codex_work(held: &Held, work: &wire::Work) -> (RowKind, Option<Decision>, bool) {
    use wire::work::Of;
    let decision = decision_view(work.decision.as_ref());
    let view = state_view(work.state);
    let failed = view == ToolStateView::Failed;
    let exploring = work.class == wire::ToolClass::Exploration as i32;
    let kind = match &work.of {
        Some(Of::Command(command)) if exploring => {
            let actions: Vec<&str> = command.action.split(',').collect();
            let verb = if actions.contains(&"read") {
                ExploreVerb::Read
            } else if actions.contains(&"search") {
                ExploreVerb::Search
            } else {
                ExploreVerb::List
            };
            RowKind::Explore {
                verb,
                subject: command.command.clone(),
                state: view,
            }
        }
        Some(Of::Command(command)) if command.background => RowKind::Background {
            command: command.command.clone(),
            running: in_flight(view),
        },
        Some(Of::Command(command)) => {
            let (output_head, more_lines) = output_head(&held.item.text);
            RowKind::Command {
                command: command.command.clone(),
                state: view,
                exit_code: command.exit_code,
                output_head,
                more_lines,
                duration_ms: duration(held, work.ended_at_ms),
            }
        }
        Some(Of::FileChange(change)) => RowKind::FileChange {
            files: change.changes.iter().map(codex_file).collect(),
            state: view,
        },
        Some(Of::Mcp(call)) => RowKind::ToolCall {
            server: call.server.clone(),
            tool: call.tool.clone(),
            fact: first_fact(&serde_json::from_slice(&call.arguments_json).unwrap_or(Value::Null)),
            state: view,
            result: if call.error.is_empty() {
                String::from_utf8_lossy(&call.result_json).into_owned()
            } else {
                call.error.clone()
            },
        },
        Some(Of::WebSearch(search)) => RowKind::Explore {
            verb: ExploreVerb::WebSearch,
            subject: search.query.clone(),
            state: view,
        },
        Some(Of::Image(image)) => RowKind::Image {
            image: image_of(&held.item.attachments),
            path: image.path.clone(),
            generated: image.generated,
        },
        Some(Of::Collab(collab)) => RowKind::Subagent {
            description: collab.prompt.clone(),
            running: in_flight(view),
            tool_count: 0,
            last_tool: collab.tool.clone(),
            answer: String::new(),
            duration_ms: duration(held, work.ended_at_ms),
        },
        None => RowKind::Unrecognized {
            what: "work".into(),
            summary: String::new(),
        },
    };
    (kind, decision, failed)
}

fn codex_file(change: &wire::FileChange) -> FileRow {
    let (added, removed) = patch_counts(&change.patch);
    let change_view =
        match FileChangeKind::try_from(change.kind).unwrap_or(FileChangeKind::Unspecified) {
            FileChangeKind::Add => FileChangeView::Created { lines: added },
            FileChangeKind::Delete => FileChangeView::Deleted,
            _ if !change.move_to.is_empty() => FileChangeView::Moved {
                to: change.move_to.clone(),
            },
            _ => FileChangeView::Edited,
        };
    FileRow {
        path: change.path.clone(),
        change: change_view,
        added,
        removed,
    }
}

/// Added and removed lines of a unified diff body.
pub(crate) fn patch_counts(patch: &str) -> (u32, u32) {
    let mut added = 0;
    let mut removed = 0;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        match line.chars().next() {
            Some('+') => added += 1,
            Some('-') => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}
