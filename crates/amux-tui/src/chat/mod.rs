//! Structured chat dispatch. The outer view owns exactly one native
//! per-agent view; Claude and Codex keep their content, panels, and key
//! semantics separate while sharing only proven terminal renderers.

pub(crate) mod attach;
pub(crate) mod attachments;
pub(crate) mod blocks;
pub(crate) mod claude;
mod codex;
pub mod diff;
pub(crate) mod frame;
pub(crate) mod inline;
mod unsupported;
pub(crate) mod viewport;

use std::cell::RefCell;

use amux_ui::{
    AgentId, AgentMessagePresentation, AgentMessageSender, Command, FamilyNeed, Model, OpId,
    StructuredProtocol, Why, message_digest,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
pub use frame::PaintStats;
use frame::{
    ChatFrameParts, ChatGeometry, FeedMetrics, FrameSpacing, PaintCache, PaintedBlock,
    compose_chat_frame, feed_metrics,
};
use ratatui::text::Line;
use serde::{Deserialize, Serialize};
use viewport::{FeedViewport, apply_scroll, move_focus, toggle_focused_run};

use crate::clipboard::ClipboardContent;
use crate::composer::Composer;
use crate::render::{FrameContext, Theme};
use crate::view::{QuitGuard, UiAction};

/// Feed scroll state shared because both native screens have the same
/// sticky-bottom terminal interaction, not because their feed entries share
/// a representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedScroll {
    Following,
    Paused {
        top_line: usize,
        entry_watermark: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum AgentChatView {
    Claude(claude::View),
    Codex(codex::View),
    /// A protocol this build has no fold for. It renders a placeholder and
    /// takes no input, so a kind can ship before its reader does without
    /// leaving its agents unreachable from the fleet.
    Unsupported(unsupported::View),
}

#[derive(Clone, Debug)]
struct CachedFeedMetrics {
    viewport: (u16, u16),
    metrics: FeedMetrics,
    blocks: Vec<PaintedBlock>,
    following_geometry: ChatGeometry,
    paused_geometry: ChatGeometry,
}

impl CachedFeedMetrics {
    fn geometry(&self, paused: bool) -> ChatGeometry {
        if paused {
            self.paused_geometry
        } else {
            self.following_geometry
        }
    }
}

/// Renderer-local state for one structured chat. Native sub-state remains
/// namespaced; dispatch is exhaustive at this one additive seam.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatView {
    pub agent: AgentId,
    pub(crate) viewport: FeedViewport,
    inner: AgentChatView,
    /// Metrics from the adapter blocks painted for the latest frame. Key
    /// handling consumes this instead of walking and painting the feed a
    /// second time merely to discover its scroll bounds.
    ///
    /// Skipped by serde like the paint cache below: both are derived from
    /// the last paint, so a deserialized chat rebuilds them on its next
    /// draw rather than carrying a frame's worth of geometry around. A
    /// chat restored from bytes must therefore be drawn before its keys
    /// are handled, exactly as a freshly opened one must.
    #[serde(skip)]
    feed_metrics: RefCell<Option<CachedFeedMetrics>>,
    #[serde(skip)]
    paint_cache: RefCell<PaintCache>,
}

impl Clone for ChatView {
    fn clone(&self) -> Self {
        Self {
            agent: self.agent,
            viewport: self.viewport.clone(),
            inner: self.inner.clone(),
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        }
    }
}

fn frame_parts(
    model: &Model,
    chat: &ChatView,
    cache: &mut PaintCache,
    ctx: &FrameContext,
) -> ChatFrameParts {
    match &chat.inner {
        AgentChatView::Claude(view) => {
            claude::claude_frame_parts(model, view, &chat.viewport, cache, ctx)
        }
        AgentChatView::Codex(view) => {
            codex::codex_frame_parts(model, view, &chat.viewport, cache, ctx)
        }
        AgentChatView::Unsupported(view) => unsupported::frame_parts(model, view, ctx),
    }
}

impl ChatView {
    pub fn open(model: &Model, agent: AgentId, leader: char, kitty: bool) -> Option<Self> {
        let protocol = model.agent(agent)?.structured_protocol()?;
        let inner =
            match protocol {
                StructuredProtocol::Claude => {
                    AgentChatView::Claude(claude::View::open(agent, leader, kitty))
                }
                StructuredProtocol::Codex => {
                    AgentChatView::Codex(codex::View::open(agent, leader, kitty))
                }
                StructuredProtocol::ClaudeSdk => AgentChatView::Unsupported(
                    unsupported::View::open(agent, protocol.as_str(), leader, kitty),
                ),
            };
        Some(Self {
            agent,
            viewport: FeedViewport::following(),
            inner,
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        })
    }

    /// Whether the last paint's feed metrics are still cached. The
    /// serde round-trip tests read it to prove the caches stay behind.
    #[cfg(test)]
    pub(crate) fn has_cached_metrics(&self) -> bool {
        self.feed_metrics.borrow().is_some()
    }

    /// Deterministic constructors used by pure golden fixtures.
    pub fn open_claude(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            viewport: FeedViewport::following(),
            inner: AgentChatView::Claude(claude::View::open(agent, leader, kitty)),
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        }
    }

    pub fn open_codex(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            viewport: FeedViewport::following(),
            inner: AgentChatView::Codex(codex::View::open(agent, leader, kitty)),
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        }
    }

    pub fn open_unsupported(
        agent: AgentId,
        protocol: &'static str,
        leader: char,
        kitty: bool,
    ) -> Self {
        Self {
            agent,
            viewport: FeedViewport::following(),
            inner: AgentChatView::Unsupported(unsupported::View::open(
                agent, protocol, leader, kitty,
            )),
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        }
    }

    pub fn composer_mut(&mut self) -> &mut Composer {
        match &mut self.inner {
            AgentChatView::Claude(view) => &mut view.composer,
            AgentChatView::Codex(view) => &mut view.composer,
            AgentChatView::Unsupported(view) => &mut view.composer,
        }
    }

    pub fn quit_guard(&self) -> &QuitGuard {
        match &self.inner {
            AgentChatView::Claude(view) => &view.quit_guard,
            AgentChatView::Codex(view) => &view.quit_guard,
            AgentChatView::Unsupported(view) => &view.quit_guard,
        }
    }

    pub fn quit_guard_mut(&mut self) -> &mut QuitGuard {
        match &mut self.inner {
            AgentChatView::Claude(view) => &mut view.quit_guard,
            AgentChatView::Codex(view) => &mut view.quit_guard,
            AgentChatView::Unsupported(view) => &mut view.quit_guard,
        }
    }

    pub fn set_help(&mut self, help: bool) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.help = help,
            AgentChatView::Codex(view) => view.help = help,
            AgentChatView::Unsupported(view) => view.help = help,
        }
    }

    pub fn set_kitty(&mut self, kitty: bool) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.kitty = kitty,
            AgentChatView::Codex(view) => view.kitty = kitty,
            AgentChatView::Unsupported(view) => view.kitty = kitty,
        }
    }

    pub fn set_scroll(&mut self, scroll: FeedScroll) {
        self.viewport.scroll = scroll;
    }

    /// Current shared feed position, exposed for deterministic interaction
    /// recordings without granting another mutation path around the reducer.
    pub fn scroll(&self) -> &FeedScroll {
        &self.viewport.scroll
    }

    pub fn set_codex_configuration_label(&mut self, label: Option<String>) {
        if let AgentChatView::Codex(view) = &mut self.inner {
            view.configuration_label = label;
            self.feed_metrics.get_mut().take();
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn paint_stats(&self) -> PaintStats {
        self.paint_cache.borrow().stats()
    }

    #[cfg(feature = "fixtures")]
    pub fn feed_total_rows(&self) -> Option<usize> {
        self.feed_metrics
            .borrow()
            .as_ref()
            .map(|cached| cached.metrics.total_rows)
    }

    pub fn reconcile(&mut self, model: &Model) {
        self.feed_metrics.get_mut().take();
        match &mut self.inner {
            AgentChatView::Claude(view) => view.reconcile(model),
            AgentChatView::Codex(view) => view.reconcile(model),
            AgentChatView::Unsupported(_) => {}
        }
    }

    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        self.feed_metrics.get_mut().take();
        match &mut self.inner {
            AgentChatView::Claude(view) => view.note_dispatched(op, command),
            AgentChatView::Codex(view) => view.note_dispatched(op, command),
            // This chat dispatches nothing, so it has nothing to await.
            AgentChatView::Unsupported(_) => {}
        }
    }

    pub fn needs_tick(&self, model: &Model) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.needs_tick(model),
            AgentChatView::Codex(view) => view.needs_tick(model),
            AgentChatView::Unsupported(_) => false,
        }
    }

    pub fn expire_quit_guard(&mut self, now: DateTime<Utc>) -> bool {
        self.quit_guard_mut().expire(now)
    }

    fn layout_for(
        &self,
        model: &Model,
        viewport: (u16, u16),
        now: DateTime<Utc>,
        target_paused: bool,
    ) -> (FeedMetrics, ChatGeometry) {
        if let Some(cached) = self.feed_metrics.borrow().as_ref()
            && cached.viewport == viewport
        {
            return (cached.metrics.clone(), cached.geometry(target_paused));
        }

        // An input can arrive before the first frame, including in tests.
        // Build the same adapter parts once as a fallback, then retain the
        // resulting metrics for every subsequent key until render or
        // reconciliation refreshes them.
        let ctx = FrameContext {
            viewport,
            theme: Theme::default(),
            now,
        };
        let mut cache = self.paint_cache.borrow_mut();
        let parts = frame_parts(model, self, &mut cache, &ctx);
        drop(cache);
        let following_geometry = parts.geometry(viewport, false);
        let paused_geometry = parts.geometry(viewport, true);
        let metrics = feed_metrics(&parts.feed, FrameSpacing::DEFAULT, &paused_geometry);
        let geometry = if target_paused {
            paused_geometry
        } else {
            following_geometry
        };
        self.feed_metrics.replace(Some(CachedFeedMetrics {
            viewport,
            metrics: metrics.clone(),
            blocks: parts.feed.blocks,
            following_geometry,
            paused_geometry,
        }));
        (metrics, geometry)
    }

    fn metrics_for(&self, model: &Model, viewport: (u16, u16), now: DateTime<Utc>) -> FeedMetrics {
        self.layout_for(model, viewport, now, true).0
    }

    fn pending_leader(&self) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.pending_leader,
            AgentChatView::Codex(view) => view.pending_leader,
            AgentChatView::Unsupported(view) => view.pending_leader,
        }
    }

    /// The review page, while it is the frame.
    fn open_review_mut(&mut self) -> Option<&mut crate::review::ReviewView> {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.open_review_mut(),
            // Only Claude's chat can draft a review.
            AgentChatView::Codex(_) | AgentChatView::Unsupported(_) => None,
        }
    }

    fn overlay_open(&self) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.overlay_open(),
            AgentChatView::Codex(view) => view.overlay_open(),
            // The placeholder never opens an overlay of its own.
            AgentChatView::Unsupported(_) => false,
        }
    }

    /// Read a text attachment in the fullscreen reader.
    ///
    /// Only Claude's chat has a reader; a Codex chat states the pasted
    /// text's length on the feed row and leaves it there until Codex's
    /// screen grows one of its own.
    fn open_text_reader(&mut self, name: String, body: String) {
        if let AgentChatView::Claude(view) = &mut self.inner {
            view.open_text_reader(name, body);
        }
    }

    /// Read a sent review in the fullscreen reader, reporting whether a
    /// reader opened — a chat without one has no use for the diff.
    fn open_review_reader(
        &mut self,
        header: amux_ui::review::ReviewHeader,
        comments: Vec<amux_ui::review::ReviewComment>,
    ) -> bool {
        match &mut self.inner {
            AgentChatView::Claude(view) => {
                view.open_review_reader(header, comments);
                true
            }
            _ => false,
        }
    }

    fn consume_shared_leader(&mut self) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.pending_leader = false,
            AgentChatView::Codex(view) => view.pending_leader = false,
            AgentChatView::Unsupported(view) => view.pending_leader = false,
        }
        self.quit_guard_mut().disarm();
    }

    fn copy_text(&self) -> Option<String> {
        let cached = self.feed_metrics.borrow();
        let blocks = &cached.as_ref()?.blocks;
        match self.viewport.focus {
            Some(focused) => blocks
                .iter()
                .find(|block| block.key == focused)
                .map(|block| block.copy_text.clone()),
            None => blocks.last().map(|block| block.copy_text.clone()),
        }
    }
}

/// Open the attachment the feed's focus is on, if it is on one.
///
/// An image or a file leaves for the host's viewer through the runtime,
/// which fetches and verifies the bytes first; pasted text is read here,
/// because there is no file to hand anyone. A sent review opens the
/// reader and asks for its diff in the same breath: the comments arrived
/// with the message, the patch they hang on is an artifact on the
/// agent's host that this viewer may never have seen.
fn open_focused_attachment(chat: &mut ChatView, model: &Model) -> Option<UiAction> {
    let focus = chat.viewport.focus?;
    let mention = attachments::focused_mention(model, chat.agent, focus)?;
    match attachments::opening(&mention)? {
        attachments::Opening::External(id) => Some(UiAction::Dispatch(Command::OpenAttachment {
            agent: chat.agent,
            id,
        })),
        attachments::Opening::Read { title, body } => {
            chat.open_text_reader(title, body);
            None
        }
        attachments::Opening::Review { header, comments } => {
            let id = header.diff.clone();
            chat.open_review_reader(*header, comments)
                .then_some(UiAction::Dispatch(Command::FetchDiff {
                    agent: chat.agent,
                    id,
                }))
        }
    }
}

pub fn handle_chat_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
    now: DateTime<Utc>,
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    // Feed focus is shared frame state. The native handler owns the first
    // leader press; once it is pending, these chords are consumed here so
    // Claude and Codex cannot drift. Ctrl+arrows are the terminal-dependent
    // convenience tier for the same movement. Native overlays own every key,
    // so help can close on any press and Claude's reader keeps its Esc chain.
    if !chat.overlay_open() {
        let metrics = chat.metrics_for(model, viewport, now);
        let leader_pending = chat.pending_leader();
        if leader_pending {
            match key.code {
                KeyCode::Char('k') => {
                    chat.consume_shared_leader();
                    move_focus(
                        &mut chat.viewport,
                        &metrics,
                        -1,
                        entry_watermark(model, chat.agent),
                    );
                    return None;
                }
                KeyCode::Char('j') => {
                    chat.consume_shared_leader();
                    move_focus(
                        &mut chat.viewport,
                        &metrics,
                        1,
                        entry_watermark(model, chat.agent),
                    );
                    return None;
                }
                KeyCode::Char('y') => {
                    chat.consume_shared_leader();
                    return chat.copy_text().map(UiAction::CopyToClipboard);
                }
                KeyCode::Char('o') => {
                    chat.consume_shared_leader();
                    // The chord opens what the focus is on: an
                    // attachment row goes to the host's viewer or the
                    // reader, an exploration run opens and shuts. One
                    // chord, because the feed has one focus.
                    if let Some(action) = open_focused_attachment(chat, model) {
                        return Some(action);
                    }
                    let cached = chat.feed_metrics.get_mut();
                    let blocks = cached
                        .as_ref()
                        .map(|cached| cached.blocks.as_slice())
                        .unwrap_or_default();
                    toggle_focused_run(&mut chat.viewport, blocks);
                    return None;
                }
                _ => {}
            }
        }

        if !leader_pending && key.modifiers.contains(KeyModifiers::CONTROL) {
            let delta = match key.code {
                KeyCode::Up => Some(-1),
                KeyCode::Down => Some(1),
                _ => None,
            };
            if let Some(delta) = delta {
                chat.quit_guard_mut().disarm();
                move_focus(
                    &mut chat.viewport,
                    &metrics,
                    delta,
                    entry_watermark(model, chat.agent),
                );
                return None;
            }
        }

        if !leader_pending && key.code == KeyCode::Esc && chat.viewport.focus.take().is_some() {
            chat.quit_guard_mut().disarm();
            return None;
        }
    }

    let action = match &mut chat.inner {
        AgentChatView::Claude(view) => claude::handle_chat_key(view, model, key, viewport, now),
        AgentChatView::Codex(view) => codex::handle_chat_key(view, model, key, viewport, now),
        AgentChatView::Unsupported(view) => unsupported::handle_chat_key(view, model, key, now),
    };
    let intent = match &mut chat.inner {
        AgentChatView::Claude(view) => view.scroll_intent.take(),
        AgentChatView::Codex(view) => view.scroll_intent.take(),
        // The placeholder emits no scroll intents; wheel motion over its
        // feed still routes through the shared viewport below.
        AgentChatView::Unsupported(_) => None,
    };
    if let Some(intent) = intent {
        let metrics = chat.metrics_for(model, viewport, now);
        apply_scroll(
            &mut chat.viewport,
            &metrics,
            intent,
            entry_watermark(model, chat.agent),
        );
    }
    action
}

/// Ctrl+V: attach whatever the clipboard holds.
///
/// The content is a parameter rather than read here, so the binding runs
/// the same way in a test and in a recording as it does under a person's
/// hands — a capture of pasting an image must not depend on what the
/// recording machine's clipboard happened to hold.
pub fn handle_chat_clipboard(chat: &mut ChatView, model: &Model, content: ClipboardContent) {
    match &mut chat.inner {
        AgentChatView::Claude(view) => claude::keys::attach_clipboard(view, model, content),
        AgentChatView::Codex(view) => codex::keys::attach_clipboard(view, model, content),
        // The placeholder has no draft to attach anything to.
        AgentChatView::Unsupported(_) => {}
    }
}

pub fn handle_chat_paste(chat: &mut ChatView, model: &Model, text: &str) {
    match &mut chat.inner {
        AgentChatView::Claude(view) => claude::handle_chat_paste(view, model, text),
        AgentChatView::Codex(view) => codex::handle_chat_paste(view, model, text),
        // Nothing to paste into.
        AgentChatView::Unsupported(_) => {}
    }
}

/// Route wheel motion over the feed through the same reducer as paging.
/// Mouse buttons, motion, and wheel events over any other chat region are
/// deliberately inert; native selection remains the terminal's Shift
/// override while capture is enabled.
pub fn handle_chat_mouse(
    chat: &mut ChatView,
    model: &Model,
    event: MouseEvent,
    size: (u16, u16),
) -> bool {
    const NOTCH_ROWS: i32 = 3;
    let rows = match event.kind {
        MouseEventKind::ScrollUp => -NOTCH_ROWS,
        MouseEventKind::ScrollDown => NOTCH_ROWS,
        _ => return false,
    };

    // The review page is the whole frame while it is open, so a notch
    // anywhere on screen scrolls its body rather than the feed it hides.
    // It scrolls without moving the cursor, exactly as its own scroll
    // keys do.
    if let Some(review) = chat.open_review_mut() {
        review.resize(size.0, size.1);
        let before = review.scroll();
        review.handle_wheel(rows);
        return review.scroll() != before;
    }

    let intent = viewport::ScrollIntent::Rows(rows);
    let (metrics, geometry) = chat.layout_for(
        model,
        size,
        Utc::now(),
        matches!(chat.viewport.scroll, FeedScroll::Paused { .. }),
    );
    let row = event.row as usize;
    if chat.overlay_open()
        || row < geometry.feed_top
        || row >= geometry.feed_top.saturating_add(geometry.feed_rows)
    {
        return false;
    }

    apply_scroll(
        &mut chat.viewport,
        &metrics,
        intent,
        entry_watermark(model, chat.agent),
    )
}

pub(crate) fn build_chat_lines(
    model: &Model,
    chat: &ChatView,
    ctx: &FrameContext,
) -> Vec<Line<'static>> {
    const MIN_WIDTH: usize = 24;
    const MIN_HEIGHT: usize = 10;

    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return vec![Line::from("amux: terminal too small")];
    }
    let mut cache = chat.paint_cache.borrow_mut();
    cache.reset_stats();
    let parts = frame_parts(model, chat, &mut cache, ctx);
    drop(cache);
    let overlaid = parts.overlay.is_some();
    let banner = parts.banner.is_some();
    let following_geometry = parts.geometry(ctx.viewport, false);
    let paused_geometry = parts.geometry(ctx.viewport, true);
    let metrics = feed_metrics(&parts.feed, FrameSpacing::DEFAULT, &paused_geometry);
    let blocks = parts.feed.blocks.clone();
    chat.feed_metrics.replace(Some(CachedFeedMetrics {
        viewport: ctx.viewport,
        metrics,
        blocks,
        following_geometry,
        paused_geometry,
    }));

    let mut lines = compose_chat_frame(parts, &chat.viewport, ctx.theme, ctx.viewport);
    // The sticky diagnostic takes the header gap rather than reducing the
    // feed, and stays off overlays whose rows are all content.
    let row = 1 + usize::from(banner);
    if !overlaid && model.has_invariant_warning() && lines.len() > row {
        lines[row] = blocks::invariant_warning_row(width, ctx.theme);
    }
    lines
}

/// Everything an agent-message row needs besides the message itself: who
/// this chat belongs to (so a sender's host can be named only when it is
/// somebody else's), whether completions are open, and the chord that
/// changes that — the affordance has to name the key, so the two travel
/// together.
#[derive(Clone, Copy)]
pub(crate) struct MessageView<'m> {
    model: &'m Model,
    agent: AgentId,
    open: bool,
    leader: char,
}

impl<'m> MessageView<'m> {
    pub(crate) fn new(model: &'m Model, agent: AgentId, open: bool, leader: char) -> Self {
        Self {
            model,
            agent,
            open,
            leader,
        }
    }

    pub(crate) fn sender(&self, from: &str) -> String {
        sender_marker(self.model, self.agent, Model::agent_message_sender(from))
    }

    /// The rows a message's body makes (U4). An ordinary message shows
    /// everything it said — someone is talking to this agent. A
    /// completion is a report from a child and closes to its first line,
    /// stating what is behind the fold and how to open it, because a chat
    /// that unrolls every finished child's last message stops being
    /// readable at the exact moment several of them finish. An exit says
    /// what little the envelope carried and offers nothing to open,
    /// because there is nothing there.
    pub(crate) fn body(&self, presentation: AgentMessagePresentation, text: &str) -> MessageBody {
        match presentation {
            AgentMessagePresentation::Inbound => MessageBody {
                text: text.to_string(),
                affordance: None,
            },
            AgentMessagePresentation::Notice => MessageBody {
                text: message_digest(text).head.to_string(),
                affordance: None,
            },
            AgentMessagePresentation::Finished if self.open => MessageBody {
                text: text.to_string(),
                affordance: (message_digest(text).hidden_lines > 0)
                    .then(|| format!("⌃ close · C-{} m", self.leader)),
            },
            AgentMessagePresentation::Finished => {
                let digest = message_digest(text);
                MessageBody {
                    text: digest.head.to_string(),
                    affordance: match digest.hidden_lines {
                        0 => None,
                        1 => Some(format!("⌄ 1 more line · C-{} m", self.leader)),
                        n => Some(format!("⌄ {n} more lines · C-{} m", self.leader)),
                    },
                }
            }
        }
    }
}

/// A message body as it is being shown: what to render, and the one line
/// that states what is not being rendered.
pub(crate) struct MessageBody {
    pub(crate) text: String,
    pub(crate) affordance: Option<String>,
}

/// The directional glyph a message wears (U4): one per presentation, the
/// same in both chats.
pub(crate) fn message_glyph(
    presentation: AgentMessagePresentation,
    theme: crate::render::Theme,
) -> (&'static str, ratatui::style::Style) {
    match presentation {
        AgentMessagePresentation::Finished => ("✔", theme.ok()),
        AgentMessagePresentation::Notice => ("·", theme.muted()),
        AgentMessagePresentation::Inbound => ("←", theme.emphasis()),
    }
}

/// Who a message came from, in words (U4): the sender's name, and the
/// host only when it is not this agent's own. A chat row is for a person,
/// and a person reading their own machine's name in every row learns
/// nothing from it.
///
/// A host this inventory cannot name is left exactly as it arrived. An
/// address nobody here can resolve is still the truth about where the
/// message came from, and shortening it to the half we recognise would
/// be inventing agreement.
pub(crate) fn sender_marker(
    model: &Model,
    agent: AgentId,
    sender: AgentMessageSender<'_>,
) -> String {
    let AgentMessageSender::Address { name, host, .. } = sender else {
        return sender.raw().to_string();
    };
    if model
        .agent(agent)
        .is_some_and(|card| card.agent.host_id == host)
    {
        return name.to_string();
    }
    match model.host_name(host) {
        Some(host_name) => format!("{name} @ {host_name}"),
        None => sender.raw().to_string(),
    }
}

/// The banner a child raises in its parent's chat (U1): who is waiting,
/// what for, and — from the child's own layer — the one line that says
/// which act is blocked.
///
/// Composed, never synthesized. Nothing is written into the parent's
/// stream and nothing is stored, so the banner is a fact about right now:
/// answering the ask anywhere, in the child's own chat or on another
/// device, empties it on the next frame with nothing to clear. Only the
/// loudest need is named; the rest are counted, because a chat that
/// spends four rows on other agents' business is no longer this agent's
/// chat.
pub(crate) fn family_banner(model: &Model, agent: AgentId) -> Option<FamilyBanner> {
    let needs = model.family_needs(agent);
    let first = needs.first()?;
    let name = first.card.display_name();
    let mut text = match (first.why, ask_detail(model, first)) {
        (Why::Permission, Some(detail)) => format!("{name} needs permission: {detail}"),
        (Why::Permission, None) => format!("{name} needs permission"),
        (Why::Question, Some(detail)) => format!("{name} has a question: {detail}"),
        (Why::Question, None) => format!("{name} has a question"),
        (Why::Finished, _) => format!("{name} finished"),
    };
    if needs.len() > 1 {
        text.push_str(&format!(" · +{} more", needs.len() - 1));
    }
    Some(FamilyBanner {
        child: first.agent(),
        text,
    })
}

/// The banner, before it is words: the need it names and the child that
/// raised it. The parent's chat needs both — the words for the row, the
/// child for the panel the row leads to (U2).
pub(crate) struct FamilyBanner {
    /// The child the loudest need belongs to: the one `<leader> a` docks.
    pub(crate) child: AgentId,
    text: String,
}

impl FamilyBanner {
    /// The row as it reads. The chord that docks the child's own panel
    /// here is named only when it would open one — a finished child
    /// wants a person, not an answer, and a parent whose own ask holds
    /// the bottom block has nowhere to put a guest (P10).
    pub(crate) fn row(&self, answerable: bool, leader: char) -> String {
        match answerable {
            true => format!("{} · C-{leader} a answer", self.text),
            false => self.text.clone(),
        }
    }
}

/// The child's layer decides what its own ask looks like; the parent's
/// chat only decides that it is shown at all.
fn ask_detail(model: &Model, need: &FamilyNeed<'_>) -> Option<String> {
    match need.layer()? {
        StructuredProtocol::Claude => claude::ask_detail(model, need.agent()),
        StructuredProtocol::Codex => codex::ask_detail(model, need.agent()),
        // An unfolded layer raises no need, so it has no detail to give.
        StructuredProtocol::ClaudeSdk => None,
    }
}

/// The next agent to show while cycling through a family (U3): the one
/// after this chat's agent in family order, wrapping past the last back
/// to the top row — so `into the children and back` is one repeated key
/// rather than two.
///
/// Members the chrome cannot open are skipped rather than shown: a chat
/// needs a structured protocol this build renders and a host that answers,
/// and dropping the human onto a frame that can say nothing would be a
/// worse answer than staying put. When nothing else in the family
/// qualifies, the key does nothing at all.
pub(crate) fn next_in_family(model: &Model, agent: AgentId) -> Option<AgentId> {
    let root = model.family_root(agent)?;
    let line: Vec<AgentId> = std::iter::once(root)
        .chain(
            model
                .family_of(root)
                .into_iter()
                .map(|member| member.card.agent.id),
        )
        .collect();
    let at = line.iter().position(|id| *id == agent)?;
    line.iter()
        .cycle()
        .skip(at + 1)
        .take(line.len() - 1)
        .copied()
        .find(|id| openable(model, *id))
}

fn openable(model: &Model, agent: AgentId) -> bool {
    model.agent(agent).is_some_and(|card| {
        card.structured_protocol().is_some() && model.host_online(card.agent.host_id)
    })
}

/// Which of the family chords would do something in this chat right now
/// — the input the `?` overlay derives its family rows from, so the
/// overlay can never name a chord that is inert here (P10).
pub(crate) fn family_keys(model: &Model, agent: AgentId) -> crate::bindings::FamilyKeys {
    crate::bindings::FamilyKeys {
        cycle: next_in_family(model, agent).is_some(),
        reports: has_closable_completion(model, agent),
        answer: family_banner(model, agent)
            .is_some_and(|banner| inline::can_open(model, agent, banner.child)),
    }
}

/// Whether any completion in this chat has a body behind its first line
/// — the exact condition under which `<leader> m` changes what is on
/// screen. A completion that said one thing is already showing all of
/// it, and a chat of those has nothing to open.
fn has_closable_completion(model: &Model, agent: AgentId) -> bool {
    model
        .claude(agent)
        .is_some_and(amux_ui::claude::ClaudeLayer::has_foldable_completion)
        || model
            .codex(agent)
            .is_some_and(amux_ui::codex::CodexLayer::has_foldable_completion)
}

/// The header's family marker (U3): how many agents this one has spawned,
/// at any depth, and empty when it has spawned none. It is also the
/// discoverable half of `<leader> n` — the count says there is somewhere
/// to cycle to.
pub(crate) fn subagent_marker(model: &Model, agent: AgentId) -> String {
    match model.family_of(agent).len() {
        0 => String::new(),
        1 => " · ⋯ 1 subagent".to_string(),
        n => format!(" · ⋯ {n} subagents"),
    }
}

/// The shared terminal sentence for a typed amux send. Agent folds retain
/// different native call types, but the outbound conversation row is one TUI
/// idiom and must never choose a blank line as its visible summary.
pub(crate) fn format_amux_send(to: Option<&str>, text: Option<&str>) -> String {
    let target = to.unwrap_or("an agent");
    match text.and_then(|text| text.lines().find(|line| !line.trim().is_empty())) {
        Some(head) => format!("→ {target} · {}", head.trim()),
        None => format!("→ {target}"),
    }
}

pub fn entry_watermark(model: &Model, agent: AgentId) -> u64 {
    match model
        .agent(agent)
        .and_then(amux_ui::AgentCard::structured_protocol)
    {
        Some(StructuredProtocol::Claude) => claude::entry_watermark(model, agent),
        Some(StructuredProtocol::Codex) => model.codex(agent).map_or(0, |layer| {
            layer.evicted_entries() + layer.entry_count() as u64
        }),
        Some(StructuredProtocol::ClaudeSdk) | None => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use amux_ui::{
        Agent, AgentId, Attention, ClaudeCommand, Command, HostEntry, HostTrustStatus, Model, Msg,
        OpId, SendGate, ServerMsg, StreamEntry, StreamMsg, StructuredProtocol, update,
    };
    use chrono::{DateTime, TimeDelta};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::text::Line;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        AgentChatView, ChatView, FeedScroll, build_chat_lines, entry_watermark, format_amux_send,
    };
    use crate::chat::blocks::RunKey;
    use crate::chat::frame::{BlockKey, PaintedBlock};
    use crate::render::{FrameContext, INVARIANT_WARNING, Theme, str_width};
    use crate::view::{UiAction, ViewState, visible_rows};

    fn at(seconds: i64) -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(1_754_697_600 + seconds, 0).expect("fixture timestamp")
    }

    fn a_host(online: bool) -> HostEntry {
        HostEntry {
            id: Uuid::from_u128(42),
            name: "protocol-host".to_string(),
            online,
            version: None,
            capabilities: None,
            trust_status: HostTrustStatus::Trusted,
            last_dial_error: None,
        }
    }

    fn model_with_protocol(protocol: &str) -> (Model, AgentId) {
        let agent = Uuid::from_u128(41);
        let host = Uuid::from_u128(42);
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
            Msg::Server(ServerMsg::HostUpserted { host: a_host(true) }),
            Msg::Server(ServerMsg::AgentUpserted {
                agent: Agent {
                    id: agent,
                    host_id: host,
                    name: Some("protocol-test".to_string()),
                    command: "test-agent".to_string(),
                    working_dir: "/work".into(),
                    kind: match protocol {
                        amux_ui::claude::PROTOCOL => amux_ui::AgentKind::Claude {
                            driver: amux_ui::ClaudeDriver::Pty,
                        },
                        amux_ui::codex::PROTOCOL => amux_ui::AgentKind::Codex,
                        _ => amux_ui::AgentKind::TestAgent,
                    },
                    readonly: false,
                    args: Vec::new(),
                    created_at: at(0),
                    parent: None,
                    working_on: None,
                },
            }),
        ] {
            update(&mut model, msg);
        }
        (model, agent)
    }

    fn idle_claude_model() -> (Model, AgentId) {
        let (mut model, agent) = model_with_protocol(amux_ui::claude::PROTOCOL);
        for event in [
            StreamMsg::Opened { truncated: false },
            StreamMsg::ReplayComplete,
        ] {
            update(&mut model, Msg::Stream { agent, event });
        }
        (model, agent)
    }

    fn claude_plan_reader_model() -> (Model, AgentId) {
        let (mut model, agent) = idle_claude_model();
        update(
            &mut model,
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: at(3),
                    entries: vec![
                        StreamEntry {
                            seq: 1,
                            payload: json!({"type": "amux.transcript_ready"}),
                        },
                        StreamEntry {
                            seq: 2,
                            payload: json!({
                                "type": "user",
                                "uuid": "dddddddd-0000-4000-8000-000000000001",
                                "sessionId": "22222222-2222-4222-8222-222222222222",
                                "timestamp": "2026-08-12T09:00:00.000Z",
                                "message": {"role": "user", "content": "make a plan"},
                                "origin": {"kind": "human"},
                                "promptSource": "typed"
                            }),
                        },
                        StreamEntry {
                            seq: 3,
                            payload: json!({
                                "type": "hook.permission_request",
                                "tool_name": "ExitPlanMode",
                                "tool_input": {"plan": "# plan\n\n- step"},
                                "permission_mode": "default"
                            }),
                        },
                    ],
                },
            },
        );
        (model, agent)
    }

    /// A Claude chat whose one prompt carries an image attachment, with
    /// the refs row that states its name and size.
    fn image_prompt_model() -> (Model, AgentId, amux_ui::DraftAttachment) {
        let (mut model, agent) = idle_claude_model();
        let image = amux_ui::DraftAttachment::from_bytes(
            amux_ui::ArtifactKind::Image,
            "screenshot.png",
            "image/png",
            vec![b'p'; 2048],
        );
        let element = amux_ui::format_mention(&amux_ui::Mention {
            kind: amux_ui::MentionKind::Image {
                id: image.id.clone(),
            },
            name: image.name.clone(),
            size: Some(image.size),
            path: None,
        });
        update(
            &mut model,
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: at(3),
                    entries: vec![
                        StreamEntry {
                            seq: 1,
                            payload: json!({"type": "amux.transcript_ready"}),
                        },
                        StreamEntry {
                            seq: 2,
                            payload: json!({
                                "type": "amux.attachments",
                                "input_id": null,
                                "refs": [{
                                    "id": image.id,
                                    "kind": "image",
                                    "name": image.name,
                                    "mime": image.mime,
                                    "size": image.size,
                                }],
                            }),
                        },
                        StreamEntry {
                            seq: 3,
                            payload: json!({
                                "type": "user",
                                "uuid": "dddddddd-0000-4000-8000-000000000001",
                                "sessionId": "22222222-2222-4222-8222-222222222222",
                                "timestamp": "2026-08-12T09:00:00.000Z",
                                "message": {"role": "user", "content": format!("look\n{element}")},
                                "origin": {"kind": "human"},
                                "promptSource": "typed"
                            }),
                        },
                    ],
                },
            },
        );
        (model, agent, image)
    }

    /// The fold chord opens what the focus is on. On an image row that
    /// means the host's viewer, through the runtime, which is the only
    /// place the bytes exist — the chat never holds them.
    #[test]
    fn the_fold_chord_opens_the_focused_image_attachment() {
        let (model, agent, image) = image_prompt_model();
        let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
        let ctx = FrameContext {
            viewport: (100, 30),
            theme: Theme::default(),
            now: at(0),
        };
        // Painting fills the metrics the focus moves through.
        build_chat_lines(&model, &chat, &ctx);

        // The newest block is the prompt's one attachment row.
        let leader = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        super::handle_chat_key(&mut chat, &model, leader, (100, 30), at(0));
        super::handle_chat_key(
            &mut chat,
            &model,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            (100, 30),
            at(0),
        );
        super::handle_chat_key(&mut chat, &model, leader, (100, 30), at(0));
        let action = super::handle_chat_key(
            &mut chat,
            &model,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            (100, 30),
            at(0),
        );
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::OpenAttachment {
                agent,
                id: image.id
            })),
            "the chord opened the focused image"
        );
    }

    fn seed_focus_cache(chat: &mut ChatView) {
        let block = |key, text: &str| PaintedBlock {
            key: BlockKey(key),
            lines: vec![Line::from(text.to_string())],
            copy_text: text.to_string(),
            run: None,
        };
        chat.feed_metrics.replace(Some(super::CachedFeedMetrics {
            viewport: (120, 40),
            metrics: super::frame::FeedMetrics {
                total_rows: 100,
                feed_rows: 20,
                max_top: 80,
                ranges: vec![
                    (BlockKey(1), Range { start: 0, end: 5 }),
                    (BlockKey(2), Range { start: 40, end: 45 }),
                    (BlockKey(3), Range { start: 90, end: 95 }),
                ],
            },
            blocks: vec![
                block(1, "oldest block"),
                block(2, "middle block"),
                block(3, "newest block"),
            ],
            following_geometry: super::frame::ChatGeometry {
                width: 120,
                height: 40,
                feed_top: 2,
                feed_rows: 21,
                bottom_top: 39,
            },
            paused_geometry: super::frame::ChatGeometry {
                width: 120,
                height: 40,
                feed_top: 2,
                feed_rows: 20,
                bottom_top: 39,
            },
        }));
    }

    fn press_chat(
        chat: &mut ChatView,
        model: &Model,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<UiAction> {
        super::handle_chat_key(
            chat,
            model,
            KeyEvent::new(code, modifiers),
            (120, 40),
            at(0),
        )
    }

    fn leader_chat(chat: &mut ChatView, model: &Model, key: char) -> Option<UiAction> {
        assert_eq!(
            press_chat(chat, model, KeyCode::Char('a'), KeyModifiers::CONTROL,),
            None
        );
        press_chat(chat, model, KeyCode::Char(key), KeyModifiers::NONE)
    }

    fn send_prompt(model: &mut Model, agent: AgentId, seconds: i64) {
        update(model, Msg::Tick { now: at(seconds) });
        update(
            model,
            Msg::Command {
                op: OpId(Uuid::from_u128(90)),
                command: Command::Claude(ClaudeCommand::SendPrompt {
                    agent,
                    text: "next task".to_string(),
                }),
            },
        );
    }

    #[test]
    fn known_protocols_dispatch_their_native_views() {
        let (claude, claude_agent) = model_with_protocol(amux_ui::claude::PROTOCOL);
        let claude =
            ChatView::open(&claude, claude_agent, 'a', false).expect("known Claude protocol opens");
        assert!(matches!(claude.inner, AgentChatView::Claude(_)));

        let (codex, codex_agent) = model_with_protocol(amux_ui::codex::PROTOCOL);
        let codex =
            ChatView::open(&codex, codex_agent, 'a', false).expect("known Codex protocol opens");
        assert!(matches!(codex.inner, AgentChatView::Codex(_)));
    }

    #[test]
    fn amux_send_summary_uses_the_first_non_empty_line_for_both_adapters() {
        assert_eq!(
            format_amux_send(
                Some("runner"),
                Some("\n  \n  rerun with --nocapture  \nignored")
            ),
            "→ runner · rerun with --nocapture"
        );
        assert_eq!(format_amux_send(Some("runner"), Some("\n  \n")), "→ runner");
        assert_eq!(format_amux_send(None, None), "→ an agent");
    }

    #[test]
    fn both_agents_route_paging_and_endpoints_through_the_shared_viewport() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = protocol.as_str();
            let (model, agent) = model_with_protocol(wire);
            let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            let ctx = FrameContext {
                viewport: (120, 40),
                theme: Theme::default(),
                now: at(0),
            };
            chat.feed_metrics.replace(Some(super::CachedFeedMetrics {
                viewport: ctx.viewport,
                metrics: super::frame::FeedMetrics {
                    total_rows: 100,
                    feed_rows: 20,
                    max_top: 80,
                    ranges: Vec::new(),
                },
                blocks: Vec::new(),
                following_geometry: super::frame::ChatGeometry {
                    width: 120,
                    height: 40,
                    feed_top: 2,
                    feed_rows: 21,
                    bottom_top: 39,
                },
                paused_geometry: super::frame::ChatGeometry {
                    width: 120,
                    height: 40,
                    feed_top: 2,
                    feed_rows: 20,
                    bottom_top: 39,
                },
            }));

            super::handle_chat_key(
                &mut chat,
                &model,
                KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
                ctx.viewport,
                ctx.now,
            );
            assert!(
                matches!(chat.viewport.scroll, FeedScroll::Paused { .. }),
                "{protocol:?} PgUp pauses"
            );

            super::handle_chat_key(
                &mut chat,
                &model,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                ctx.viewport,
                ctx.now,
            );
            assert_eq!(
                chat.viewport.scroll,
                FeedScroll::Following,
                "{protocol:?} PgDn at the bottom follows"
            );

            super::handle_chat_key(
                &mut chat,
                &model,
                KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
                ctx.viewport,
                ctx.now,
            );
            assert!(matches!(
                chat.viewport.scroll,
                FeedScroll::Paused { top_line: 0, .. }
            ));

            super::handle_chat_key(
                &mut chat,
                &model,
                KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
                ctx.viewport,
                ctx.now,
            );
            assert_eq!(chat.viewport.scroll, FeedScroll::Following);
        }
    }

    #[test]
    fn focus_chords_move_blocks_and_keep_them_visible_in_both_chats() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = protocol.as_str();
            let (model, agent) = model_with_protocol(wire);
            let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            seed_focus_cache(&mut chat);

            leader_chat(&mut chat, &model, 'k');
            assert_eq!(chat.viewport.focus, Some(BlockKey(3)), "{protocol:?}");
            assert_eq!(chat.viewport.scroll, FeedScroll::Following, "{protocol:?}");

            leader_chat(&mut chat, &model, 'k');
            assert_eq!(chat.viewport.focus, Some(BlockKey(2)), "{protocol:?}");
            assert!(matches!(
                chat.viewport.scroll,
                FeedScroll::Paused { top_line: 40, .. }
            ));

            press_chat(&mut chat, &model, KeyCode::Up, KeyModifiers::CONTROL);
            assert_eq!(chat.viewport.focus, Some(BlockKey(1)), "{protocol:?}");
            assert!(matches!(
                chat.viewport.scroll,
                FeedScroll::Paused { top_line: 0, .. }
            ));

            press_chat(&mut chat, &model, KeyCode::Down, KeyModifiers::CONTROL);
            assert_eq!(chat.viewport.focus, Some(BlockKey(2)), "{protocol:?}");
            let FeedScroll::Paused { top_line, .. } = chat.viewport.scroll else {
                panic!("{protocol:?} focus stays paused away from the newest rows");
            };
            assert!(
                top_line <= 40 && 45 <= top_line + 20,
                "focused block is visible"
            );

            press_chat(&mut chat, &model, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(chat.viewport.focus, None, "{protocol:?} Esc clears focus");
        }
    }

    #[test]
    fn native_help_overlays_take_shared_focus_keys_in_both_chats() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = protocol.as_str();
            let (model, agent) = model_with_protocol(wire);
            let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            seed_focus_cache(&mut chat);

            press_chat(&mut chat, &model, KeyCode::Char('?'), KeyModifiers::NONE);
            assert!(chat.overlay_open(), "{protocol:?} help opens");
            press_chat(&mut chat, &model, KeyCode::Up, KeyModifiers::CONTROL);
            assert!(
                !chat.overlay_open(),
                "{protocol:?} help consumes Ctrl+Up and closes"
            );
            assert_eq!(
                chat.viewport.focus, None,
                "{protocol:?} help keeps focus movement behind the overlay"
            );

            chat.viewport.focus = Some(BlockKey(2));
            press_chat(&mut chat, &model, KeyCode::Char('?'), KeyModifiers::NONE);
            assert!(chat.overlay_open(), "{protocol:?} help reopens");
            press_chat(&mut chat, &model, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!chat.overlay_open(), "{protocol:?} Esc closes help");
            assert_eq!(
                chat.viewport.focus,
                Some(BlockKey(2)),
                "{protocol:?} Esc leaves the covered block focus intact"
            );
        }
    }

    #[test]
    fn claude_reader_takes_esc_before_shared_block_focus() {
        let (model, agent) = claude_plan_reader_model();
        let mut chat = ChatView::open(&model, agent, 'a', false).expect("Claude chat opens");
        chat.reconcile(&model);
        assert!(chat.overlay_open(), "the pending plan opens its reader");
        chat.viewport.focus = Some(BlockKey(2));

        press_chat(&mut chat, &model, KeyCode::Esc, KeyModifiers::NONE);

        assert!(!chat.overlay_open(), "Esc closes the reader");
        assert_eq!(
            chat.viewport.focus,
            Some(BlockKey(2)),
            "the reader consumes Esc before shared focus clearing"
        );
    }

    #[test]
    fn focus_copy_uses_the_focused_block_or_the_newest_block_in_both_chats() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = protocol.as_str();
            let (model, agent) = model_with_protocol(wire);
            let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            seed_focus_cache(&mut chat);

            assert_eq!(
                leader_chat(&mut chat, &model, 'y'),
                Some(UiAction::CopyToClipboard("newest block".to_string())),
                "{protocol:?} copies newest when focus is absent"
            );

            chat.viewport.focus = Some(BlockKey(1));
            assert_eq!(
                leader_chat(&mut chat, &model, 'y'),
                Some(UiAction::CopyToClipboard("oldest block".to_string())),
                "{protocol:?} copies focused block"
            );
        }
    }

    #[test]
    fn leader_o_toggles_the_focused_exploration_run() {
        let (model, agent) = model_with_protocol(amux_ui::claude::PROTOCOL);
        let mut chat = ChatView::open(&model, agent, 'a', false).expect("Claude chat opens");
        seed_focus_cache(&mut chat);
        let run = RunKey(2);
        chat.feed_metrics
            .get_mut()
            .as_mut()
            .expect("seeded cache")
            .blocks[1]
            .run = Some(run);
        chat.viewport.focus = Some(BlockKey(2));

        assert_eq!(leader_chat(&mut chat, &model, 'o'), None);
        assert_eq!(
            chat.viewport.expanded,
            std::collections::BTreeSet::from([run])
        );

        assert_eq!(leader_chat(&mut chat, &model, 'o'), None);
        assert!(chat.viewport.expanded.is_empty());
    }

    /// The running program hands wheel events to `handle_chat_mouse`, so
    /// the review page only scrolls under a mouse if that entry point knows
    /// about it — reaching into the page's own wheel handler would prove
    /// nothing about the program.
    #[test]
    fn mouse_wheel_scrolls_the_open_review_page_without_moving_its_cursor() {
        let (model, agent) = model_with_protocol(StructuredProtocol::Claude.as_str());
        let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
        let AgentChatView::Claude(view) = &mut chat.inner else {
            panic!("a Claude chat");
        };
        view.review = Some(Box::new(crate::chat::claude::draft::ReviewDraft::opened(
            crate::review::fixture::sample_review(),
        )));

        let wheel = |kind| MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        fn page(chat: &mut ChatView) -> &mut crate::review::ReviewView {
            let AgentChatView::Claude(view) = &mut chat.inner else {
                panic!("a Claude chat");
            };
            view.open_review_mut().expect("the page is open")
        }
        let cursor = page(&mut chat).cursor();

        // At the top there is nothing above to reveal, so the frame is
        // unchanged and the program has no reason to redraw.
        assert!(!super::handle_chat_mouse(
            &mut chat,
            &model,
            wheel(MouseEventKind::ScrollUp),
            (80, 12),
        ));
        assert_eq!(page(&mut chat).scroll(), 0);

        assert!(super::handle_chat_mouse(
            &mut chat,
            &model,
            wheel(MouseEventKind::ScrollDown),
            (80, 12),
        ));
        assert_eq!(page(&mut chat).scroll(), 3, "one notch is three rows");
        assert_eq!(
            page(&mut chat).cursor(),
            cursor,
            "the wheel scrolls the body, it does not move the cursor"
        );

        assert!(super::handle_chat_mouse(
            &mut chat,
            &model,
            wheel(MouseEventKind::ScrollUp),
            (80, 12),
        ));
        assert_eq!(page(&mut chat).scroll(), 0);
        // The feed underneath never moved while the page had the frame.
        assert_eq!(chat.viewport.scroll, FeedScroll::Following);
    }

    #[test]
    fn mouse_wheel_routes_three_rows_through_both_chat_viewports() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = protocol.as_str();
            let (model, agent) = model_with_protocol(wire);
            let mut chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            chat.feed_metrics.replace(Some(super::CachedFeedMetrics {
                viewport: (120, 40),
                metrics: super::frame::FeedMetrics {
                    total_rows: 100,
                    feed_rows: 20,
                    max_top: 80,
                    ranges: Vec::new(),
                },
                blocks: Vec::new(),
                following_geometry: super::frame::ChatGeometry {
                    width: 120,
                    height: 40,
                    feed_top: 2,
                    feed_rows: 21,
                    bottom_top: 39,
                },
                paused_geometry: super::frame::ChatGeometry {
                    width: 120,
                    height: 40,
                    feed_top: 2,
                    feed_rows: 20,
                    bottom_top: 39,
                },
            }));
            let paint_stats = chat.paint_cache.borrow().stats();
            let event = |kind, row| MouseEvent {
                kind,
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            };

            assert!(!super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::ScrollUp, 0),
                (120, 40),
            ));
            assert!(!super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::Down(MouseButton::Left), 5),
                (120, 40),
            ));
            assert!(!super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::Drag(MouseButton::Left), 5),
                (120, 40),
            ));

            assert!(super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::ScrollUp, 5),
                (120, 40),
            ));
            assert!(matches!(
                chat.viewport.scroll,
                FeedScroll::Paused { top_line: 77, .. }
            ));
            assert_eq!(chat.paint_cache.borrow().stats(), paint_stats);
            assert!(super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::ScrollDown, 5),
                (120, 40),
            ));
            assert_eq!(chat.viewport.scroll, FeedScroll::Following);
            assert!(!super::handle_chat_mouse(
                &mut chat,
                &model,
                event(MouseEventKind::ScrollDown, 5),
                (120, 40),
            ));
        }
    }

    #[test]
    fn fabricated_protocol_keeps_the_fleet_card_and_neutral_watermark() {
        let (model, agent) = model_with_protocol("fabricated_structured_v1");
        assert!(
            model.agent(agent).is_some(),
            "inventory card remains present"
        );

        let mut view = ViewState::default();
        view.open_chat(&model, agent);

        assert!(view.chat.is_none(), "the fleet remains the active view");
        assert_eq!(visible_rows(&model, &view).len(), 1, "card stays visible");
        assert_eq!(entry_watermark(&model, agent), 0);
    }

    #[test]
    fn claude_chat_ticks_for_a_fresh_idle_echo_then_stops_when_it_ages_out() {
        let (mut model, agent) = idle_claude_model();
        send_prompt(&mut model, agent, 100);
        let chat = ChatView::open(&model, agent, 'a', false).expect("Claude chat");

        assert!(matches!(
            amux_ui::claude::phase(&model, agent),
            amux_ui::claude::ChatPhase::Idle { .. }
        ));
        assert_eq!(
            model.effective_attention(model.agent(agent).expect("agent card")),
            Attention::Working
        );
        assert!(
            chat.needs_tick(&model),
            "a fresh echo over an idle phase must keep advancing observation time"
        );

        update(
            &mut model,
            Msg::Tick {
                now: at(100) + TimeDelta::seconds(601),
            },
        );
        assert_eq!(
            model.effective_attention(model.agent(agent).expect("agent card")),
            Attention::Unknown
        );
        assert_eq!(
            amux_ui::claude::send_gate(&model, agent),
            SendGate::SendInFlight
        );
        assert!(
            !chat.needs_tick(&model),
            "an aged echo keeps the safety gate closed without repainting forever"
        );
    }

    #[test]
    fn claude_chat_keeps_ordinary_working_phase_ticking() {
        let (mut model, agent) = idle_claude_model();
        update(
            &mut model,
            Msg::Stream {
                agent,
                event: StreamMsg::Batch {
                    at: at(10),
                    entries: vec![StreamEntry {
                        seq: 1,
                        payload: json!({
                            "type": "user",
                            "uuid": "dddddddd-0000-4000-8000-000000000001",
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-11T22:00:00.000Z",
                            "message": {"role": "user", "content": "do the thing"},
                            "origin": {"kind": "human"},
                            "promptSource": "typed"
                        }),
                    }],
                },
            },
        );
        let chat = ChatView::open(&model, agent, 'a', false).expect("Claude chat");

        assert!(matches!(
            amux_ui::claude::phase(&model, agent),
            amux_ui::claude::ChatPhase::Working
        ));
        assert!(chat.needs_tick(&model));
    }

    /// The kernel's own consistency warning is a chat row, not a fleet
    /// row: it wears the chat's grid, reaches both edges on the
    /// background, and carries no border glyph the full-screen frame
    /// would otherwise leave stranded in column 0 and the last cell.
    #[test]
    fn the_chat_draws_its_invariant_warning_on_the_chat_grid() {
        let theme = Theme::default();
        for protocol in [amux_ui::claude::PROTOCOL, amux_ui::codex::PROTOCOL] {
            let (model, agent) = model_with_protocol(protocol);
            // The runtime setter is crate-private on purpose — a renderer
            // only reads this fact — so serde supplies the failing model.
            let mut value = serde_json::to_value(&model).expect("serialize model");
            value["invariant_warning"] = serde_json::Value::Bool(true);
            let model: Model = serde_json::from_value(value).expect("deserialize model");
            let chat = ChatView::open(&model, agent, 'a', false).expect("chat opens");
            let ctx = FrameContext {
                viewport: (100, 30),
                theme,
                now: at(0),
            };

            let lines = build_chat_lines(&model, &chat, &ctx);
            let warning = lines
                .iter()
                .find(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                        .contains(INVARIANT_WARNING)
                })
                .unwrap_or_else(|| panic!("{protocol} chat lost its invariant warning"));

            let text = warning
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(
                text.starts_with(&format!("{}⚠", " ".repeat(crate::chat::blocks::GLYPH_COL))),
                "{protocol} warning does not start on the chat's glyph column: {text:?}"
            );
            assert_eq!(
                str_width(&text),
                100,
                "{protocol} warning does not fill the frame: {text:?}"
            );

            let classes = warning
                .spans
                .iter()
                .flat_map(|span| {
                    std::iter::repeat_n(theme.classify(span.style), str_width(&span.content))
                })
                .collect::<String>();
            assert!(
                !classes.contains('?'),
                "{protocol} warning paints an unnamed style: {classes}"
            );
        }
    }

    #[test]
    fn offline_pending_echo_does_not_keep_claude_chat_ticking() {
        let (mut model, agent) = idle_claude_model();
        send_prompt(&mut model, agent, 100);
        update(
            &mut model,
            Msg::Server(ServerMsg::HostUpserted {
                host: a_host(false),
            }),
        );
        let chat = ChatView::open(&model, agent, 'a', false).expect("Claude chat");

        assert_eq!(
            model.effective_attention(model.agent(agent).expect("agent card")),
            Attention::Unknown
        );
        assert_eq!(
            amux_ui::claude::send_gate(&model, agent),
            SendGate::SendInFlight
        );
        assert!(!chat.needs_tick(&model));
    }
}
