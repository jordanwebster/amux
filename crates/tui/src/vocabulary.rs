//! The chat vocabulary as terminal components, drawn from authored view
//! values rather than from sessions: every row kind, not-confirmed prompts
//! and answers, and the review page, once each. The renderer never sees
//! the provider kind, and ui-view's own goldens prove the projection per
//! kind, so one drawing of each component is the whole terminal claim.
//!
//! The component goldens and the PNG renderer both draw this set.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ui_view::{
    AnswerView, AskRow, AttachmentView, Decision, DecisionView, ExploreVerb, FileChangeView,
    FileRow, Granted, LineKind, OptionView, PatchHead, PatchLine, PlanVerdict, QuestionView,
    Resolution, Row, RowKind, RunInfo, Segment, ToolStateView,
};
use wire::{BlobRef, BoundaryKind, EnvelopeKind, SendState};

use crate::chat::review::ReviewPage;
use crate::chat::rows::{RowFacts, RowState, on_rail, row_lines};
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
        "The fleet at 110 columns as the laptop sees it, framed and titled amux: a terminal Claude, a headless Claude and a Codex on the desk, every one idle, each row with its attention mark, name, kind, host and how it is reached, age, status word and what it is working on; the agent count above, the connection, host count and keys on the status bar.",
    ),
    (
        "fleet_60col",
        "The same fleet at 60 columns: rows keep name, kind, host and age and drop the status word and summary; the status bar keeps the keys that fit.",
    ),
    (
        "fleet_standings",
        "A fleet of every standing at 110 columns: a headless Claude asking for permission leads with its mark and status in accent and the needs-you count above, a terminal Claude working, an expanded family whose one-shot child finished, a Codex whose provider exited with its cause, an idle agent, and an agent on a studio that went offline, its row muted with no current status; the status bar counts the offline host.",
    ),
    (
        "fleet_standings_80col",
        "The same fleet at 80 columns: the host cell narrows and the working-on summary is dropped before the status word.",
    ),
    (
        "hosts",
        "The hosts overlay over that fleet: every trusted host with how it is reached (this machine local, the desk direct, the studio offline), inside the fleet's frame, with esc to close.",
    ),
    (
        "chat_strip",
        "A headless Claude chat on the desk read from the laptop: header with model and mode, the first turn, the composer and its keys.",
    ),
    (
        "ask_escape",
        "A terminal Claude on the desk showing a tool server's sign-in dialog in its own terminal, read from the laptop: the escape card docked where the composer was, its reason sending the person to Claude's own terminal and Stop as the one choice (a remote agent's terminal is not offered here), with the conversation still above it: the prompt, the text that introduced the tool server's call and the call, in the order Claude wrote them.",
    ),
    (
        "running_call",
        "A terminal Claude on the desk mid-call, read from the laptop: the text that introduced the call above it and the call drawn running while it runs, before any result.",
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
        let mut lines = Vec::new();
        // Consecutive tool rows share the rail, as a chat draws them.
        for (i, (row, state)) in rows.iter().enumerate() {
            let tool = on_rail(row);
            let older = i > 0 && on_rail(&rows[i - 1].0);
            let newer = rows.get(i + 1).is_some_and(|(next, _)| on_rail(next));
            let state = RowState {
                rail: tool && (older || newer),
                joined: tool && newer,
                ..*state
            };
            lines.extend(row_lines(row, state, &facts(row), w, theme));
        }
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
    rail: false,
    joined: false,
};
const OPEN: RowState = RowState {
    focused: false,
    expanded: true,
    rail: false,
    joined: false,
};
const FOCUSED: RowState = RowState {
    focused: true,
    expanded: false,
    rail: false,
    joined: false,
};

/// What the layout would look up for an authored row: a collapsed run's
/// newest subjects, and the first edit's landed patch.
fn facts(row: &Row) -> RowFacts {
    let mut facts = RowFacts::default();
    if row.run.as_ref().is_some_and(|run| run.is_summary) {
        facts.run_subjects = vec![
            "crates/store/src/lib.rs".into(),
            "crates/tui/src/app.rs".into(),
        ];
    }
    if let RowKind::FileChange { files, state } = &row.kind
        && *state == ToolStateView::Succeeded
        && files
            .first()
            .is_some_and(|file| file.path == "crates/tui/src/fleet.rs")
    {
        let line = |number, kind, text: &str| PatchLine {
            number: Some(number),
            kind,
            text: text.into(),
        };
        facts.patch = Some(PatchHead {
            lines: vec![
                line(
                    41,
                    LineKind::Context,
                    "    fn redraw(&mut self, frame: &mut Frame) {",
                ),
                line(42, LineKind::Removed, "        self.paint_all(frame);"),
                line(42, LineKind::Added, "        if self.dirty {"),
                line(43, LineKind::Added, "            self.paint_all(frame);"),
                line(44, LineKind::Added, "        }"),
            ],
            more: 10,
        });
    }
    facts
}

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
            output_tail: output.iter().map(|s| (*s).into()).collect(),
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
            "A command waiting for the person's permission, one running, one denied with a note, one succeeded with its output head, one failed with its exit code, opened.",
            vec![
                (
                    Row {
                        attention: true,
                        ..command(ToolStateView::Running, None, &[], 0)
                    },
                    CLOSED,
                ),
                (command(ToolStateView::Running, None, &[], 0), CLOSED),
                (
                    decided(
                        command(ToolStateView::Denied, None, &[], 0),
                        DecisionView::Denied,
                        None,
                        Some("Use cargo clean instead"),
                    ),
                    CLOSED,
                ),
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
                        duration_ms: None,
                    }),
                    CLOSED,
                ),
                (
                    row(RowKind::Background {
                        command: "cargo watch".into(),
                        running: false,
                        duration_ms: Some(240_000),
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
            "Asks that became rows: a question answered with a note, several questions answered with a note, a plan approved and one sent back with its note, a form sent, a link declined, access granted for the turn, an open question, and a dialog this build couldn't read, closed and opened.",
            vec![
                (
                    row(RowKind::Ask(AskRow::Question {
                        questions: vec![question(false)],
                        answers: vec![answer(&["Behind a flag"], None)],
                        note: Some("flip it on for the desk first".into()),
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
                        edits_accepted: false,
                        writing: false,
                        note: None,
                    })),
                    CLOSED,
                ),
                (
                    row(RowKind::Ask(AskRow::Plan {
                        plan: "Rename every wire code.".into(),
                        verdict: PlanVerdict::SentBack,
                        edits_accepted: false,
                        writing: false,
                        note: Some("Don't touch the wire codes yet".into()),
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
                        note: None,
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
