//! Structured chat dispatch. The outer view owns exactly one native
//! per-agent view; Claude and Codex keep their content, panels, and key
//! semantics separate while sharing only proven terminal renderers.

pub(crate) mod blocks;
pub(crate) mod claude;
mod codex;
pub mod diff;
pub(crate) mod frame;
pub(crate) mod inline;
pub(crate) mod viewport;

use std::cell::RefCell;

use amux_ui::{
    AgentId, AgentMessageKind, AgentMessagePresentation, Command, FamilyNeed, Model, OpId,
    StructuredProtocol, Why, message_digest,
};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
pub use frame::PaintStats;
use frame::{
    FeedMetrics, FrameSpacing, PaintCache, PaintedBlock, compose_chat_frame, feed_metrics,
};
use ratatui::text::Line;
use viewport::{FeedViewport, apply_scroll, move_focus, toggle_focused_run};

use crate::composer::Composer;
use crate::render::{FrameContext, Theme};
use crate::view::{QuitGuard, UiAction};

/// Feed scroll state shared because both native screens have the same
/// sticky-bottom terminal interaction, not because their feed entries share
/// a representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedScroll {
    Following,
    Paused {
        top_line: usize,
        entry_watermark: u64,
    },
}

#[derive(Clone, Debug)]
enum AgentChatView {
    Claude(claude::View),
    Codex(codex::View),
}

#[derive(Clone, Debug)]
struct CachedFeedMetrics {
    viewport: (u16, u16),
    metrics: FeedMetrics,
    blocks: Vec<PaintedBlock>,
}

/// Renderer-local state for one structured chat. Native sub-state remains
/// namespaced; dispatch is exhaustive at this one additive seam.
#[derive(Debug)]
pub struct ChatView {
    pub agent: AgentId,
    pub(crate) viewport: FeedViewport,
    inner: AgentChatView,
    /// Metrics from the adapter blocks painted for the latest frame. Key
    /// handling consumes this instead of walking and painting the feed a
    /// second time merely to discover its scroll bounds.
    feed_metrics: RefCell<Option<CachedFeedMetrics>>,
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

impl ChatView {
    pub fn open(model: &Model, agent: AgentId, leader: char, kitty: bool) -> Option<Self> {
        let protocol = model.agent(agent)?.structured_protocol()?;
        let inner = match protocol {
            StructuredProtocol::Claude => {
                AgentChatView::Claude(claude::View::open(agent, leader, kitty))
            }
            StructuredProtocol::Codex => {
                AgentChatView::Codex(codex::View::open(agent, leader, kitty))
            }
        };
        Some(Self {
            agent,
            viewport: FeedViewport::following(),
            inner,
            feed_metrics: RefCell::new(None),
            paint_cache: RefCell::new(PaintCache::default()),
        })
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

    pub fn composer_mut(&mut self) -> &mut Composer {
        match &mut self.inner {
            AgentChatView::Claude(view) => &mut view.composer,
            AgentChatView::Codex(view) => &mut view.composer,
        }
    }

    pub fn quit_guard_mut(&mut self) -> &mut QuitGuard {
        match &mut self.inner {
            AgentChatView::Claude(view) => &mut view.quit_guard,
            AgentChatView::Codex(view) => &mut view.quit_guard,
        }
    }

    pub fn set_help(&mut self, help: bool) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.help = help,
            AgentChatView::Codex(view) => view.help = help,
        }
    }

    pub fn set_kitty(&mut self, kitty: bool) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.kitty = kitty,
            AgentChatView::Codex(view) => view.kitty = kitty,
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
        }
    }

    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
        self.feed_metrics.get_mut().take();
        match &mut self.inner {
            AgentChatView::Claude(view) => view.note_dispatched(op, command),
            AgentChatView::Codex(view) => view.note_dispatched(op, command),
        }
    }

    pub fn needs_tick(&self, model: &Model) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.needs_tick(model),
            AgentChatView::Codex(view) => view.needs_tick(model),
        }
    }

    pub fn expire_quit_guard(&mut self, now: DateTime<Utc>) -> bool {
        self.quit_guard_mut().expire(now)
    }

    fn metrics_for(&self, model: &Model, viewport: (u16, u16), now: DateTime<Utc>) -> FeedMetrics {
        if let Some(cached) = self.feed_metrics.borrow().as_ref()
            && cached.viewport == viewport
        {
            return cached.metrics.clone();
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
        let parts = match &self.inner {
            AgentChatView::Claude(view) => {
                let mut cache = self.paint_cache.borrow_mut();
                claude::claude_frame_parts(model, view, &self.viewport, &mut cache, &ctx)
            }
            AgentChatView::Codex(view) => {
                let mut cache = self.paint_cache.borrow_mut();
                codex::codex_frame_parts(model, view, &self.viewport, &mut cache, &ctx)
            }
        };
        let geometry = match &self.inner {
            AgentChatView::Claude(view) => claude::geometry(model, view, viewport, true),
            AgentChatView::Codex(view) => codex::geometry(model, view, viewport, true),
        };
        let metrics = feed_metrics(&parts.feed, FrameSpacing::DEFAULT, &geometry);
        self.feed_metrics.replace(Some(CachedFeedMetrics {
            viewport,
            metrics: metrics.clone(),
            blocks: parts.feed.blocks,
        }));
        metrics
    }

    fn pending_leader(&self) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.pending_leader,
            AgentChatView::Codex(view) => view.pending_leader,
        }
    }

    fn overlay_open(&self) -> bool {
        match &self.inner {
            AgentChatView::Claude(view) => view.overlay_open(),
            AgentChatView::Codex(view) => view.overlay_open(),
        }
    }

    fn consume_shared_leader(&mut self) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.pending_leader = false,
            AgentChatView::Codex(view) => view.pending_leader = false,
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
    };
    let intent = match &mut chat.inner {
        AgentChatView::Claude(view) => view.scroll_intent.take(),
        AgentChatView::Codex(view) => view.scroll_intent.take(),
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

pub fn handle_chat_paste(chat: &mut ChatView, model: &Model, text: &str) {
    match &mut chat.inner {
        AgentChatView::Claude(view) => claude::handle_chat_paste(view, model, text),
        AgentChatView::Codex(view) => codex::handle_chat_paste(view, model, text),
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
    let intent = match event.kind {
        MouseEventKind::ScrollUp => viewport::ScrollIntent::Rows(-3),
        MouseEventKind::ScrollDown => viewport::ScrollIntent::Rows(3),
        _ => return false,
    };
    let geometry = match &chat.inner {
        AgentChatView::Claude(view) => claude::geometry(
            model,
            view,
            size,
            matches!(chat.viewport.scroll, FeedScroll::Paused { .. }),
        ),
        AgentChatView::Codex(view) => codex::geometry(
            model,
            view,
            size,
            matches!(chat.viewport.scroll, FeedScroll::Paused { .. }),
        ),
    };
    let row = event.row as usize;
    if chat.overlay_open()
        || row < geometry.feed_top
        || row >= geometry.feed_top.saturating_add(geometry.feed_rows)
    {
        return false;
    }

    let metrics = chat.metrics_for(model, size, Utc::now());
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
    let parts = match &chat.inner {
        AgentChatView::Claude(view) => {
            claude::claude_frame_parts(model, view, &chat.viewport, &mut cache, ctx)
        }
        AgentChatView::Codex(view) => {
            codex::codex_frame_parts(model, view, &chat.viewport, &mut cache, ctx)
        }
    };
    drop(cache);
    let overlaid = parts.overlay.is_some();
    let banner = parts.banner.is_some();
    let paused_geometry = match &chat.inner {
        AgentChatView::Claude(view) => claude::geometry(model, view, ctx.viewport, true),
        AgentChatView::Codex(view) => codex::geometry(model, view, ctx.viewport, true),
    };
    let metrics = feed_metrics(&parts.feed, FrameSpacing::DEFAULT, &paused_geometry);
    let blocks = parts.feed.blocks.clone();
    chat.feed_metrics.replace(Some(CachedFeedMetrics {
        viewport: ctx.viewport,
        metrics,
        blocks,
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
        sender_marker(self.model, self.agent, from)
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
/// host only when it is not this agent's own. The wire carries
/// `name/<host uuid>` because that pair is the address a reply is sent
/// to; a chat row is for a person, and a person reading their own
/// machine's name in every row learns nothing from it.
///
/// A host this inventory cannot name is left exactly as it arrived. An
/// address nobody here can resolve is still the truth about where the
/// message came from, and shortening it to the half we recognise would
/// be inventing agreement.
pub(crate) fn sender_marker(model: &Model, agent: AgentId, from: &str) -> String {
    let Some((name, host)) = from.rsplit_once('/') else {
        return from.to_string();
    };
    let Ok(host) = host.parse::<amux_ui::HostId>() else {
        return from.to_string();
    };
    if model
        .agent(agent)
        .is_some_and(|card| card.agent.host_id == host)
    {
        return name.to_string();
    }
    match model.host_name(host) {
        Some(host_name) => format!("{name} @ {host_name}"),
        None => from.to_string(),
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
    let closable = |kind: &AgentMessageKind, text: &str| {
        kind.presentation() == AgentMessagePresentation::Finished
            && message_digest(text).hidden_lines > 0
    };
    match model
        .agent(agent)
        .and_then(amux_ui::AgentCard::structured_protocol)
    {
        Some(StructuredProtocol::Claude) => model.claude(agent).is_some_and(|layer| {
            layer.entries().any(|entry| match &entry.kind {
                amux_ui::claude::FeedEntryKind::AgentMessage(message) => {
                    closable(&message.kind, &message.text)
                }
                _ => false,
            })
        }),
        Some(StructuredProtocol::Codex) => model.codex(agent).is_some_and(|layer| {
            layer.entries().any(|entry| match &entry.kind {
                amux_ui::codex::FeedEntryKind::AgentMessage(message) => {
                    closable(&message.kind, &message.text)
                }
                _ => false,
            })
        }),
        None => false,
    }
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

pub fn entry_watermark(model: &Model, agent: AgentId) -> u64 {
    match model
        .agent(agent)
        .and_then(amux_ui::AgentCard::structured_protocol)
    {
        Some(StructuredProtocol::Claude) => claude::entry_watermark(model, agent),
        Some(StructuredProtocol::Codex) => model.codex(agent).map_or(0, |layer| {
            layer.evicted_entries() + layer.entry_count() as u64
        }),
        None => 0,
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

    use super::{AgentChatView, ChatView, FeedScroll, build_chat_lines, entry_watermark};
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
                    agent_type: "test-agent".to_string(),
                    io_protocols: vec![protocol.to_string()],
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
    fn both_agents_route_paging_and_endpoints_through_the_shared_viewport() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = match protocol {
                StructuredProtocol::Claude => amux_ui::claude::PROTOCOL,
                StructuredProtocol::Codex => amux_ui::codex::PROTOCOL,
            };
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
            let wire = match protocol {
                StructuredProtocol::Claude => amux_ui::claude::PROTOCOL,
                StructuredProtocol::Codex => amux_ui::codex::PROTOCOL,
            };
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
            let wire = match protocol {
                StructuredProtocol::Claude => amux_ui::claude::PROTOCOL,
                StructuredProtocol::Codex => amux_ui::codex::PROTOCOL,
            };
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
            let wire = match protocol {
                StructuredProtocol::Claude => amux_ui::claude::PROTOCOL,
                StructuredProtocol::Codex => amux_ui::codex::PROTOCOL,
            };
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

    #[test]
    fn mouse_wheel_routes_three_rows_through_both_chat_viewports() {
        for protocol in [StructuredProtocol::Claude, StructuredProtocol::Codex] {
            let wire = match protocol {
                StructuredProtocol::Claude => amux_ui::claude::PROTOCOL,
                StructuredProtocol::Codex => amux_ui::codex::PROTOCOL,
            };
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
            }));
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
