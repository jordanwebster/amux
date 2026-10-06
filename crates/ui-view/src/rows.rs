//! Chat rows: one row per item, the row id the item key. A run is an
//! attribute on its members; the newest member is the visible summary.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use ui_state::{Held, ItemBody, Key, SessionState};
use wire::{
    Attachment, BlobRef, BoundaryKind, DecisionOutcome, EnvelopeKind, FileChangeKind, SendState,
    ToolCall, ToolState, TurnOutcome,
};

use crate::ask::{QuestionView, lifted, question, question_view, recorded_answers};
use crate::segments::{Segment, segments};

/// How many lines of a command's output a row carries.
pub const OUTPUT_HEAD_LINES: usize = 3;
/// The output's last lines a command row carries.
pub const OUTPUT_TAIL_LINES: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
    /// Drawn in the attention ink: an open ask points at it, or it failed.
    pub attention: bool,
    /// A subagent's own step: collapsed under the subagent's row, which
    /// opens to it.
    pub parent: Option<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
        /// The output's last lines, for a step opened to show how it
        /// ended; the lines before them number `more_lines` plus the head,
        /// less these.
        output_tail: Vec<String>,
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
        /// How long it ran, once it ended.
        duration_ms: Option<i64>,
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
        /// A message this agent sent: the recipient as the model named it,
        /// and how the send went; empty and unspecified on one it received.
        to: String,
        sent: SendState,
        rejection: String,
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

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum AskRow {
    /// A Claude AskUserQuestion call: its questions, then what was picked
    /// and typed for each, read from the tool's recorded result.
    Question {
        questions: Vec<QuestionView>,
        /// One per question once answered, in the ask's order.
        answers: Vec<AnswerView>,
        /// The note that went out with the answers.
        note: Option<String>,
        resolution: Resolution,
    },
    Plan {
        plan: String,
        verdict: PlanVerdict,
        /// Approved with edits accepted without asking from then on: the
        /// decision's scope names Claude's `acceptEdits` mode.
        edits_accepted: bool,
        /// Why it was sent back, when the person said.
        note: Option<String>,
        /// Still being written: it grows as it streams.
        writing: bool,
    },
    /// Questions asked as the work, then the answers sent.
    Questions {
        questions: Vec<QuestionView>,
        /// One per question once answered, in the ask's order.
        answers: Vec<AnswerView>,
        note: Option<String>,
        resolution: Resolution,
    },
    /// A tool server's form: "Sent 3 fields to github".
    Form {
        server: String,
        message: String,
        /// The names of the fields sent.
        fields: Vec<String>,
        resolution: Resolution,
    },
    /// A tool server's link to open.
    Link {
        server: String,
        message: String,
        url: String,
        resolution: Resolution,
    },
    /// Files and network beyond the sandbox: "Granted write target/ this
    /// turn".
    Grant {
        reason: String,
        read: Vec<String>,
        write: Vec<String>,
        network: bool,
        hosts: Vec<String>,
        granted: Option<Granted>,
        resolution: Resolution,
    },
    /// A dialog the provider showed that this build could not read: it was
    /// answered in the provider's own terminal or ended by a stop.
    Unanswerable {
        reason: String,
        resolution: Resolution,
    },
}

/// How an ask that is the work closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Resolution {
    Open,
    /// Answered, sent, opened or granted.
    Answered,
    /// Declined, or a grant that granted nothing.
    Declined,
    Cancelled,
    /// Closed by a fact that did not say how.
    Dismissed,
}

/// One question's answer: the picked options, a typed answer, or a secret
/// answer that reads "answered (hidden)".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AnswerView {
    /// "(Recommended)" lifted off, as on the card.
    pub picked: Vec<String>,
    pub other: Option<String>,
    pub hidden: bool,
}

/// What an access grant granted, and for how long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Granted {
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub network: bool,
    pub for_session: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum PlanVerdict {
    Open,
    Approved,
    SentBack,
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ToolStateView {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ExploreVerb {
    Read,
    Search,
    List,
    Fetch,
    WebSearch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FileRow {
    pub path: String,
    pub change: FileChangeView,
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum FileChangeView {
    Edited,
    Created { lines: u32 },
    Deleted,
    Moved { to: String },
}

/// A permission decision: allowed or denied, with scope and note when the
/// provider says them, and where it was answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Decision {
    pub outcome: DecisionView,
    pub scope: Option<String>,
    pub note: Option<String>,
    /// Answered in the provider's own interface.
    pub elsewhere: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
    let parent = parent_of(held);
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

/// The subagent call a step belongs to, when it is a subagent's own step.
pub(crate) fn parent_of(held: &Held) -> Option<Key> {
    match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
            if !tool.parent_key.is_empty() =>
        {
            Some(tool.parent_key.clone())
        }
        _ => None,
    }
}

fn run_expanded(state: &SessionState, order: u64, expanded: &HashSet<Key>) -> bool {
    let transcript = state.transcript();
    let Some(run) = transcript.run_at(order) else {
        return false;
    };
    // The expansion set is a handful of keys and a run can be thousands of
    // calls long: look each key up rather than walking the run.
    expanded.iter().any(|key| {
        transcript
            .get(key)
            .is_some_and(|held| (run.oldest..=run.newest).contains(&held.item.order))
    })
}

/// What a run's summary names: the newest member's subject.
pub(crate) fn subject_of(held: &Held) -> String {
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
pub(crate) fn kind_of(state: &SessionState, held: &Held) -> (RowKind, Option<Decision>, bool) {
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
            Pty::Ask(ask) => plain(ask_row(ask)),
            Pty::Plan(plan) => plain(plan_row(held, plan)),
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
            Sdk::Plan(plan) => plain(plan_row(held, plan)),
            Sdk::ModelSwitch(m) => plain(model_switch(m)),
            Sdk::Compaction(c) => plain(compaction(c)),
            Sdk::Ask(ask) => plain(ask_row(ask)),
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
            Codex::Ask(ask) => plain(ask_row(ask)),
        },
        ItemBody::Undecodable => plain(RowKind::Unrecognized {
            what: item.kind.clone(),
            summary: String::new(),
        }),
    }
}

/// An ask that is the work, drawn from its own item.
fn ask_row(item: &wire::AskItem) -> RowKind {
    use wire::ask_item::Ask;
    let closed = item.closed.clone().unwrap_or_default();
    let resolution = match item.closed.as_ref().map(|closed| closed.outcome()) {
        None => Resolution::Open,
        Some(wire::AskOutcome::Answered) => Resolution::Answered,
        Some(wire::AskOutcome::Declined) => Resolution::Declined,
        Some(wire::AskOutcome::Cancelled) => Resolution::Cancelled,
        Some(wire::AskOutcome::Dismissed | wire::AskOutcome::Unspecified) => Resolution::Dismissed,
    };
    RowKind::Ask(match &item.ask {
        Some(Ask::Question(asked)) => AskRow::Questions {
            questions: asked.questions.iter().map(question).collect(),
            answers: closed
                .answers
                .iter()
                .map(|answer| AnswerView {
                    picked: answer
                        .picked
                        .iter()
                        .map(|label| lifted(label, "", "", false).label)
                        .collect(),
                    other: answer.other.clone(),
                    hidden: answer.hidden,
                })
                .collect(),
            note: (!closed.note.is_empty()).then_some(closed.note),
            resolution,
        },
        Some(Ask::Form(form)) => AskRow::Form {
            server: form.server.clone(),
            message: form.message.clone(),
            fields: closed.fields,
            resolution,
        },
        Some(Ask::Link(link)) => AskRow::Link {
            server: link.server.clone(),
            message: link.message.clone(),
            url: link.url.clone(),
            resolution,
        },
        Some(Ask::Access(access)) => AskRow::Grant {
            reason: access.reason.clone(),
            read: access.read.clone(),
            write: access.write.clone(),
            network: access.network,
            hosts: access.network_hosts.clone(),
            granted: closed.grant.map(|grant| Granted {
                read: grant.read,
                write: grant.write,
                network: grant.network,
                for_session: grant.for_session,
            }),
            resolution,
        },
        Some(Ask::Unanswerable(unanswerable)) => AskRow::Unanswerable {
            reason: unanswerable.reason.clone(),
            resolution,
        },
        None => {
            return RowKind::Unrecognized {
                what: "ask".into(),
                summary: String::new(),
            };
        }
    })
}

/// A plan item: the plan is the item's text.
fn plan_row(held: &Held, plan: &wire::Plan) -> RowKind {
    let (verdict, edits_accepted) = match plan.verdict() {
        wire::PlanVerdict::Undecided => (PlanVerdict::Open, false),
        wire::PlanVerdict::Approved => (PlanVerdict::Approved, false),
        wire::PlanVerdict::ApprovedAcceptingEdits => (PlanVerdict::Approved, true),
        wire::PlanVerdict::SentBack => (PlanVerdict::SentBack, false),
        wire::PlanVerdict::Dismissed => (PlanVerdict::Dismissed, false),
    };
    RowKind::Ask(AskRow::Plan {
        plan: held.item.text.clone(),
        verdict,
        edits_accepted,
        note: plan.note.clone(),
        writing: !plan.complete,
    })
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
/// None when that is nothing: terminal Claude's thinking row lands with
/// the item before it, so the two times say nothing about how long it
/// thought.
fn thinking_duration(state: &SessionState, held: &Held, complete: bool) -> Option<i64> {
    if !complete {
        return None;
    }
    let before = state
        .transcript()
        .range(0..=held.item.order.saturating_sub(1))
        .next_back()?;
    Some(held.item.at_ms - before.item.at_ms).filter(|ms| *ms > 0)
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
        to: m.to.clone(),
        sent: m.send_state(),
        rejection: m.rejection.clone(),
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

fn output_tail(text: &str) -> Vec<String> {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(OUTPUT_TAIL_LINES)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
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
    } else if let Some(kind) = crate::plan::plan_file_row(
        state,
        held,
        &tool.name,
        &field(&input, "file_path"),
        field(&input, "content"),
        in_flight(view),
    ) {
        return (kind, None, false);
    } else if let Some(verb) = explore_verb(tool.class) {
        RowKind::Explore {
            verb,
            subject,
            state: view,
        }
    } else {
        match tool.name.as_str() {
            "Bash" if tool.background => RowKind::Background {
                command: field(&input, "command"),
                running: in_flight(view),
                duration_ms,
            },
            "Bash" => {
                let (output_head, more_lines) = output_head(&tool.outcome_text);
                RowKind::Command {
                    command: field(&input, "command"),
                    state: view,
                    exit_code: tool.exit_code,
                    output_head,
                    more_lines,
                    output_tail: output_tail(&tool.outcome_text),
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
            "Agent" | "Task" => subagent(held, tool, &input),
            "AskUserQuestion" => {
                let questions = question_view(&input);
                // A question cannot be declined: one that closed without
                // answers was dismissed, by the person or by a stop.
                let resolution = match view {
                    ToolStateView::Pending | ToolStateView::Running => Resolution::Open,
                    ToolStateView::Succeeded => Resolution::Answered,
                    ToolStateView::Denied | ToolStateView::Failed | ToolStateView::Cancelled => {
                        Resolution::Dismissed
                    }
                };
                let answers = match resolution {
                    Resolution::Answered => recorded_answers(&questions, &tool.outcome_json),
                    _ => Vec::new(),
                };
                RowKind::Ask(AskRow::Question {
                    questions,
                    answers,
                    note: decision.as_ref().and_then(|decision| decision.note.clone()),
                    resolution,
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
                // Claude fills the plan in from its plan file; a call that
                // carries none reads it from that file's Write.
                let mut plan = field(&input, "plan");
                if plan.is_empty() {
                    plan = crate::plan::written_before(state, held.item.order);
                }
                return (
                    RowKind::Ask(AskRow::Plan {
                        plan,
                        verdict,
                        edits_accepted: verdict == PlanVerdict::Approved
                            && decision
                                .as_ref()
                                .and_then(|decision| decision.scope.as_deref())
                                == Some("acceptEdits"),
                        note: decision.as_ref().and_then(|decision| decision.note.clone()),
                        writing: false,
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

/// The verb a call that only looks is drawn with; none for one drawn its
/// own way.
fn explore_verb(class: i32) -> Option<ExploreVerb> {
    match wire::ToolClass::try_from(class) {
        Ok(wire::ToolClass::Read) => Some(ExploreVerb::Read),
        Ok(wire::ToolClass::Search) => Some(ExploreVerb::Search),
        Ok(wire::ToolClass::List) => Some(ExploreVerb::List),
        Ok(wire::ToolClass::Fetch) => Some(ExploreVerb::Fetch),
        Ok(wire::ToolClass::WebSearch) => Some(ExploreVerb::WebSearch),
        Ok(
            wire::ToolClass::Look | wire::ToolClass::Unspecified | wire::ToolClass::Consequential,
        )
        | Err(_) => None,
    }
}

fn subagent(held: &Held, tool: &ToolCall, input: &Value) -> RowKind {
    let progress = tool.subagent.clone().unwrap_or_default();
    let view = state_view(tool.state);
    RowKind::Subagent {
        description: field(input, "description"),
        running: in_flight(view) || (tool.background && !progress.finished),
        tool_count: progress.tool_count,
        last_tool: progress.last_tool,
        answer: tool.outcome_text.clone(),
        duration_ms: duration(held, tool.ended_at_ms),
    }
}

fn codex_work(held: &Held, work: &wire::Work) -> (RowKind, Option<Decision>, bool) {
    use wire::work::Of;
    let decision = decision_view(work.decision.as_ref());
    let view = state_view(work.state);
    let failed = view == ToolStateView::Failed;
    let kind = match (&work.of, explore_verb(work.class)) {
        (Some(Of::Command(command)), Some(verb)) => RowKind::Explore {
            verb,
            subject: command.command.clone(),
            state: view,
        },
        (of, _) => match of {
            Some(Of::Command(command)) if command.background => RowKind::Background {
                command: command.command.clone(),
                running: in_flight(view),
                duration_ms: duration(held, work.ended_at_ms),
            },
            Some(Of::Command(command)) => {
                let (output_head, more_lines) = output_head(&held.item.text);
                RowKind::Command {
                    command: command.command.clone(),
                    state: view,
                    exit_code: command.exit_code,
                    output_head,
                    more_lines,
                    output_tail: output_tail(&held.item.text),
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
                fact: first_fact(
                    &serde_json::from_slice(&call.arguments_json).unwrap_or(Value::Null),
                ),
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

/// One line of a landed edit's patch, numbered on the side it belongs to
/// (the new file for context and added lines, the old for removed ones)
/// when the provider said where its hunk starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PatchLine {
    pub number: Option<u32>,
    pub kind: crate::review::LineKind,
    pub text: String,
}

/// The head of a landed file change: its first file's first lines, and
/// how many lines of that file's patch follow them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PatchHead {
    pub lines: Vec<PatchLine>,
    pub more: usize,
}

/// The first `max` lines of the patch a landed file change made, for a
/// chat to show under its row; None for a row that changed no file, has
/// not landed, or carries no patch.
pub fn patch_head(state: &SessionState, key: &Key, max: usize) -> Option<PatchHead> {
    let held = state.transcript().get(key)?;
    let lines = match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool)) => {
            if state_view(tool.state) != ToolStateView::Succeeded {
                return None;
            }
            claude_patch(tool)?
        }
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => {
            if state_view(work.state) != ToolStateView::Succeeded {
                return None;
            }
            match &work.of {
                Some(wire::work::Of::FileChange(change)) => {
                    unified_lines(&change.changes.first()?.patch)
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    if lines.is_empty() {
        return None;
    }
    let more = lines.len().saturating_sub(max);
    Some(PatchHead {
        lines: lines.into_iter().take(max).collect(),
        more,
    })
}

fn claude_patch(tool: &ToolCall) -> Option<Vec<PatchLine>> {
    use crate::review::LineKind;
    if !matches!(
        tool.name.as_str(),
        "Edit" | "MultiEdit" | "NotebookEdit" | "Write"
    ) {
        return None;
    }
    let out: Value = serde_json::from_slice(&tool.outcome_json).unwrap_or(Value::Null);
    if let Some(hunks) = out.get("structuredPatch").and_then(Value::as_array)
        && !hunks.is_empty()
    {
        let mut lines = Vec::new();
        for hunk in hunks {
            let start = |name: &str| hunk.get(name).and_then(Value::as_u64).map(|n| n as u32);
            let (mut old, mut new) = (start("oldStart"), start("newStart"));
            for line in hunk
                .get("lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                lines.push(numbered(line, &mut old, &mut new));
            }
        }
        return Some(lines);
    }
    let input = input(tool);
    if tool.name == "Write" {
        return Some(
            field(&input, "content")
                .lines()
                .enumerate()
                .map(|(i, text)| PatchLine {
                    number: Some(i as u32 + 1),
                    kind: LineKind::Added,
                    text: text.to_owned(),
                })
                .collect(),
        );
    }
    let edits = input
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![input.clone()]);
    let mut lines = Vec::new();
    for edit in &edits {
        for (name, kind) in [
            ("old_string", LineKind::Removed),
            ("new_string", LineKind::Added),
        ] {
            lines.extend(field(edit, name).lines().map(|text| PatchLine {
                number: None,
                kind,
                text: text.to_owned(),
            }));
        }
    }
    Some(lines)
}

/// One patch line, numbered from the running old and new positions.
fn numbered(line: &str, old: &mut Option<u32>, new: &mut Option<u32>) -> PatchLine {
    use crate::review::LineKind;
    let (kind, text) = match line.chars().next() {
        Some('+') => (LineKind::Added, &line[1..]),
        Some('-') => (LineKind::Removed, &line[1..]),
        Some(' ') => (LineKind::Context, &line[1..]),
        _ => (LineKind::Context, line),
    };
    let number = if kind == LineKind::Removed {
        *old
    } else {
        *new
    };
    let step = |at: &mut Option<u32>| {
        if let Some(at) = at.as_mut() {
            *at += 1;
        }
    };
    match kind {
        LineKind::Removed => step(old),
        LineKind::Added => step(new),
        LineKind::Context => {
            step(old);
            step(new);
        }
    }
    PatchLine {
        number,
        kind,
        text: text.to_owned(),
    }
}

/// The body lines of a unified diff, numbered from its hunk headers.
fn unified_lines(patch: &str) -> Vec<PatchLine> {
    let mut lines = Vec::new();
    let (mut old, mut new) = (None, None);
    // Lines each side of the open hunk still holds, from its header: a
    // file's header lines ("--- ", "+++ ", "index ") come only between
    // hunks, so a removed "-- comment" inside one is a line of the patch.
    let (mut old_left, mut new_left) = (0u32, 0u32);
    for line in patch.lines() {
        if line.starts_with('\\') {
            continue;
        }
        if old_left > 0 || new_left > 0 {
            match line.chars().next() {
                Some('-') => old_left = old_left.saturating_sub(1),
                Some('+') => new_left = new_left.saturating_sub(1),
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            lines.push(numbered(line, &mut old, &mut new));
            continue;
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            let range = |sign: char| {
                let range = header
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix(sign))?;
                let mut parts = range.split(',');
                let start = parts.next()?.parse().ok()?;
                let count = parts.next().map_or(Some(1), |count| count.parse().ok())?;
                Some((start, count))
            };
            let (from, to) = (range('-'), range('+'));
            (old, new) = (from.map(|(start, _)| start), to.map(|(start, _)| start));
            (old_left, new_left) = (
                from.map_or(0, |(_, count)| count),
                to.map_or(0, |(_, count)| count),
            );
            continue;
        }
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
        {
            continue;
        }
        lines.push(numbered(line, &mut old, &mut new));
    }
    lines
}

/// The subjects of the newest `n` members of the run whose summary sits at
/// `order`, newest first: what a collapsed run names beside its counts.
pub fn run_subjects(state: &SessionState, order: u64, n: usize) -> Vec<String> {
    let transcript = state.transcript();
    let Some(run) = transcript.run_at(order) else {
        return Vec::new();
    };
    let mut subjects: Vec<String> = Vec::new();
    for held in transcript.range(run.oldest..=run.newest).rev() {
        let subject = subject_of(held);
        if !subject.is_empty() && !subjects.contains(&subject) {
            subjects.push(subject);
        }
        if subjects.len() == n {
            break;
        }
    }
    subjects
}
