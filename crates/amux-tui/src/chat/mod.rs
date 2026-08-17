//! Structured chat dispatch. The outer view owns exactly one native
//! per-agent view; Claude and Codex keep their content, panels, and key
//! semantics separate while sharing only proven terminal renderers.

pub(crate) mod claude;
mod codex;
mod layout;

pub use claude::diff;

use amux_ui::{AgentId, Command, Model, OpId, StructuredProtocol};
use chrono::{DateTime, Utc};
use crossterm::event::KeyEvent;
use ratatui::text::Line;

use crate::composer::Composer;
use crate::render::FrameContext;
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

/// Renderer-local state for one structured chat. Native sub-state remains
/// namespaced; dispatch is exhaustive at this one additive seam.
#[derive(Clone, Debug)]
pub struct ChatView {
    pub agent: AgentId,
    inner: AgentChatView,
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
        Some(Self { agent, inner })
    }

    /// Deterministic constructors used by pure golden fixtures.
    pub fn open_claude(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            inner: AgentChatView::Claude(claude::View::open(agent, leader, kitty)),
        }
    }

    pub fn open_codex(agent: AgentId, leader: char, kitty: bool) -> Self {
        Self {
            agent,
            inner: AgentChatView::Codex(codex::View::open(agent, leader, kitty)),
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
        match &mut self.inner {
            AgentChatView::Claude(view) => view.scroll = scroll,
            AgentChatView::Codex(view) => view.scroll = scroll,
        }
    }

    pub fn set_codex_configuration_label(&mut self, label: Option<String>) {
        if let AgentChatView::Codex(view) = &mut self.inner {
            view.configuration_label = label;
        }
    }

    pub fn reconcile(&mut self, model: &Model) {
        match &mut self.inner {
            AgentChatView::Claude(view) => view.reconcile(model),
            AgentChatView::Codex(view) => view.reconcile(model),
        }
    }

    pub fn note_dispatched(&mut self, op: OpId, command: &Command) {
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
}

pub fn handle_chat_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
    now: DateTime<Utc>,
) -> Option<UiAction> {
    match &mut chat.inner {
        AgentChatView::Claude(view) => claude::handle_chat_key(view, model, key, viewport, now),
        AgentChatView::Codex(view) => codex::handle_chat_key(view, model, key, viewport, now),
    }
}

pub fn handle_chat_paste(chat: &mut ChatView, model: &Model, text: &str) {
    match &mut chat.inner {
        AgentChatView::Claude(view) => claude::handle_chat_paste(view, model, text),
        AgentChatView::Codex(view) => codex::handle_chat_paste(view, model, text),
    }
}

pub(crate) fn build_chat_lines(
    model: &Model,
    chat: &ChatView,
    ctx: &FrameContext,
) -> Vec<Line<'static>> {
    match &chat.inner {
        AgentChatView::Claude(view) => claude::build_chat_lines(model, view, ctx),
        AgentChatView::Codex(view) => codex::build_chat_lines(model, view, ctx),
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
    use amux_ui::{Agent, AgentId, Model, Msg, ServerMsg, update};
    use chrono::DateTime;
    use uuid::Uuid;

    use super::{AgentChatView, ChatView, entry_watermark};
    use crate::view::{ViewState, visible_rows};

    fn model_with_protocol(protocol: &str) -> (Model, AgentId) {
        let agent = Uuid::from_u128(41);
        let host = Uuid::from_u128(42);
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host),
            }),
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
                    created_at: DateTime::from_timestamp(1_754_697_600, 0)
                        .expect("fixture timestamp"),
                },
            }),
        ] {
            update(&mut model, msg);
        }
        (model, agent)
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
}
