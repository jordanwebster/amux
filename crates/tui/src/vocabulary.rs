//! The chat vocabulary as terminal components, drawn from authored view
//! values rather than from sessions: every row kind, the activity line,
//! each ask body with its choices, the unanswerable escape, not-confirmed
//! prompts and answers, the exited composer and the review page, once each,
//! and the session strip once per provider kind. The renderer never sees
//! the provider kind, and ui-view's own goldens prove the projection per
//! kind, so one drawing of each component is the whole terminal claim.
//!
//! The component goldens and the PNG renderer both draw this set.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ui_state::{Activity, ActivityKind, Composer, Waiting};
use ui_view::{
    AnswerView, AskBody, AskCard, AskRow, AttachmentView, Away, CardState, Choice, ChoiceOutcome,
    ContextView, Decision, DecisionView, ExploreVerb, FileChangeView, FileRow, Granted, OptionView,
    OutboxRow, OutboxState, PlanVerdict, QuestionView, QueuedRow, Resolution, Row, RowKind,
    RunInfo, Scope, Segment, ServerView, SignInView, Strip, TasksView, ToolStateView, UsageView,
};
use wire::{BlobRef, BoundaryKind, EnvelopeKind, SendState, SignInState};

use crate::chat::ask::AskUi;
use crate::chat::composer::{
    TrayRow, activity_line, editor_lines, foot_cards, placeholder, strip_line,
};
use crate::chat::review::ReviewPage;
use crate::chat::rows::{RowState, row_lines};
use crate::chat::{hint_line, on_panel};
use crate::editor::Editor;
use crate::theme::Theme;

/// Columns every component is drawn at.
pub const WIDTH: u16 = 100;
/// Rows the full-screen review page is drawn at.
const PAGE_HEIGHT: u16 = 30;
/// What terminal Claude's interpreter says when Claude shows a tool server's
/// form in its own terminal.
const UNANSWERABLE: &str = "Claude is showing a form from a tool server this build can't read. Attach to Claude's terminal to answer it, or stop the agent.";

/// The whole frames drawn from served hosts, with what each shows; the
/// frames test draws exactly these.
pub const FRAMES: &[(&str, &str)] = &[
    (
        "fleet",
        "The fleet at 110 columns as the laptop sees it: a terminal Claude, a headless Claude and a Codex on the desk, every one idle, with the trusted host count.",
    ),
    (
        "fleet_60col",
        "The same fleet at 60 columns: rows keep name, kind and host and drop the prompt and age.",
    ),
    (
        "chat_strip",
        "A headless Claude chat on the desk read from the laptop: header with model and mode, the first turn, the composer and its keys.",
    ),
    (
        "ask_escape",
        "A terminal Claude on the desk showing a tool server's sign-in dialog in its own terminal, read from the laptop: the escape card docked where the composer was, its reason sending the person to Claude's own terminal and Stop as the one choice (a remote agent's terminal is not offered here), with the conversation still above it.",
    ),
    (
        "rewind_before_swap",
        "The same chat after the desk rewound under it: the Reset is pending, the second turn's rows stay on screen and the header says the host is away until the rebuilt transcript catches up.",
    ),
    (
        "rewind_after_swap",
        "After the CaughtUp: the rebuilt transcript swapped in, the rewound turn gone, and the composer live again.",
    ),
];

/// One named component drawn in `theme`.
pub struct Component {
    pub name: &'static str,
    /// What it shows, for the index of goldens.
    pub shows: &'static str,
    pub buffer: Buffer,
}

/// Every component, in catalogue order.
pub fn components(theme: Theme) -> Vec<Component> {
    let w = usize::from(WIDTH);
    let mut out = Vec::new();
    let mut add = |name, shows, lines: Vec<Line<'static>>| {
        out.push(Component {
            name,
            shows,
            buffer: paint_lines(lines),
        });
    };
    for (name, shows, rows) in row_sets() {
        let lines = rows
            .iter()
            .flat_map(|(row, state)| row_lines(row, *state, w, theme))
            .collect();
        add(name, shows, lines);
    }
    add(
        "activity_line",
        "The line above the composer for each kind of work: working, thinking, a running tool with its subject, subagents, compacting and retrying with and without a cap.",
        activities()
            .iter()
            .map(|(activity, running)| activity_line(activity, *running, w, theme))
            .collect(),
    );
    for (name, shows, card, attach) in cards() {
        let mut ui = AskUi::default();
        ui.sync(&card);
        let lines = ui.render(&card, "fixer", attach, w, theme).lines;
        add(name, shows, on_panel(lines, w, theme));
    }
    for (name, shows, strip) in strips() {
        let mut lines: Vec<Line<'static>> = strip_line(&strip, w, theme).into_iter().collect();
        lines.extend(foot_cards(&strip, w, theme));
        add(name, shows, lines);
    }
    add(
        "tray_not_confirmed",
        "The tray under the feed: a queued prompt that can be sent now or withdrawn, a prompt that was sent but never confirmed, selected with resend and discard, a rejected one, and the keys the selected row takes.",
        tray(w, theme),
    );
    for (name, shows, composer, away, draft) in composers() {
        let mut editor = Editor::default();
        editor.set(draft, Vec::new());
        let (mut lines, _) = editor_lines(
            &editor,
            &placeholder(&composer, "fixer", "studio", away),
            w,
            theme,
        );
        lines.insert(0, Line::default());
        lines.push(hint_line(&composer, away, false, &editor, w, theme));
        add(name, shows, lines);
    }
    out.push(Component {
        name: "review_page",
        shows: "The full-screen review page over a working-tree diff: title with head and totals, the file list with status, counts and comments, hunks with added and removed lines tinted, a saved comment under its line and the comment editor open on another.",
        buffer: review_page(theme),
    });
    out
}

fn paint_lines(lines: Vec<Line<'static>>) -> Buffer {
    let height = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("test terminal");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn review_page(theme: Theme) -> Buffer {
    let patch = "\
diff --git a/src/sum.rs b/src/sum.rs
index 1111111..2222222 100644
--- a/src/sum.rs
+++ b/src/sum.rs
@@ -1,5 +1,6 @@
 fn main() {
     let a = 1;
-    let b = 2;
-    println!(\"{}\", a + b);
+    let b = 3;
+    let c = 4;
+    println!(\"{}\", a + b + c);
 }
diff --git a/NOTES.md b/NOTES.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/NOTES.md
@@ -0,0 +1 @@
+Remember the sum changed.
";
    let diff = wire::Diff {
        patch: Some(BlobRef {
            hash: vec![7; 32],
            name: "working-tree.diff".into(),
            mime: "text/x-diff".into(),
            size: patch.len() as u64,
        }),
        base: Some(wire::DiffBase {
            base: Some(wire::diff_base::Base::WorkingTree(wire::Empty {})),
        }),
        head: "0463fb2c".into(),
        merge_base: None,
    };
    let mut page = ReviewPage::new(diff, patch.into());
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('c')));
    for c in "why 3? the spec says b stays 2".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    page.key(key(KeyCode::Enter));
    page.key(key(KeyCode::Char('j')));
    page.key(key(KeyCode::Char('c')));
    for c in "and c".chars() {
        page.key(key(KeyCode::Char(c)));
    }
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, PAGE_HEIGHT)).expect("test terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            page.draw(frame, area, None, theme);
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

// --- rows ------------------------------------------------------------------

fn row(kind: RowKind) -> Row {
    Row {
        id: "k".into(),
        order: 1,
        at_ms: 0,
        kind,
        run: None,
        collapsed: false,
        decision: None,
        attention: false,
        parent: None,
    }
}

fn text(words: &str) -> Vec<Segment> {
    vec![Segment::Text(words.into())]
}

const CLOSED: RowState = RowState {
    focused: false,
    expanded: false,
};
const OPEN: RowState = RowState {
    focused: false,
    expanded: true,
};
const FOCUSED: RowState = RowState {
    focused: true,
    expanded: false,
};

fn decided(mut row: Row, outcome: DecisionView, scope: Option<&str>, note: Option<&str>) -> Row {
    row.decision = Some(Decision {
        outcome,
        scope: scope.map(Into::into),
        note: note.map(Into::into),
        elsewhere: false,
    });
    row
}

type RowSet = (&'static str, &'static str, Vec<(Row, RowState)>);

#[allow(clippy::too_many_lines)]
fn row_sets() -> Vec<RowSet> {
    let image = BlobRef {
        hash: vec![1; 32],
        name: "screen.png".into(),
        mime: "image/png".into(),
        size: 48_213,
    };
    let command = |state, exit_code, output: &[&str], more| {
        row(RowKind::Command {
            command: "cargo test -p store".into(),
            state,
            exit_code,
            output_head: output.iter().map(|s| (*s).into()).collect(),
            more_lines: more,
            duration_ms: Some(12_400),
        })
    };
    let question = |multi| QuestionView {
        header: if multi { "Logging" } else { "Rollout" }.into(),
        question: if multi {
            "What should be logged?"
        } else {
            "How should the migration roll out?"
        }
        .into(),
        multi_select: multi,
        options: vec![
            OptionView {
                label: "Behind a flag".into(),
                description: "Off by default for one release".into(),
                preview: String::new(),
                recommended: true,
            },
            OptionView {
                label: "All at once".into(),
                description: String::new(),
                preview: String::new(),
                recommended: false,
            },
        ],
        allow_other: true,
        secret: false,
    };
    let answer = |picked: &[&str], other: Option<&str>| AnswerView {
        picked: picked.iter().map(|s| (*s).into()).collect(),
        other: other.map(Into::into),
        hidden: false,
    };
    let mut run_summary = row(RowKind::Explore {
        verb: ExploreVerb::Read,
        subject: "crates/store/src/lib.rs".into(),
        state: ToolStateView::Succeeded,
    });
    run_summary.run = Some(RunInfo {
        newest: "k".into(),
        oldest: "k0".into(),
        reads: 6,
        searches: 2,
        len: 8,
        anchor: "crates/store/src/lib.rs".into(),
        is_summary: true,
        open_below: false,
    });
    let mut attention = row(RowKind::Prose {
        text: text("Waiting on you above."),
        streaming: false,
        working_note: false,
    });
    attention.attention = true;
    vec![
        (
            "row_prompt",
            "A prompt with an image chip and a review chip at their places, and a steered prompt.",
            vec![
                (
                    row(RowKind::Prompt {
                        text: vec![
                            Segment::Text("Why does the fleet flicker? See ".into()),
                            Segment::Attachment(AttachmentView::Image(image.clone())),
                            Segment::Text(" and my notes ".into()),
                            Segment::Attachment(AttachmentView::Review {
                                patch: None,
                                comments: 2,
                            }),
                        ],
                        steered: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prompt {
                        text: text("Also check the relay path."),
                        steered: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_prose",
            "A finished reply with markdown, a reply still streaming, a working note set apart from the answer, a focused row and a row flagged for attention.",
            vec![
                (
                    row(RowKind::Prose {
                        text: text(
                            "The flicker comes from **two redraws** per tick:\n\n- the fleet repaints on every inventory event\n- the header repaints on the clock\n\nRun `just test-tui` after the fix.",
                        ),
                        streaming: false,
                        working_note: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("Checking the redraw path now"),
                        streaming: true,
                        working_note: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("I'll read the fleet module first."),
                        streaming: false,
                        working_note: true,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Prose {
                        text: text("A focused reply."),
                        streaming: false,
                        working_note: false,
                    }),
                    FOCUSED,
                ),
                (attention, CLOSED),
            ],
        ),
        (
            "row_thinking",
            "Thinking closed with its duration, then opened.",
            vec![
                (
                    row(RowKind::Thinking {
                        text: "The redraw happens twice because the tick and the event both mark the screen dirty.".into(),
                        open: false,
                        duration_ms: Some(4_200),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Thinking {
                        text: "The redraw happens twice because the tick and the event both mark the screen dirty.".into(),
                        open: false,
                        duration_ms: Some(4_200),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_tool_call",
            "A tool-server call running, one allowed for the session with its decision on the row, one denied with a note, and one failed, opened to its result.",
            vec![
                (
                    row(RowKind::ToolCall {
                        server: "github".into(),
                        tool: "create_issue".into(),
                        fact: "jlw/amux".into(),
                        state: ToolStateView::Running,
                        result: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::ToolCall {
                            server: String::new(),
                            tool: "WebFetch".into(),
                            fact: "docs.rs/ratatui".into(),
                            state: ToolStateView::Succeeded,
                            result: String::new(),
                        }),
                        DecisionView::Allowed,
                        Some("for this session"),
                        None,
                    ),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::ToolCall {
                            server: "linear".into(),
                            tool: "delete_issue".into(),
                            fact: "FOX-12".into(),
                            state: ToolStateView::Denied,
                            result: String::new(),
                        }),
                        DecisionView::Denied,
                        None,
                        Some("never delete issues"),
                    ),
                    CLOSED,
                ),
                (
                    row(RowKind::ToolCall {
                        server: "github".into(),
                        tool: "get_pr".into(),
                        fact: "#412".into(),
                        state: ToolStateView::Failed,
                        result: "404 Not Found".into(),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_file_change",
            "An edit across two files with counts, a created file, a deletion and a move, one auto-approved.",
            vec![
                (
                    row(RowKind::FileChange {
                        files: vec![
                            FileRow {
                                path: "crates/tui/src/fleet.rs".into(),
                                change: FileChangeView::Edited,
                                added: 12,
                                removed: 3,
                            },
                            FileRow {
                                path: "crates/tui/src/app.rs".into(),
                                change: FileChangeView::Edited,
                                added: 1,
                                removed: 1,
                            },
                        ],
                        state: ToolStateView::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    decided(
                        row(RowKind::FileChange {
                            files: vec![FileRow {
                                path: "NOTES.md".into(),
                                change: FileChangeView::Created { lines: 14 },
                                added: 14,
                                removed: 0,
                            }],
                            state: ToolStateView::Succeeded,
                        }),
                        DecisionView::AutoApproved,
                        None,
                        None,
                    ),
                    CLOSED,
                ),
                (
                    row(RowKind::FileChange {
                        files: vec![
                            FileRow {
                                path: "old.txt".into(),
                                change: FileChangeView::Deleted,
                                added: 0,
                                removed: 9,
                            },
                            FileRow {
                                path: "a.rs".into(),
                                change: FileChangeView::Moved { to: "b.rs".into() },
                                added: 0,
                                removed: 0,
                            },
                        ],
                        state: ToolStateView::Pending,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_command",
            "A command running, one succeeded with its output head, one failed with its exit code, opened.",
            vec![
                (command(ToolStateView::Running, None, &[], 0), CLOSED),
                (
                    command(
                        ToolStateView::Succeeded,
                        Some(0),
                        &["running 42 tests", "test result: ok. 42 passed"],
                        0,
                    ),
                    CLOSED,
                ),
                (
                    command(
                        ToolStateView::Failed,
                        Some(101),
                        &[
                            "running 42 tests",
                            "test reopen ... FAILED",
                            "failures: reopen",
                        ],
                        12,
                    ),
                    OPEN,
                ),
            ],
        ),
        (
            "row_explore",
            "Exploration steps: a read, a search, a listing, a fetch and a web search, then a collapsed run's summary with its counts.",
            vec![
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Read,
                        subject: "crates/tui/src/fleet.rs".into(),
                        state: ToolStateView::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Search,
                        subject: "redraw".into(),
                        state: ToolStateView::Running,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::List,
                        subject: "crates/".into(),
                        state: ToolStateView::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::Fetch,
                        subject: "https://docs.rs/ratatui".into(),
                        state: ToolStateView::Succeeded,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Explore {
                        verb: ExploreVerb::WebSearch,
                        subject: "ratatui flicker".into(),
                        state: ToolStateView::Succeeded,
                    }),
                    CLOSED,
                ),
                (run_summary, CLOSED),
            ],
        ),
        (
            "row_subagent",
            "A subagent at work with its last tool, then one finished with its answer, opened.",
            vec![
                (
                    row(RowKind::Subagent {
                        description: "Survey the redraw paths".into(),
                        running: true,
                        tool_count: 7,
                        last_tool: "Read crates/tui/src/app.rs".into(),
                        answer: String::new(),
                        duration_ms: None,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Subagent {
                        description: "Survey the redraw paths".into(),
                        running: false,
                        tool_count: 12,
                        last_tool: String::new(),
                        answer: "Two paths redraw: the tick and inventory events.".into(),
                        duration_ms: Some(95_000),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_background",
            "A background command still running and one that ended.",
            vec![
                (
                    row(RowKind::Background {
                        command: "npm run dev".into(),
                        running: true,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Background {
                        command: "cargo watch".into(),
                        running: false,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_image",
            "An image the agent read and one it generated.",
            vec![
                (
                    row(RowKind::Image {
                        image: Some(image.clone()),
                        path: "docs/screen.png".into(),
                        generated: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Image {
                        image: None,
                        path: "out/logo.png".into(),
                        generated: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_slash_output",
            "A slash command's output, closed and opened.",
            vec![
                (
                    row(RowKind::SlashOutput {
                        command: "/cost".into(),
                        args: String::new(),
                        output: "Total cost: $0.42\nTotal duration: 3m 12s".into(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::SlashOutput {
                        command: "/cost".into(),
                        args: String::new(),
                        output: "Total cost: $0.42\nTotal duration: 3m 12s".into(),
                    }),
                    OPEN,
                ),
            ],
        ),
        (
            "row_ask",
            "Asks that became rows: a question answered, several questions answered with a note, a plan approved and one sent back, a form sent, a link declined, access granted for the turn, an open question, and a dialog this build couldn't read, closed and opened.",
            vec![
                (
                    row(RowKind::Ask(AskRow::Question {
                        questions: vec![question(false)],
                        answers: vec![answer(&["Behind a flag"], None)],
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Questions {
                        questions: vec![question(false), question(true)],
                        answers: vec![
                            answer(&["All at once"], None),
                            answer(&[], Some("Only failures, with the host id")),
                        ],
                        note: Some("keep the old strings for one release".into()),
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Plan {
                        plan: "Collapse the pairing failures into one error.".into(),
                        verdict: PlanVerdict::Approved,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Plan {
                        plan: "Rename every wire code.".into(),
                        verdict: PlanVerdict::SentBack,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Form {
                        server: "github".into(),
                        message: "Create the issue in which repository?".into(),
                        fields: vec!["repository".into(), "labels".into(), "assign".into()],
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Link {
                        server: "linear".into(),
                        message: "Sign in to Linear".into(),
                        url: "https://linear.app/oauth".into(),
                        resolution: Resolution::Declined,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Grant {
                        reason: "Write the build output".into(),
                        read: vec![],
                        write: vec!["~/src/amux/target".into()],
                        network: false,
                        hosts: vec![],
                        granted: Some(Granted {
                            read: vec![],
                            write: vec!["~/src/amux/target".into()],
                            network: false,
                            for_session: false,
                        }),
                        resolution: Resolution::Answered,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Question {
                        questions: vec![question(false)],
                        answers: vec![],
                        resolution: Resolution::Open,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Unanswerable {
                        reason: UNANSWERABLE.into(),
                        resolution: Resolution::Dismissed,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Unanswerable {
                        reason: UNANSWERABLE.into(),
                        resolution: Resolution::Cancelled,
                    })),
                    OPEN,
                ),
            ],
        ),
        (
            "row_turn_end",
            "The quiet footer under a finished turn, with cost, and a failed turn.",
            vec![
                (
                    row(RowKind::TurnEnd {
                        duration_ms: Some(102_000),
                        cost_usd: Some(0.42),
                        failed: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::TurnEnd {
                        duration_ms: Some(3_000),
                        cost_usd: None,
                        failed: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        ("row_stopped", "You stopped it.", vec![(row(RowKind::Stopped), CLOSED)]),
        (
            "row_compaction",
            "Compaction with its counts, and an automatic one without.",
            vec![
                (
                    row(RowKind::Compaction {
                        tokens_before: Some(148_000),
                        tokens_after: Some(22_000),
                        automatic: false,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Compaction {
                        tokens_before: None,
                        tokens_after: None,
                        automatic: true,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_error",
            "An error retried and given up on, and one still being retried.",
            vec![
                (
                    row(RowKind::Error {
                        error_kind: "overloaded".into(),
                        message: "The API is overloaded; try again shortly.".into(),
                        attempts: 10,
                        gave_up: true,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Error {
                        error_kind: "rate_limit".into(),
                        message: "Rate limited.".into(),
                        attempts: 2,
                        gave_up: false,
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_model_switch",
            "A model reroute with its reason.",
            vec![(
                row(RowKind::ModelSwitch {
                    from: "gpt-5".into(),
                    to: "gpt-5-mini".into(),
                    reason: "usage limit".into(),
                }),
                CLOSED,
            )],
        ),
        (
            "row_boundary",
            "Session boundaries: started, cleared, compacted, resumed, forked, restarted, exited with its cause and the daemon lost.",
            [
                (BoundaryKind::Started, ""),
                (BoundaryKind::Cleared, ""),
                (BoundaryKind::Compacted, ""),
                (BoundaryKind::Resumed, ""),
                (BoundaryKind::Forked, ""),
                (BoundaryKind::Restarted, ""),
                (BoundaryKind::Exited, "exit 1"),
                (BoundaryKind::DaemonLost, ""),
            ]
            .into_iter()
            .map(|(kind, cause)| {
                (
                    row(RowKind::Boundary {
                        kind,
                        cause: cause.into(),
                    }),
                    CLOSED,
                )
            })
            .collect(),
        ),
        (
            "row_agent_message",
            "Messages between agents: one received, a child's finished report, one sent and delivered, and one the recipient refused.",
            vec![
                (
                    row(RowKind::AgentMessage {
                        from: "planner".into(),
                        kind: EnvelopeKind::Message,
                        text: "Take the relay half.".into(),
                        to: String::new(),
                        sent: SendState::Unspecified,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: "worker-2".into(),
                        kind: EnvelopeKind::Finished,
                        text: "3 specs updated".into(),
                        to: String::new(),
                        sent: SendState::Unspecified,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: String::new(),
                        kind: EnvelopeKind::Message,
                        text: "Start on the store.".into(),
                        to: "worker-1".into(),
                        sent: SendState::Sent,
                        rejection: String::new(),
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::AgentMessage {
                        from: String::new(),
                        kind: EnvelopeKind::Message,
                        text: "Stop now.".into(),
                        to: "worker-3".into(),
                        sent: SendState::Rejected,
                        rejection: "exited".into(),
                    }),
                    CLOSED,
                ),
            ],
        ),
        (
            "row_auto_review",
            "An automatic reviewer's verdict with its risk, opened to its rationale.",
            vec![(
                row(RowKind::AutoReview {
                    decision: "approved".into(),
                    risk: "low".into(),
                    rationale: "Reads only; no writes outside the workspace.".into(),
                    subject: "k2".into(),
                }),
                OPEN,
            )],
        ),
        (
            "row_unrecognized",
            "A record the interpreter kept without understanding it.",
            vec![(
                row(RowKind::Unrecognized {
                    what: "system/hook_progress".into(),
                    summary: "a hook reported progress".into(),
                }),
                CLOSED,
            )],
        ),
    ]
}

// --- the activity line -----------------------------------------------------

fn activities() -> Vec<(Activity, Option<&'static str>)> {
    let at = |kind, elapsed_ms| Activity {
        kind,
        since_ms: 0,
        elapsed_ms,
    };
    vec![
        (at(ActivityKind::Working, 3_000), None),
        (at(ActivityKind::Thinking, 12_000), None),
        (
            at(ActivityKind::Running { key: "k".into() }, 64_000),
            Some("cargo test -p store"),
        ),
        (at(ActivityKind::Subagents { count: 3 }, 95_000), None),
        (at(ActivityKind::Compacting, 2_000), None),
        (
            at(
                ActivityKind::Retrying {
                    attempt: 3,
                    max_attempts: 10,
                    retry_at_ms: None,
                },
                7_000,
            ),
            None,
        ),
        (
            at(
                ActivityKind::Retrying {
                    attempt: 2,
                    max_attempts: 0,
                    retry_at_ms: None,
                },
                1_000,
            ),
            None,
        ),
    ]
}

// --- asks ------------------------------------------------------------------

fn choice(outcome: ChoiceOutcome) -> Choice {
    Choice {
        outcome,
        primary: false,
        takes_note: false,
        answer: ui_view::Answer::Claude(wire::ClaudeAnswer::default()),
    }
}

fn card(body: AskBody, mut choices: Vec<Choice>) -> AskCard {
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    AskCard {
        kind: wire::Kind::ClaudeSdk,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body,
        choices,
        state: CardState::Open,
    }
}

type CardSet = (&'static str, &'static str, AskCard, bool);

#[allow(clippy::too_many_lines)]
fn cards() -> Vec<CardSet> {
    let command = || AskBody::Command {
        command: "deploy --check".into(),
        cwd: "/workspace".into(),
        reason: "Verify the deploy before tagging".into(),
        description: "Runs the deploy dry run".into(),
    };
    let command_choices = || {
        vec![
            choice(ChoiceOutcome::AllowOnce),
            choice(ChoiceOutcome::AllowAlways {
                subjects: vec!["deploy --check".into()],
                directories: vec![],
                mode: String::new(),
                scope: Scope::Project,
                label: String::new(),
            }),
            choice(ChoiceOutcome::AllowForSession),
            Choice {
                takes_note: true,
                ..choice(ChoiceOutcome::Deny { stops: false })
            },
            choice(ChoiceOutcome::DenyAndStop),
        ]
    };
    let option = |label: &str, description: &str, preview: &str, recommended| OptionView {
        label: label.into(),
        description: description.into(),
        preview: preview.into(),
        recommended,
    };
    let rollout = QuestionView {
        header: "Rollout".into(),
        question: "How should the migration roll out?".into(),
        multi_select: false,
        options: vec![
            option("Behind a flag", "Off by default for one release", "", true),
            option("All at once", "", "", false),
        ],
        allow_other: true,
        secret: false,
    };
    let platforms = QuestionView {
        header: "Platforms".into(),
        question: "Which platforms ship first?".into(),
        multi_select: true,
        options: vec![
            option("macOS", "", "", false),
            option("Linux", "x86_64 and arm64", "", false),
            option("Windows", "", "", false),
        ],
        allow_other: true,
        secret: false,
    };
    let layout = QuestionView {
        header: "Layout".into(),
        question: "Which layout for the host list?".into(),
        multi_select: false,
        options: vec![
            option(
                "Cards",
                "One card per host",
                "┌──────────────────────┐\n│ Studio        ● live │\n│ ~/src/amux  3 agents │\n└──────────────────────┘",
                false,
            ),
            option(
                "Grouped list",
                "",
                "Studio   ● live   3 agents\nLaptop   ○ away   1 agent",
                false,
            ),
        ],
        allow_other: false,
        secret: false,
    };
    let token = QuestionView {
        header: "Token".into(),
        question: "Paste the deploy token".into(),
        multi_select: false,
        options: vec![],
        allow_other: true,
        secret: true,
    };
    let mut sending = card(command(), command_choices());
    sending.state = CardState::Sending;
    let mut rejected = card(command(), command_choices());
    rejected.state = CardState::Rejected("the ask was already answered".into());
    let mut not_confirmed = card(command(), command_choices());
    not_confirmed.state = CardState::NotConfirmed;
    let mut second = card(command(), command_choices());
    second.position = 1;
    second.count = 3;
    vec![
        (
            "ask_command",
            "A command permission: the command, where it runs and why, with every scope stated as what happens, deny with a note, deny and stop, and Stop in the menu; one of three waiting.",
            second,
            false,
        ),
        (
            "ask_edit",
            "A file edit permission with its counts and diff.",
            card(
                AskBody::Edit {
                    path: "crates/tui/src/fleet.rs".into(),
                    files: 1,
                    added: 2,
                    removed: 1,
                    diff: "@@ -10,3 +10,4 @@\n let a = 1;\n-let b = 2;\n+let b = 3;\n+let c = 4;".into(),
                    reason: String::new(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::AllowForSession),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
        ),
        (
            "ask_tool",
            "A tool-server permission with its arguments.",
            card(
                AskBody::Tool {
                    server: "github".into(),
                    tool: "create_issue".into(),
                    arguments: "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Flicker\"\n}".into(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
        ),
        (
            "ask_codex_command",
            "A Codex command approval: allow similar commands by prefix, a network rule for its hosts, and decline.",
            card(
                AskBody::Command {
                    command: "curl localhost:8080/health".into(),
                    cwd: "/workspace".into(),
                    reason: String::new(),
                    description: String::new(),
                },
                vec![
                    choice(ChoiceOutcome::AllowOnce),
                    choice(ChoiceOutcome::AllowSimilar {
                        prefix: vec!["curl".into()],
                    }),
                    choice(ChoiceOutcome::AllowNetwork {
                        hosts: vec!["localhost".into()],
                    }),
                    choice(ChoiceOutcome::Deny { stops: false }),
                    choice(ChoiceOutcome::DenyAndStop),
                ],
            ),
            false,
        ),
        (
            "ask_question_single",
            "One pick-one question with the recommended option lifted and Something else.",
            card(AskBody::Question(vec![rollout.clone()]), vec![]),
            false,
        ),
        (
            "ask_question_multi",
            "One multi-select question: square boxes before any pick.",
            card(AskBody::Question(vec![platforms.clone()]), vec![]),
            false,
        ),
        (
            "ask_questions_steps",
            "Several questions: the header chips are the steps, the first open.",
            card(
                AskBody::Question(vec![rollout, platforms, token.clone()]),
                vec![],
            ),
            false,
        ),
        (
            "ask_question_previews",
            "A question with previews: the highlighted option's preview above the options.",
            card(AskBody::Question(vec![layout]), vec![]),
            false,
        ),
        (
            "ask_question_secret",
            "A question whose answer is secret: typed characters show as bullets.",
            card(AskBody::Question(vec![token]), vec![]),
            false,
        ),
        (
            "ask_plan",
            "A plan to approve, approve with edits accepted automatically, or send back with a note.",
            card(
                AskBody::Plan {
                    plan: "## Plan\n\n1. Collapse the pairing failures into one error.\n2. Update the three specs.\n3. Keep the wire codes.".into(),
                },
                vec![
                    choice(ChoiceOutcome::ApprovePlan {
                        auto_accept_edits: false,
                    }),
                    choice(ChoiceOutcome::ApprovePlan {
                        auto_accept_edits: true,
                    }),
                    Choice {
                        takes_note: true,
                        ..choice(ChoiceOutcome::SendBack)
                    },
                ],
            ),
            false,
        ),
        (
            "ask_form",
            "A form from a tool server with a text field, a choice and a toggle, to submit or decline.",
            card(
                AskBody::Form {
                    server: "github".into(),
                    message: "Create the issue in which repository?".into(),
                    schema_json: r#"{"type":"object","properties":{"repository":{"type":"string","title":"Repository"},"labels":{"type":"string","enum":["bug","ios","docs"],"title":"Labels"},"assign":{"type":"boolean","title":"Assign to me"}},"required":["repository"]}"#.into(),
                },
                vec![
                    choice(ChoiceOutcome::Submit),
                    choice(ChoiceOutcome::Decline),
                ],
            ),
            false,
        ),
        (
            "ask_link",
            "A tool server asking to open a link.",
            card(
                AskBody::Link {
                    server: "linear".into(),
                    message: "Sign in to Linear".into(),
                    url: "https://linear.app/oauth/authorize".into(),
                },
                vec![
                    choice(ChoiceOutcome::OpenLink),
                    choice(ChoiceOutcome::Decline),
                ],
            ),
            false,
        ),
        (
            "ask_access",
            "An access grant for files and network, for the turn or the session, or deny.",
            card(
                AskBody::Access {
                    reason: "Write the build output and fetch crates".into(),
                    read: vec!["~/.cargo".into()],
                    write: vec!["~/src/amux/target".into()],
                    network: true,
                    hosts: vec!["crates.io".into()],
                },
                vec![
                    choice(ChoiceOutcome::GrantForTurn),
                    choice(ChoiceOutcome::GrantForSession),
                    choice(ChoiceOutcome::Deny { stops: false }),
                ],
            ),
            false,
        ),
        (
            "ask_unanswerable",
            "The escape from an ask this client cannot answer: the reason, Stop, and the agent's own terminal where it has one.",
            card(
                AskBody::Unanswerable {
                    reason: UNANSWERABLE.into(),
                },
                vec![],
            ),
            true,
        ),
        (
            "ask_sending",
            "An answer on its way: the card says so and takes no second answer.",
            sending,
            false,
        ),
        (
            "ask_rejected",
            "An answer the agent refused, with its reason.",
            rejected,
            false,
        ),
        (
            "ask_not_confirmed",
            "An answer that was sent but never confirmed, with resend and discard.",
            not_confirmed,
            false,
        ),
    ]
}

// --- the strip -------------------------------------------------------------

fn strips() -> Vec<(&'static str, &'static str, Strip)> {
    vec![
        (
            "strip_claude_pty",
            "Terminal Claude's session facts: tasks with the current one, context filling up and a background task.",
            Strip {
                tasks: Some(TasksView {
                    done: 2,
                    total: 5,
                    current: "Update the specs".into(),
                }),
                context: Some(ContextView {
                    used_tokens: 164_000,
                    window_tokens: Some(200_000),
                    percent: Some(82),
                    in_strip: true,
                }),
                model: Some("opus".into()),
                effort: None,
                mode: Some("default".into()),
                background: Some(1),
                ..Strip::default()
            },
        ),
        (
            "strip_claude_sdk",
            "Headless Claude's session facts: usage near its limit, a tool server that failed and one needing sign-in, and the account signed out.",
            Strip {
                model: Some("sonnet".into()),
                effort: Some("high".into()),
                mode: Some("plan".into()),
                usage: Some(UsageView {
                    blocked: false,
                    windows: vec![("5-hour".into(), 91.0, Some(3_600_000))],
                    credits: None,
                }),
                failed_servers: vec![
                    ServerView {
                        name: "github".into(),
                        error: "connection refused".into(),
                        needs_auth: false,
                    },
                    ServerView {
                        name: "linear".into(),
                        error: String::new(),
                        needs_auth: true,
                    },
                ],
                sign_in: Some(SignInView {
                    state: SignInState::SignedOut,
                    account: "jo@example.com".into(),
                    message: String::new(),
                }),
                ..Strip::default()
            },
        ),
        (
            "strip_codex",
            "Codex's session facts: context used, usage blocked until a reset with credits left, and what it is working on.",
            Strip {
                context: Some(ContextView {
                    used_tokens: 180_000,
                    window_tokens: Some(200_000),
                    percent: Some(90),
                    in_strip: true,
                }),
                model: Some("gpt-5".into()),
                effort: Some("medium".into()),
                usage: Some(UsageView {
                    blocked: true,
                    windows: vec![("weekly".into(), 100.0, Some(86_400_000))],
                    credits: Some("$4.20".into()),
                }),
                working_on: Some("Update the relay tests".into()),
                ..Strip::default()
            },
        ),
    ]
}

// --- the tray and the composer ---------------------------------------------

fn tray(width: usize, theme: Theme) -> Vec<Line<'static>> {
    let rows = [
        TrayRow::Queued(QueuedRow {
            input_id: vec![1],
            text: text("Then run the relay tests."),
            from_agent: None,
            mine: true,
            steered: false,
            can_withdraw: true,
            can_send_now: true,
        }),
        TrayRow::Outbox(OutboxRow {
            input_id: vec![2],
            text: text("Check the deployment once."),
            state: OutboxState::NotConfirmed,
        }),
        TrayRow::Outbox(OutboxRow {
            input_id: vec![3],
            text: text("Stop the server."),
            state: OutboxState::Rejected("exited".into()),
        }),
    ];
    let selected = 1;
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| row.line(i == selected, width, theme))
        .collect();
    lines.push(Line::default());
    let mut hint = Line::from("  ");
    crate::text::push(&mut hint, rows[selected].hint(), theme.muted(), width);
    lines.push(hint);
    lines
}

fn composers() -> Vec<(&'static str, &'static str, Composer, Away, &'static str)> {
    vec![
        (
            "composer_exited",
            "The exited composer: one Enter resumes the agent with the draft as its first prompt.",
            Composer::Resume,
            Away::Plain,
            "Carry on from the failing test.",
        ),
        (
            "composer_exited_empty",
            "The exited composer before anything is typed.",
            Composer::Resume,
            Away::Plain,
            "",
        ),
        (
            "composer_detached",
            "The composer while the agent's host is away: the draft is kept and sending waits.",
            Composer::Disabled(Waiting::Detached),
            Away::Plain,
            "",
        ),
        (
            "composer_detached_signed_out",
            "The composer while the agent's host is away and this machine is signed out of its account: the cause is this machine's, the draft is kept and sending waits until it signs in.",
            Composer::Disabled(Waiting::Detached),
            Away::SignedOut,
            "",
        ),
    ]
}
