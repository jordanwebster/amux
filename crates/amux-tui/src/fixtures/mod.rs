//! Deterministic named states shared by visual tests and screenshot tools.
//!
//! Fixtures enter through `amux-ui` messages, just like the runtime. They do
//! not construct provider layers or feed entries directly, so captures keep
//! exercising the reducer boundary that production uses.

mod gallery;

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use amux_ui::{
    Agent, AgentId, HostEntry, HostId, HostTrustStatus, Model, Msg, ProfileEntry, ProfileId,
    ServerMsg, StreamEntry, StreamMsg, StructuredProtocol, update,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::chat::{handle_chat_key, handle_chat_mouse};
use crate::switcher::SwitcherState;
use crate::view::Mode;
use crate::{ChatView, FrameContext, Theme, ViewState, render};

const NOW: &str = "2026-08-12T09:12:30Z";
const SESSION: &str = "22222222-2222-4222-8222-222222222222";

/// Every state currently available to deterministic renderers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedState {
    ClaudeIdle,
    ClaudeWorking,
    ClaudePermissionAsk,
    ClaudeQuestionAsk,
    ClaudePlanReader,
    ClaudeDiffReader,
    CodexIdle,
    CodexWorking,
    CodexApproval,
    CodexNetworkPolicy,
    CodexMcpStartup,
    HelpOverlay,
    Fleet,
    FleetEmpty,
    ProfileSwitcher,
    FleetSwitched,
    ClaudeLongFeed,
    CodexLongFeed,
    ClaudeScrolledBack,
    CodexScrolledBack,
    ComponentGallery,
    ComponentGalleryCodex,
    ExplorationCollapsed,
    ExplorationExpanded,
    ChatAttachmentBlocks,
    ChatMixedDraft,
    ReviewOpen,
    ReviewSelection,
    ReviewCommentBox,
    ReviewThreads,
    ReviewFileList,
    ReviewFolded,
    ReviewBranchBase,
    ChatReviewToken,
}

const ALL_STATES: &[NamedState] = &[
    NamedState::ClaudeIdle,
    NamedState::ClaudeWorking,
    NamedState::ClaudePermissionAsk,
    NamedState::ClaudeQuestionAsk,
    NamedState::ClaudePlanReader,
    NamedState::ClaudeDiffReader,
    NamedState::CodexIdle,
    NamedState::CodexWorking,
    NamedState::CodexApproval,
    NamedState::CodexNetworkPolicy,
    NamedState::CodexMcpStartup,
    NamedState::HelpOverlay,
    NamedState::Fleet,
    NamedState::FleetEmpty,
    NamedState::ProfileSwitcher,
    NamedState::FleetSwitched,
    NamedState::ClaudeLongFeed,
    NamedState::CodexLongFeed,
    NamedState::ClaudeScrolledBack,
    NamedState::CodexScrolledBack,
    NamedState::ComponentGallery,
    NamedState::ComponentGalleryCodex,
    NamedState::ExplorationCollapsed,
    NamedState::ExplorationExpanded,
    NamedState::ChatAttachmentBlocks,
    NamedState::ChatMixedDraft,
    NamedState::ReviewOpen,
    NamedState::ReviewSelection,
    NamedState::ReviewCommentBox,
    NamedState::ReviewThreads,
    NamedState::ReviewFileList,
    NamedState::ReviewFolded,
    NamedState::ReviewBranchBase,
    NamedState::ChatReviewToken,
];

impl NamedState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeIdle => "claude-idle",
            Self::ClaudeWorking => "claude-working",
            Self::ClaudePermissionAsk => "claude-permission-ask",
            Self::ClaudeQuestionAsk => "claude-question-ask",
            Self::ClaudePlanReader => "claude-plan-reader",
            Self::ClaudeDiffReader => "claude-diff-reader",
            Self::CodexIdle => "codex-idle",
            Self::CodexWorking => "codex-working",
            Self::CodexApproval => "codex-approval",
            Self::CodexNetworkPolicy => "codex-network-policy",
            Self::CodexMcpStartup => "codex-mcp-startup",
            Self::HelpOverlay => "help-overlay",
            Self::Fleet => "fleet",
            Self::FleetEmpty => "fleet-empty",
            Self::ProfileSwitcher => "profile-switcher",
            Self::FleetSwitched => "fleet-switched",
            Self::ClaudeLongFeed => "claude-long-feed",
            Self::CodexLongFeed => "codex-long-feed",
            Self::ClaudeScrolledBack => "claude-scrolled-back",
            Self::CodexScrolledBack => "codex-scrolled-back",
            Self::ComponentGallery => "component-gallery",
            Self::ComponentGalleryCodex => "component-gallery-codex",
            Self::ExplorationCollapsed => "exploration-collapsed",
            Self::ExplorationExpanded => "exploration-expanded",
            Self::ChatAttachmentBlocks => "chat-attachment-blocks",
            Self::ChatMixedDraft => "chat-mixed-draft",
            Self::ReviewOpen => "review-open",
            Self::ReviewSelection => "review-selection",
            Self::ReviewCommentBox => "review-comment-box",
            Self::ReviewThreads => "review-threads",
            Self::ReviewFileList => "review-file-list",
            Self::ReviewFolded => "review-folded",
            Self::ReviewBranchBase => "review-branch-base",
            Self::ChatReviewToken => "chat-review-token",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        all_states()
            .iter()
            .copied()
            .find(|state| state.name() == name)
    }
}

impl fmt::Display for NamedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Error returned when a screenshot subject is not in the named registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownNamedState(pub String);

impl fmt::Display for UnknownNamedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown TUI state `{}`", self.0)
    }
}

impl std::error::Error for UnknownNamedState {}

impl FromStr for NamedState {
    type Err = UnknownNamedState;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name).ok_or_else(|| UnknownNamedState(name.to_string()))
    }
}

/// Complete inputs for one deterministic call to the pure renderer.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub model: Model,
    pub view: ViewState,
    pub now: DateTime<Utc>,
}

/// The ordered registry used by screenshot listing and exhaustive tests.
pub const fn all_states() -> &'static [NamedState] {
    ALL_STATES
}

/// Build one named state at the registry's fixed clock.
pub fn fixture(state: NamedState) -> Fixture {
    match state {
        NamedState::ClaudeIdle => claude_fixture(claude_idle_rows()),
        NamedState::ClaudeWorking => claude_fixture(claude_working_rows()),
        NamedState::ClaudePermissionAsk => {
            let mut rows = claude_working_rows();
            rows.push(permission_hook());
            claude_fixture(rows)
        }
        NamedState::ClaudeQuestionAsk => {
            let mut rows = claude_working_rows();
            rows.push(question_hook());
            claude_fixture(rows)
        }
        NamedState::ClaudePlanReader => {
            let mut rows = claude_working_rows();
            rows.push(plan_hook());
            claude_fixture(rows)
        }
        NamedState::ClaudeDiffReader => {
            let mut rows = claude_working_rows();
            rows.push(permission_hook());
            let mut fixture = claude_fixture(rows);
            let chat = fixture.view.chat.as_mut().expect("Claude chat open");
            handle_chat_key(
                chat,
                &fixture.model,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
                (120, 40),
                fixed_now(),
            );
            fixture
        }
        NamedState::CodexIdle => codex_fixture(codex_idle_rows()),
        NamedState::CodexWorking => codex_fixture(codex_working_rows()),
        NamedState::CodexApproval => codex_fixture(codex_approval_rows(false)),
        NamedState::CodexNetworkPolicy => codex_fixture(codex_approval_rows(true)),
        NamedState::CodexMcpStartup => codex_fixture(codex_mcp_rows()),
        NamedState::HelpOverlay => {
            let mut fixture = claude_fixture(claude_idle_rows());
            fixture
                .view
                .chat
                .as_mut()
                .expect("Claude chat open")
                .set_help(true);
            fixture
        }
        NamedState::Fleet => fleet_fixture(false),
        NamedState::FleetEmpty => fleet_fixture(true),
        NamedState::ProfileSwitcher => profile_switcher_fixture(),
        NamedState::FleetSwitched => switched_fleet_fixture(),
        NamedState::ClaudeLongFeed => long_feed(StructuredProtocol::Claude, 1_000),
        NamedState::CodexLongFeed => long_feed(StructuredProtocol::Codex, 1_000),
        NamedState::ClaudeScrolledBack => scrolled_back(StructuredProtocol::Claude),
        NamedState::CodexScrolledBack => scrolled_back(StructuredProtocol::Codex),
        NamedState::ComponentGallery => claude_fixture(gallery::gallery_rows()),
        NamedState::ComponentGalleryCodex => codex_fixture(gallery::codex_gallery_rows()),
        // Both halves of the pair are one transcript. Collapsed is what a
        // run looks like on arrival; expanded is the same screen with the
        // first run present in the feed viewport's expansion set.
        NamedState::ExplorationCollapsed => claude_fixture(gallery::exploration_rows()),
        NamedState::ExplorationExpanded => {
            let mut fixture = claude_fixture(gallery::exploration_rows());
            let chat = fixture.view.chat.as_mut().expect("Claude chat open");
            let run = fixture
                .model
                .claude(chat.agent)
                .expect("Claude fixture has a layer")
                .feed_items()
                .into_iter()
                .find_map(|item| match item {
                    amux_ui::claude::FeedItem::ExplorationRun { id, .. } => {
                        Some(crate::chat::blocks::RunKey(id))
                    }
                    amux_ui::claude::FeedItem::Entry(_) => None,
                })
                .expect("exploration fixture has a folded run");
            chat.viewport.expanded.insert(run);
            fixture
        }
        NamedState::ChatAttachmentBlocks => claude_fixture(claude_attachment_rows()),
        NamedState::ChatMixedDraft => {
            let mut fixture = claude_fixture(claude_idle_rows());
            let chat = fixture.view.chat.as_mut().expect("Claude chat open");
            let composer = chat.composer_mut();
            composer.insert_str("compare ");
            composer.attach(fixture_image());
            composer.insert_str(" against the trace ");
            composer.paste_or_attach(&pasted_body(240));
            fixture
        }
        NamedState::ReviewOpen => review_fixture(review_working_tree(), &[]),
        NamedState::ReviewSelection => review_fixture(review_working_tree(), &REVIEW_SELECT),
        NamedState::ReviewCommentBox => {
            let mut fixture = review_fixture(review_working_tree(), &REVIEW_SELECT);
            review_keys(&mut fixture, &[ReviewKey::Char('c')]);
            review_type(
                &mut fixture,
                "the old name is public; keep a re-export for one release",
            );
            fixture
        }
        NamedState::ReviewThreads => {
            let mut fixture = commented_review();
            // Back to the top, then `n` to the first row anybody wrote on.
            review_keys(&mut fixture, &[ReviewKey::Char('g'), ReviewKey::Char('n')]);
            fixture
        }
        NamedState::ReviewFileList => {
            let mut fixture = commented_review();
            review_keys(
                &mut fixture,
                &[
                    ReviewKey::Char('g'),
                    ReviewKey::Char('f'),
                    ReviewKey::Char('j'),
                ],
            );
            fixture
        }
        NamedState::ReviewFolded => {
            let mut fixture = review_fixture(review_working_tree(), &[]);
            review_keys(&mut fixture, &[ReviewKey::Char('z')]);
            fixture
        }
        NamedState::ReviewBranchBase => review_fixture(
            amux_ui::DiffBase::Branch {
                base: "main".to_string(),
            },
            &[],
        ),
        NamedState::ChatReviewToken => {
            let mut fixture = review_fixture(review_working_tree(), &[]);
            for text in ["say why the store had to go", "give it a doc comment"] {
                review_keys(&mut fixture, &[ReviewKey::Char('j'), ReviewKey::Char('c')]);
                review_type(&mut fixture, text);
                review_keys(&mut fixture, &[ReviewKey::Enter]);
            }
            // Back to the draft, with something typed after the token: the
            // message a review actually rides in.
            review_keys(&mut fixture, &[ReviewKey::Char('q')]);
            review_type(&mut fixture, " \u{2014} two things before this lands");
            fixture
        }
    }
}

// --- the review page --------------------------------------------------------

/// A key the scripted review states press. Naming them rather than
/// building `KeyEvent`s inline keeps a state's script readable as the
/// sequence a person would type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewKey {
    Char(char),
    Enter,
}

impl ReviewKey {
    fn event(self) -> KeyEvent {
        match self {
            Self::Char(character) => KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            Self::Enter => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        }
    }
}

/// Down two rows, then a selection running over the removed row and the
/// added one under it.
const REVIEW_SELECT: [ReviewKey; 4] = [
    ReviewKey::Char('j'),
    ReviewKey::Char('j'),
    ReviewKey::Char('v'),
    ReviewKey::Char('j'),
];

fn review_working_tree() -> amux_ui::DiffBase {
    amux_ui::DiffBase::WorkingTree
}

/// The chat with `<leader> r` pressed, the daemon's frozen diff delivered,
/// and `script` typed into the page that opened over it.
///
/// Every state below reaches the page the way a person does — through the
/// chord, the reducer, and the chat's own key handler — so a capture can
/// never show a page the running program cannot produce.
fn review_fixture(base: amux_ui::DiffBase, script: &[ReviewKey]) -> Fixture {
    let mut fixture = recording_start();
    press_leader(&mut fixture, 'r');
    deliver_frozen_diff(&mut fixture, base);
    review_keys(&mut fixture, script);
    fixture
}

/// The page after three comments have been written on it, the way the
/// threads, file-list and token captures need it.
///
/// Each comment is walked to from where the last one was saved: `j` steps
/// rows and `]` steps files, so the script reads as the trip a person
/// takes rather than as a set of coordinates.
fn commented_review() -> Fixture {
    let mut fixture = review_fixture(review_working_tree(), &[]);
    for (walk, text) in [
        (
            // A file opens on its first real row rather than on the break
            // above it, so every walk here is one step shorter than the
            // count of rows it passes.
            &[ReviewKey::Char('j'), ReviewKey::Char('j')][..],
            "Say why the store had to go.",
        ),
        (
            &[ReviewKey::Char(']'), ReviewKey::Char(']')][..],
            "Name the crate this belongs to.",
        ),
        (
            &[ReviewKey::Char('j'), ReviewKey::Char('j')][..],
            "Give it a doc comment.",
        ),
    ] {
        review_keys(&mut fixture, walk);
        review_keys(&mut fixture, &[ReviewKey::Char('c')]);
        review_type(&mut fixture, text);
        review_keys(&mut fixture, &[ReviewKey::Enter]);
    }
    fixture
}

// --- scripted recordings ----------------------------------------------------

/// One scripted input for a recording: what a person does, not what a
/// terminal happens to send.
///
/// Everything here runs through the same entry points the running program
/// uses, so a recording can never show a screen the program cannot reach.
#[derive(Clone, Debug)]
pub enum ScriptStep {
    /// One key, exactly as the terminal delivers it.
    Key(KeyEvent),
    /// A run of characters typed one key at a time.
    Type(String),
    /// A bracketed paste.
    Paste(String),
    /// Ctrl+V with a stated clipboard, so a recording does not depend on
    /// what the recording machine happens to be holding.
    Clipboard(crate::clipboard::ClipboardContent),
    /// The daemon answers the review chord with the fixture's frozen diff.
    FrozenDiff(amux_ui::DiffBase),
    /// The conversation under the chat becomes another named state's: how
    /// a recording shows an ask taking the surface over and handing it
    /// back, without inventing a transcript of its own.
    Conversation(NamedState),
}

impl ScriptStep {
    /// What this step is, for the recording's event log.
    pub fn label(&self) -> String {
        match self {
            Self::Key(key) => key_label(*key),
            Self::Type(text) => format!("type {text:?}"),
            Self::Paste(text) => format!("paste {} lines", text.lines().count()),
            Self::Clipboard(content) => format!("ctrl+v {}", clipboard_label(content)),
            Self::FrozenDiff(base) => match base {
                amux_ui::DiffBase::WorkingTree => "diff ready (working tree)".to_string(),
                amux_ui::DiffBase::Branch { base } => format!("diff ready (branch {base})"),
            },
            Self::Conversation(state) => format!("conversation becomes {}", state.name()),
        }
    }
}

fn key_label(key: KeyEvent) -> String {
    let mut label = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        label.push_str("ctrl+");
    }
    match key.code {
        KeyCode::Char(character) => label.push(character),
        KeyCode::Enter => label.push_str("enter"),
        KeyCode::Backspace => label.push_str("backspace"),
        KeyCode::Left => label.push_str("left"),
        KeyCode::Right => label.push_str("right"),
        other => label.push_str(&format!("{other:?}")),
    }
    label
}

fn clipboard_label(content: &crate::clipboard::ClipboardContent) -> String {
    match content {
        crate::clipboard::ClipboardContent::Image { mime, bytes } => {
            format!("{mime} ({} bytes)", bytes.len())
        }
        crate::clipboard::ClipboardContent::Path(path) => path.display().to_string(),
        crate::clipboard::ClipboardContent::Text(text) => format!("{} chars", text.chars().count()),
        crate::clipboard::ClipboardContent::Empty => "an empty clipboard".to_string(),
    }
}

/// Apply one scripted step to a fixture, through the production handlers.
pub fn apply_step(fixture: &mut Fixture, step: &ScriptStep) {
    match step {
        ScriptStep::Key(key) => press(fixture, *key),
        ScriptStep::Type(text) => review_type(fixture, text),
        ScriptStep::Paste(text) => {
            let chat = fixture.view.chat.as_mut().expect("chat open");
            crate::chat::handle_chat_paste(chat, &fixture.model, text);
        }
        ScriptStep::Clipboard(content) => {
            let chat = fixture.view.chat.as_mut().expect("chat open");
            crate::chat::handle_chat_clipboard(chat, &fixture.model, content.clone());
        }
        ScriptStep::FrozenDiff(base) => deliver_frozen_diff(fixture, base.clone()),
        ScriptStep::Conversation(state) => {
            fixture.model = fixture_model(*state);
            fixture
                .view
                .chat
                .as_mut()
                .expect("chat open")
                .reconcile(&fixture.model);
        }
    }
}

/// The chat a recording starts from: an idle Claude conversation with an
/// empty draft.
pub fn recording_start() -> Fixture {
    claude_fixture(claude_idle_rows())
}

/// The daemon's answer to the review chord, delivered the way the runtime
/// delivers it: the dispatched command, then its outcome, then a reconcile.
fn deliver_frozen_diff(fixture: &mut Fixture, base: amux_ui::DiffBase) {
    let agent = agent_id(StructuredProtocol::Claude);
    let command = amux_ui::Command::RequestDiff {
        agent,
        base: base.clone(),
    };
    deliver_diff_response(
        fixture,
        command,
        crate::review::fixture::sample_diff_response(base),
    );
}

/// Deliver a caller-supplied diff through the same command/outcome/reconcile
/// boundary as the runtime. Integration tests use this to freeze a real
/// repository patch without teaching the fixture layer how to run git.
pub fn deliver_diff_response(
    fixture: &mut Fixture,
    command: amux_ui::Command,
    response: amux_ui::DiffResponse,
) {
    let op = amux_ui::OpId(uuid::Uuid::from_u128(0x5eed_0000_0000_0004));
    fixture
        .view
        .chat
        .as_mut()
        .expect("chat open")
        .note_dispatched(op, &command);
    update(&mut fixture.model, Msg::Command { op, command });
    update(
        &mut fixture.model,
        Msg::OpResult {
            op,
            outcome: amux_ui::OpOutcome::DiffReady { response },
        },
    );
    fixture
        .view
        .chat
        .as_mut()
        .expect("chat open")
        .reconcile(&fixture.model);
}

/// The model one named state stands on, for a recording that swaps the
/// conversation under a draft.
fn fixture_model(state: NamedState) -> Model {
    fixture(state).model
}

fn press_leader(fixture: &mut Fixture, code: char) {
    for key in [
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE),
    ] {
        press(fixture, key);
    }
}

/// Apply one scripted key through the chat's own handler.
pub fn review_keys(fixture: &mut Fixture, script: &[ReviewKey]) {
    for key in script {
        press(fixture, key.event());
    }
}

fn review_type(fixture: &mut Fixture, text: &str) {
    for character in text.chars() {
        press(
            fixture,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
        );
    }
}

fn press(fixture: &mut Fixture, key: KeyEvent) {
    let chat = fixture.view.chat.as_mut().expect("Claude chat open");
    handle_chat_key(chat, &fixture.model, key, (120, 40), fixture.now);
}

// --- attachments ------------------------------------------------------------

fn fixture_image() -> amux_ui::DraftAttachment {
    amux_ui::DraftAttachment::from_bytes(
        amux_ui::ArtifactKind::Image,
        "screenshot.png",
        "image/png",
        vec![b'p'; 120_433],
    )
}

fn fixture_file() -> amux_ui::DraftAttachment {
    amux_ui::DraftAttachment::from_bytes(
        amux_ui::ArtifactKind::File,
        "trace.log",
        "text/plain",
        vec![b'x'; 4096],
    )
}

fn fixture_report() -> amux_ui::DraftAttachment {
    amux_ui::DraftAttachment::from_bytes(
        amux_ui::ArtifactKind::File,
        "coverage.html",
        "text/html",
        vec![b'y'; 20_000],
    )
}

fn pasted_body(lines: usize) -> String {
    (1..=lines).map(|n| format!("stack frame {n}\n")).collect()
}

fn attachment_element(attachment: &amux_ui::DraftAttachment) -> String {
    let kind = match attachment.kind {
        amux_ui::ArtifactKind::Image => amux_ui::MentionKind::Image {
            id: attachment.id.clone(),
        },
        _ => amux_ui::MentionKind::File {
            id: attachment.id.clone(),
        },
    };
    amux_ui::format_mention(&amux_ui::Mention {
        kind,
        name: attachment.name.clone(),
        size: Some(attachment.size),
        path: None,
    })
}

/// The synthetic row the daemon emits before delivering an input that
/// pins artifacts: it is what lets a host that never saw the bytes paint
/// a name and a size.
fn attachment_refs_row(attachments: &[&amux_ui::DraftAttachment]) -> Value {
    json!({
        "type": "amux.attachments",
        "input_id": null,
        "refs": attachments
            .iter()
            .map(|attachment| json!({
                "id": attachment.id,
                "kind": match attachment.kind {
                    amux_ui::ArtifactKind::Image => "image",
                    amux_ui::ArtifactKind::Diff => "diff",
                    _ => "file",
                },
                "name": attachment.name,
                "mime": attachment.mime,
                "size": attachment.size,
            }))
            .collect::<Vec<_>>(),
    })
}

fn claude_attachment_rows() -> Vec<Value> {
    let image = fixture_image();
    let file = fixture_file();
    let report = fixture_report();
    let pasted = amux_ui::format_mention(&amux_ui::Mention {
        kind: amux_ui::MentionKind::Text {
            lines: 240,
            body: pasted_body(240),
        },
        name: "pasted-1".into(),
        size: None,
        path: None,
    });
    vec![
        claude_ready(),
        json!({"type": "permission-mode", "permissionMode": "default"}),
        attachment_refs_row(&[&image, &file]),
        claude_prompt(
            1,
            &format!(
                "The sync panel drops rows — here is the screen, the log, and the stack I pasted.\n{}\n{}\n{}",
                attachment_element(&image),
                attachment_element(&file),
                pasted,
            ),
        ),
        attachment_refs_row(&[&report]),
        claude_assistant(
            2,
            &format!(
                "It drops rows above 4k. The coverage run is next to it:\n{}",
                attachment_element(&report),
            ),
            Some("end_turn"),
        ),
        json!({
            "type": "system",
            "subtype": "turn_duration",
            "uuid": "ffffffff-0000-4000-8000-000000000004",
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:06Z",
            "durationMs": 6200,
        }),
    ]
}

/// Build a retained feed of exactly `entries` provider-native items.
pub fn long_feed(protocol: StructuredProtocol, entries: usize) -> Fixture {
    let mut rows = Vec::with_capacity(entries + 1);
    rows.push(match protocol {
        StructuredProtocol::Claude => claude_ready(),
        StructuredProtocol::Codex => codex_ready(),
        StructuredProtocol::ClaudeSdk => {
            unreachable!("an SDK-driven chat has no native feed to fixture")
        }
    });
    match protocol {
        StructuredProtocol::Claude => rows.extend((0..entries).map(claude_long_row)),
        StructuredProtocol::Codex => rows.extend((0..entries).map(codex_long_row)),
        StructuredProtocol::ClaudeSdk => {
            unreachable!("an SDK-driven chat has no native feed to fixture")
        }
    }
    match protocol {
        StructuredProtocol::Claude => claude_fixture(rows),
        StructuredProtocol::Codex => codex_fixture(rows),
        StructuredProtocol::ClaudeSdk => {
            unreachable!("an SDK-driven chat has no native feed to fixture")
        }
    }
}

fn scrolled_back(protocol: StructuredProtocol) -> Fixture {
    let mut fixture = long_feed(protocol, 1_000);
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("fixture terminal");
    let context = FrameContext {
        viewport: (120, 40),
        theme: Theme::default(),
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .expect("warm long-feed frame");
    let chat = fixture.view.chat.as_mut().expect("structured chat open");
    let wheel_up = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 4,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    for event_index in 0..4 {
        assert!(
            handle_chat_mouse(chat, &fixture.model, wheel_up, (120, 40)),
            "{protocol:?} long feed can scroll another three rows at wheel event {event_index}"
        );
    }
    fixture
}

fn fixed_now() -> DateTime<Utc> {
    at(NOW)
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn host_id() -> HostId {
    HostId::from_u128(1)
}

fn agent_id(protocol: StructuredProtocol) -> AgentId {
    match protocol {
        StructuredProtocol::Claude => AgentId::from_u128(7),
        StructuredProtocol::ClaudeSdk => AgentId::from_u128(9),
        StructuredProtocol::Codex => AgentId::from_u128(8),
    }
}

fn agent(protocol: StructuredProtocol, name: &str) -> Agent {
    let (command, kind) = match protocol {
        StructuredProtocol::Claude => (
            "claude",
            amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
        ),
        StructuredProtocol::ClaudeSdk => (
            "claude",
            amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Sdk,
            },
        ),
        StructuredProtocol::Codex => ("codex", amux_ui::AgentKind::Codex),
    };
    Agent {
        id: agent_id(protocol),
        host_id: host_id(),
        name: Some(name.to_string()),
        command: command.to_string(),
        working_dir: "/work/amux".into(),
        kind,
        readonly: false,
        args: Vec::new(),
        created_at: at("2026-08-12T09:00:00Z"),
        parent: None,
        working_on: None,
    }
}

fn host() -> HostEntry {
    HostEntry {
        id: host_id(),
        name: "mbp".to_string(),
        online: true,
        version: Some("0.4.0".to_string()),
        capabilities: Some(amux_ui::Capabilities::default()),
        trust_status: HostTrustStatus::Trusted,
        last_dial_error: None,
    }
}

fn base_messages(protocol: StructuredProtocol, name: &str) -> Vec<Msg> {
    vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host: host() }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: agent(protocol, name),
        }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: agent_id(protocol),
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: agent_id(protocol),
            event: StreamMsg::ReplayComplete,
        },
    ]
}

fn model(protocol: StructuredProtocol, name: &str, rows: Vec<Value>) -> Model {
    let mut messages = base_messages(protocol, name);
    messages.push(Msg::Stream {
        agent: agent_id(protocol),
        event: StreamMsg::Batch {
            at: at("2026-08-12T09:12:20Z"),
            entries: rows
                .into_iter()
                .enumerate()
                .map(|(offset, payload)| StreamEntry {
                    seq: offset as u64 + 1,
                    payload,
                })
                .collect(),
        },
    });
    fold(messages)
}

fn fold(messages: Vec<Msg>) -> Model {
    let mut model = Model::default();
    for message in messages {
        update(&mut model, message);
    }
    let violations = model.check_invariants();
    assert!(violations.is_empty(), "fixture coherent: {violations:?}");
    model
}

fn chat_fixture(protocol: StructuredProtocol, name: &str, rows: Vec<Value>) -> Fixture {
    let model = model(protocol, name, rows);
    let mut chat = ChatView::open(&model, agent_id(protocol), 'a', false)
        .expect("fixture advertises a structured protocol");
    if protocol == StructuredProtocol::Codex {
        chat.set_codex_configuration_label(Some(
            "model=gpt-5.4 · approval=on-request · sandbox=workspace-write".to_string(),
        ));
    }
    chat.reconcile(&model);
    Fixture {
        model,
        view: ViewState {
            chat: Some(chat),
            ..ViewState::default()
        },
        now: fixed_now(),
    }
}

fn claude_fixture(rows: Vec<Value>) -> Fixture {
    chat_fixture(StructuredProtocol::Claude, "fix-auth", rows)
}

fn codex_fixture(rows: Vec<Value>) -> Fixture {
    chat_fixture(StructuredProtocol::Codex, "codex-retry", rows)
}

fn fleet_fixture(empty: bool) -> Fixture {
    let mut messages = vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host: host() }),
    ];
    if !empty {
        messages.extend([
            Msg::Server(ServerMsg::AgentUpserted {
                agent: agent(StructuredProtocol::Claude, "fix-auth"),
            }),
            Msg::Server(ServerMsg::AgentUpserted {
                agent: agent(StructuredProtocol::Codex, "codex-retry"),
            }),
        ]);
    }
    messages.extend([
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ]);
    Fixture {
        model: fold(messages),
        view: ViewState::default(),
        now: fixed_now(),
    }
}

/// The accounts one installation holds, as the front door lists them.
/// Three, because two would not show that the list is a list, and the
/// third is logged out — an account you still have but are not connected
/// to is exactly what the status column exists for.
fn profile_entries() -> Vec<ProfileEntry> {
    vec![
        ProfileEntry {
            id: ProfileId(Uuid::from_u128(0x9e01)),
            label: "Personal".to_string(),
            email: Some("robin@example.com".to_string()),
            status: "connected".to_string(),
            socket: PathBuf::from("/run/amux/9e01.sock"),
        },
        ProfileEntry {
            id: ProfileId(Uuid::from_u128(0x9e02)),
            label: "Work".to_string(),
            email: Some("robin@northwind.example".to_string()),
            status: "connected".to_string(),
            socket: PathBuf::from("/run/amux/9e02.sock"),
        },
        ProfileEntry {
            id: ProfileId(Uuid::from_u128(0x9e03)),
            label: "Conference laptop".to_string(),
            email: Some("robin@example.org".to_string()),
            status: "logged out".to_string(),
            socket: PathBuf::from("/run/amux/9e03.sock"),
        },
    ]
}

/// The switcher, open over the personal fleet with the work account under
/// the cursor: the frame a person sees the moment before they switch.
fn profile_switcher_fixture() -> Fixture {
    let mut fixture = fleet_fixture(false);
    let entries = profile_entries();
    let work = entries[1].socket.clone();
    fixture.view.mode = Mode::Switcher(SwitcherState::open(entries, Some(&work)));
    fixture
}

/// The fleet after that switch: the work account, which shares no host and
/// no agent with the personal one. Two accounts on one machine are two
/// devices, so nothing carries over.
fn switched_fleet_fixture() -> Fixture {
    let host = HostEntry {
        id: work_host_id(),
        name: "northwind-mbp".to_string(),
        ..host()
    };
    let messages = vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(work_host_id()),
        }),
        Msg::Server(ServerMsg::HostUpserted { host }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: work_agent(
                StructuredProtocol::Claude,
                AgentId::from_u128(21),
                "ship-invoices",
            ),
        }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: work_agent(
                StructuredProtocol::Codex,
                AgentId::from_u128(22),
                "audit-deps",
            ),
        }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
    ];
    Fixture {
        model: fold(messages),
        view: ViewState::default(),
        now: fixed_now(),
    }
}

fn work_host_id() -> HostId {
    HostId::from_u128(2)
}

fn work_agent(protocol: StructuredProtocol, id: AgentId, name: &str) -> Agent {
    Agent {
        id,
        host_id: work_host_id(),
        working_dir: "/work/northwind".into(),
        ..agent(protocol, name)
    }
}

fn claude_ready() -> Value {
    json!({"type": "amux.transcript_ready"})
}

fn claude_prompt(index: usize, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": format!("dddddddd-0000-4000-8000-{index:012}"),
        "sessionId": SESSION,
        "timestamp": "2026-08-12T09:12:00Z",
        "message": {"role": "user", "content": text},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn claude_assistant(index: usize, text: &str, stop: Option<&str>) -> Value {
    json!({
        "type": "assistant",
        "uuid": format!("eeeeeeee-0000-4000-8000-{index:012}"),
        "sessionId": SESSION,
        "timestamp": "2026-08-12T09:12:05Z",
        "message": {
            "id": format!("msg-{index}"),
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": stop,
        },
    })
}

fn claude_idle_rows() -> Vec<Value> {
    vec![
        claude_ready(),
        json!({"type": "permission-mode", "permissionMode": "default"}),
        claude_prompt(1, "Add retry with backoff to the sync client."),
        claude_assistant(
            2,
            "I added exponential backoff to `Client::reconnect` and kept the retry policy configurable.",
            Some("end_turn"),
        ),
        json!({
            "type": "system",
            "subtype": "turn_duration",
            "uuid": "ffffffff-0000-4000-8000-000000000003",
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:06Z",
            "durationMs": 6200,
        }),
    ]
}

fn claude_working_rows() -> Vec<Value> {
    vec![
        claude_ready(),
        json!({"type": "permission-mode", "permissionMode": "default"}),
        claude_prompt(10, "Now make the retry count configurable."),
        json!({
            "type": "assistant",
            "uuid": "eeeeeeee-0000-4000-8000-000000000011",
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:10Z",
            "message": {
                "id": "msg-working",
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "Inspecting the retry policy"},
                    {"type": "text", "text": "The cap belongs in `RetryConfig`; I’ll thread it through `SyncOptions`."}
                ],
                "stop_reason": "tool_use",
            },
        }),
        json!({
            "type": "assistant",
            "uuid": "eeeeeeee-0000-4000-8000-000000000012",
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:12Z",
            "message": {
                "id": "msg-tool",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu-check",
                    "name": "Bash",
                    "input": {"command": "cargo check -p amux-sync"}
                }],
                "stop_reason": "tool_use",
            },
        }),
    ]
}

fn permission_hook() -> Value {
    json!({
        "type": "hook.permission_request",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "sync/config.rs",
            "old_string": "pub max_attempts: u8,",
            "new_string": "pub max_attempts: u8,\npub jitter_ms: u16,"
        },
        "permission_mode": "default",
        "permission_suggestions": [{
            "type": "addDirectories",
            "destination": "session",
            "directories": ["/work/amux"]
        }]
    })
}

fn question_hook() -> Value {
    json!({
        "type": "hook.permission_request",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{
            "header": "Storage",
            "question": "Which stores should the migration cover?",
            "multiSelect": true,
            "options": [
                {"label": "Trust store", "description": "pairing and relay trust records"},
                {"label": "Session index", "description": "bounded tail metadata"}
            ]
        }]},
        "permission_mode": "default"
    })
}

fn plan_hook() -> Value {
    json!({
        "type": "hook.permission_request",
        "tool_name": "ExitPlanMode",
        "tool_input": {
            "plan": "## Approach\n\n1. Add RetryConfig to SyncOptions.\n2. Thread it through reconnect.\n3. Run focused and integration tests.\n\n## Out of scope\n\n- Relay backpressure.",
            "planFilePath": "~/.claude/plans/retry.md"
        },
        "permission_mode": "plan"
    })
}

fn claude_long_row(index: usize) -> Value {
    match index % 3 {
        0 => claude_prompt(index + 100, &format!("Investigate retry case {index}.")),
        1 => claude_assistant(
            index + 100,
            &format!("Retry case {index} is covered by the focused test."),
            Some("end_turn"),
        ),
        _ => json!({
            "type": "assistant",
            "uuid": format!("ffffffff-0000-4000-8000-{:012}", index + 100),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:05Z",
            "message": {
                "id": format!("msg-tool-long-{index}"),
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": format!("toolu-long-{index}"),
                    "name": "Read",
                    "input": {"file_path": format!("tests/retry_{index}.rs")}
                }],
                "stop_reason": "tool_use"
            }
        }),
    }
}

fn codex_ready() -> Value {
    json!({"type": "amux.codex_ready"})
}

fn codex_idle_rows() -> Vec<Value> {
    vec![
        codex_ready(),
        json!({"type":"turn/started","turn":{"id":"turn-done","status":"inProgress"}}),
        json!({"type":"item/completed","turnId":"turn-done","item":{"id":"user-idle","type":"userMessage","content":[{"type":"text","text":"Run the focused tests."}]}}),
        json!({"type":"item/completed","item":{"id":"cmd-idle","type":"commandExecution","command":"cargo test -p amux-ui","cwd":"/work/amux","status":"completed","exitCode":0,"aggregatedOutput":"42 passed"}}),
        json!({"type":"item/completed","item":{"id":"msg-idle","type":"agentMessage","text":"All focused tests pass.","phase":"final_answer"}}),
        json!({"type":"turn/completed","turn":{"id":"turn-done","status":"completed"}}),
    ]
}

fn codex_working_rows() -> Vec<Value> {
    vec![
        codex_ready(),
        json!({"type":"turn/started","turn":{"id":"turn-live","status":"inProgress"}}),
        json!({"type":"item/completed","turnId":"turn-live","item":{"id":"user-live","type":"userMessage","content":[{"type":"text","text":"Make retries configurable."}]}}),
        json!({"type":"item/started","item":{"id":"reason-live","type":"reasoning","content":[],"summary":[]}}),
        json!({"type":"item/reasoning/summaryTextDelta","itemId":"reason-live","summaryIndex":0,"delta":"Inspecting retry policy"}),
        json!({"type":"item/started","item":{"id":"cmd-live","type":"commandExecution","command":"cargo test -p amux-ui","cwd":"/work/amux","status":"inProgress"}}),
        json!({"type":"item/commandExecution/outputDelta","itemId":"cmd-live","stream":"stdout","delta":"running 42 tests\n"}),
        json!({"type":"item/started","item":{"id":"msg-live","type":"agentMessage","text":"Tests are still running.","phase":"commentary"}}),
    ]
}

fn codex_approval_rows(network: bool) -> Vec<Value> {
    let choices = if network {
        json!([
            "accept",
            {"applyNetworkPolicyAmendment": {
                "network_policy_amendment": {"host": "crates.io", "action": "allow"}
            }},
            "decline"
        ])
    } else {
        json!(["accept", "decline", "cancel"])
    };
    vec![
        codex_ready(),
        json!({"type":"turn/started","turn":{"id":"turn-ask","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"exec-ask","type":"commandExecution","command":"cargo test --workspace","cwd":"/work/amux","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-ask","command":"cargo test --workspace","cwd":"/work/amux","reason":"Run the repository test suite?","proposedNetworkPolicyAmendments":[{"host":"crates.io","action":"allow"}]}),
        json!({"type":"amux.codex_approval_required","request_id":"approval-1","availableDecisions":choices}),
    ]
}

fn codex_mcp_rows() -> Vec<Value> {
    vec![
        codex_ready(),
        json!({"type":"mcpServer/startupStatus/updated","threadId":"thread-1","name":"node_repl","status":"ready","error":null,"failureReason":null}),
        json!({"type":"mcpServer/startupStatus/updated","threadId":"thread-1","name":"codex_apps","status":"starting","error":null,"failureReason":null}),
        json!({"type":"mcpServer/startupStatus/updated","threadId":"thread-1","name":"issues","status":"failed","error":"launch failed","failureReason":"process exited"}),
    ]
}

fn codex_long_row(index: usize) -> Value {
    match index % 3 {
        0 => json!({
            "type": "item/completed",
            "item": {
                "id": format!("user-long-{index}"),
                "type": "userMessage",
                "content": [{"type": "text", "text": format!("Investigate retry case {index}.")}]
            }
        }),
        1 => json!({
            "type": "item/completed",
            "item": {
                "id": format!("message-long-{index}"),
                "type": "agentMessage",
                "text": format!("Retry case {index} is covered by the focused test."),
                "phase": "final_answer"
            }
        }),
        _ => json!({
            "type": "item/completed",
            "item": {
                "id": format!("command-long-{index}"),
                "type": "commandExecution",
                "command": format!("cargo test retry_case_{index}"),
                "cwd": "/work/amux",
                "status": "completed",
                "exitCode": 0,
                "aggregatedOutput": "1 passed"
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::StructuredProtocol;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{NamedState, all_states, fixture, long_feed};
    use crate::{FrameContext, Theme, render};

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut names = std::collections::BTreeSet::new();
        for state in all_states() {
            assert_eq!(NamedState::parse(state.name()), Some(*state));
            assert_eq!(state.name().parse::<NamedState>(), Ok(*state));
            assert!(
                names.insert(state.name()),
                "duplicate name {}",
                state.name()
            );
        }
        assert_eq!(NamedState::parse("unknown"), None);
    }

    /// The screen one named state draws at the size every capture uses.
    fn frame_text(state: NamedState) -> String {
        let fixture = fixture(state);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let context = FrameContext {
            viewport: (120, 40),
            theme: Theme::default(),
            now: fixture.now,
        };
        terminal
            .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
            .unwrap_or_else(|error| panic!("{state} failed to render: {error}"));
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).expect("cell in area").symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_named_state_renders_at_capture_size() {
        for state in all_states() {
            let _ = frame_text(*state);
        }
    }

    /// The gallery earns its name only while every block it was built to
    /// show is still on the screen. A block painter that grows by a row,
    /// or a fixture that gains one, pushes the oldest blocks off the top
    /// where nobody reviewing the capture would notice they had gone.
    #[test]
    fn the_gallery_shows_every_block_it_was_built_for() {
        let frame = frame_text(NamedState::ComponentGallery);
        for marker in [
            "Cap the retry backoff.",        // the prompt, on its surface
            "thought for",                   // the thinking marker
            "The cap belongs in",            // assistant markdown
            "2 reads · 1 search",            // the collapsed exploration run
            "Bash cargo check -p amux-sync", // a tool line
            "└ Finished in 4.10s",           // its continuation
            "Edit sync/config.rs · +2 −1",   // the landed file change
            "? Cap → 6 attempts",            // the answered question, collapsed
            "plan approved",                 // the approved plan and its preview
            "ctrl+t to read",
            "Subagent finished",                // a background subagent
            "codex-retry",                      // a message from another agent
            "api error (server_error)",         // an error
            "─ turn · 1m 2s",                   // the turn rule
            "permission — Edit sync/config.rs", // the docked ask panel
            "-    pub max_attempts: u8,",       // its unified diff
            "+    pub max_attempts: u16,",
        ] {
            assert!(
                frame.contains(marker),
                "the gallery no longer shows {marker:?}:\n{frame}"
            );
        }
        assert!(
            !frame.contains("@@ -"),
            "an ask-time diff is snippet-relative and may state no file position:\n{frame}"
        );
    }

    /// The Codex page carries what a Claude session cannot say at all,
    /// and the same rule holds: every block whole, none scrolled away.
    #[test]
    fn the_codex_gallery_page_shows_what_claude_cannot() {
        let frame = frame_text(NamedState::ComponentGalleryCodex);
        for marker in [
            "context compacted · turn-08",    // the compaction rule
            "Cap the retry backoff.",         // the prompt, on its surface
            "MCP servers · 0 starting",       // the Codex-only startup block
            "~ reasoning",                    // the thinking marker
            "summary: Where the cap belongs", // and its continuation
            "The cap belongs in",             // assistant markdown
            "file changes · 1 · done",        // the landed file change
            "sync/backoff.rs · update",
            "12 \u{2502}      if attempt >= config", // a numbered context row
            "13 \u{2502} -        return Err(RetryError", // a numbered removal
            "13 \u{2502} +        return Err(RetryError", // a numbered addition
            "unrecognized Codex row",                // the degraded row
            "thread/experimental/telemetryPing",     // and the method it kept
            "─ turn completed",                      // the turn rule
            "$ cargo test --workspace · awaiting approval",
            "approval — command", // the docked approval
            "apply network policy change · allow crates.io",
        ] {
            assert!(
                frame.contains(marker),
                "the Codex gallery page no longer shows {marker:?}:\n{frame}"
            );
        }
    }

    /// The point of the pair: exploration folds away, and the edit between
    /// the two runs does not.
    #[test]
    fn the_collapse_pair_keeps_the_edit_out_of_its_runs() {
        for state in [
            NamedState::ExplorationCollapsed,
            NamedState::ExplorationExpanded,
        ] {
            let frame = frame_text(state);
            assert!(
                frame.contains("2 reads · 2 searches"),
                "{state} lost the first run:\n{frame}"
            );
            assert!(
                frame.contains("2 reads · 1 search"),
                "{state} lost the second run:\n{frame}"
            );
            assert!(
                frame.contains("Edit sync/config.rs · +3 −1"),
                "{state} folded the edit into a run:\n{frame}"
            );
        }
    }

    #[test]
    fn long_feeds_retain_exactly_the_requested_entries() {
        let claude = long_feed(StructuredProtocol::Claude, 1_000);
        let claude_layer = claude
            .model
            .agents()
            .find_map(|card| claude.model.claude(card.agent.id))
            .expect("Claude layer");
        assert_eq!(claude_layer.entries().count(), 1_000);
        assert!(
            claude_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::claude::FeedEntryKind::Prompt(_)))
        );
        assert!(
            claude_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::claude::FeedEntryKind::Message(_)))
        );
        assert!(
            claude_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::claude::FeedEntryKind::Tool(_)))
        );

        let codex = long_feed(StructuredProtocol::Codex, 1_000);
        let codex_layer = codex
            .model
            .agents()
            .find_map(|card| codex.model.codex(card.agent.id))
            .expect("Codex layer");
        assert_eq!(codex_layer.entries().count(), 1_000);
        assert!(
            codex_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::codex::FeedEntryKind::Prompt(_)))
        );
        assert!(
            codex_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::codex::FeedEntryKind::Message(_)))
        );
        assert!(
            codex_layer
                .entries()
                .any(|entry| matches!(&entry.kind, amux_ui::codex::FeedEntryKind::Work(_)))
        );
    }
}
