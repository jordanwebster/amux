//! The terminal client's model: the fleet driver, at most one open chat,
//! the views' state, and the keys that move between them. Effects run as
//! tasks against the drivers and report back as [`AppEvent`]s, so the
//! loop never waits on the network with a key in hand.

use std::sync::Arc;
use std::time::{Duration, Instant};

use client::{Client, SystemClock};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use ui_runtime::{Fleet, InputError, Session, inputs};
use ui_state::{AgentKey, InputOutcome, PhaseView};
use ui_view::family_header;
use wire::{
    Agent, Attachment, CreateAgentRequest, DeleteAgentRequest, RenameAgentRequest,
    SendInputResponse, StopAgentRequest, StopMode, send_input_response,
};

use crate::chat::layout::PAGE;
use crate::chat::{ChatEffect, ChatView};
use crate::clipboard::read_clipboard;
use crate::fleet::{FleetEffect, FleetView};
use crate::text::push;
use crate::theme::Theme;

/// How long a second Ctrl+C has to arrive to quit.
const QUIT_WINDOW: Duration = Duration::from_secs(3);
/// How long a notice stays in the footer.
const NOTICE_FOR: Duration = Duration::from_secs(5);

/// What the terminal client is configured with at the CLI edge; the TUI
/// itself reads no environment.
#[derive(Clone, Debug)]
pub struct TuiConfig {
    /// Where agents created from the fleet work.
    pub working_dir: std::path::PathBuf,
    /// The leader's letter: `a` makes Ctrl+A the leader.
    pub leader: char,
    pub theme: Theme,
    /// A chat to open once its inventory row arrives.
    pub initial_chat: Option<Vec<u8>>,
    /// Whether the embedding CLI can hand the terminal to an agent's own
    /// interface.
    pub attach: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
}

/// What a finished task reports.
pub enum AppEvent {
    Opened {
        agent: AgentKey,
        result: Result<Session, String>,
    },
    Notice(String, Tone),
    /// Put words back in the composer of this agent's chat, saying why
    /// when there is something to say.
    Restore {
        agent_id: Vec<u8>,
        text: String,
        attachments: Vec<Attachment>,
        notice: Option<String>,
    },
    Attached {
        agent_id: Vec<u8>,
        blob: wire::BlobRef,
    },
    Created(Agent),
}

/// What the loop does after a key.
#[derive(Debug, PartialEq)]
pub enum Flow {
    Continue,
    Quit,
    /// Hand the terminal to this agent's own interface.
    Attach(Box<Agent>),
}

pub struct OpenChat {
    pub session: Arc<Session>,
    pub view: ChatView,
    pub agent: AgentKey,
}

pub struct App {
    client: Arc<dyn Client>,
    pub fleet: Fleet,
    pub fleet_view: FleetView,
    pub chat: Option<OpenChat>,
    pub config: TuiConfig,
    leader_pending: bool,
    quit_armed: Option<Instant>,
    help: bool,
    notice: Option<(String, Tone, Instant)>,
    opening: Option<AgentKey>,
    events: mpsc::UnboundedSender<AppEvent>,
    pub receiver: mpsc::UnboundedReceiver<AppEvent>,
}

fn now_ms() -> i64 {
    use client::Clock as _;
    SystemClock.now_ms()
}

fn plain(error: InputError) -> String {
    match error {
        InputError::Rejected(reason) => format!("not sent: {reason}"),
        InputError::Uncertain => error.to_string(),
    }
}

impl App {
    pub fn new(client: Arc<dyn Client>, fleet: Fleet, config: TuiConfig) -> App {
        let (events, receiver) = mpsc::unbounded_channel();
        let mut fleet_view = FleetView::default();
        fleet_view.attach = config.attach;
        App {
            client,
            fleet,
            fleet_view,
            chat: None,
            config,
            leader_pending: false,
            quit_armed: None,
            help: false,
            notice: None,
            opening: None,
            events,
            receiver,
        }
    }

    fn theme(&self) -> Theme {
        self.config.theme
    }

    pub fn notice(&mut self, words: impl Into<String>, tone: Tone) {
        self.notice = Some((words.into(), tone, Instant::now()));
    }

    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = Option<AppEvent>> + Send + 'static,
    {
        let events = self.events.clone();
        tokio::spawn(async move {
            if let Some(event) = task.await {
                let _ = events.send(event);
            }
        });
    }

    /// When the loop should wake with nothing else happening: the activity
    /// line's second, the quit guard's end, a notice's end.
    pub fn next_tick(&self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut at = |instant: Instant| {
            next = Some(next.map_or(instant, |n| n.min(instant)));
        };
        if let Some(armed) = self.quit_armed {
            at(armed + QUIT_WINDOW);
        }
        if let Some((_, _, since)) = &self.notice {
            at(*since + NOTICE_FOR);
        }
        if let Some(chat) = &self.chat {
            let state = chat.session.state();
            if state.phase() == PhaseView::Working || state.transcript().is_empty() {
                at(Instant::now() + Duration::from_millis(250));
            }
        }
        next
    }

    /// Drops what has expired; true when that changed the screen.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if self
            .quit_armed
            .is_some_and(|armed| armed.elapsed() >= QUIT_WINDOW)
        {
            self.quit_armed = None;
            changed = true;
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, _, since)| since.elapsed() >= NOTICE_FOR)
        {
            self.notice = None;
            changed = true;
        }
        changed || self.chat.is_some()
    }

    /// Opens the configured first chat once its row is in the fleet, and
    /// keeps the open chat's entry and host current from the inventory.
    pub fn fleet_changed(&mut self) {
        let changed = self.fleet.take_changed();
        if let Some(id) = self.config.initial_chat.clone() {
            let agent = self.fleet.state().find(&id).map(ui_state::agent_key);
            if let Some(agent) = agent {
                self.config.initial_chat = None;
                self.open(agent);
            }
        }
        let Some(chat) = &self.chat else {
            return;
        };
        let (entry, host) = {
            let fleet = self.fleet.state();
            let entry = fleet.agent(&chat.agent).cloned();
            let host = entry
                .as_ref()
                .and_then(|agent| fleet.host(&agent.host_id).cloned());
            (entry, host)
        };
        if let Some(entry) = entry.filter(|_| changed.contains(&chat.agent)) {
            chat.session.set_entry(entry);
        }
        if let Some(host) = host {
            let same = chat.session.state().host() == Some(&host);
            if !same {
                chat.session.set_host(host);
            }
        }
    }

    pub fn event(&mut self, event: AppEvent) -> Flow {
        match event {
            AppEvent::Opened { agent, result } => {
                if self.opening.as_ref() != Some(&agent) {
                    return Flow::Continue;
                }
                self.opening = None;
                match result {
                    Ok(session) => {
                        let session = Arc::new(session);
                        let host = {
                            let fleet = self.fleet.state();
                            fleet
                                .agent(&agent)
                                .and_then(|entry| fleet.host(&entry.host_id).cloned())
                        };
                        if let Some(host) = host {
                            session.set_host(host);
                        }
                        let view = ChatView::new(agent.agent.clone(), now_ms(), self.config.attach);
                        self.fleet_view.select(agent.clone());
                        self.chat = Some(OpenChat {
                            session,
                            view,
                            agent,
                        });
                    }
                    Err(error) => {
                        self.notice(format!("could not open the chat: {error}"), Tone::Warn)
                    }
                }
            }
            AppEvent::Notice(words, tone) => self.notice(words, tone),
            AppEvent::Restore {
                agent_id,
                text,
                attachments,
                notice,
            } => {
                if let Some(notice) = notice {
                    self.notice(notice, Tone::Warn);
                }
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.editor.restore(&text, attachments);
                }
            }
            AppEvent::Attached { agent_id, blob } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.attach_blob(blob);
                }
            }
            AppEvent::Created(agent) => {
                let key = ui_state::agent_key(&agent);
                self.fleet_view.select(key.clone());
                self.open(key);
            }
        }
        Flow::Continue
    }

    fn open(&mut self, agent: AgentKey) {
        let Some(entry) = self.fleet.state().agent(&agent).cloned() else {
            return;
        };
        self.opening = Some(agent.clone());
        let client = self.client.clone();
        self.spawn(async move {
            let result = Session::open(client, entry, PAGE, SystemClock)
                .await
                .map_err(|error| error.to_string());
            Some(AppEvent::Opened { agent, result })
        });
    }

    /// Back to the fleet: the chat's session closes; the fleet is home.
    pub fn close_chat(&mut self) {
        if let Some(chat) = self.chat.take() {
            self.fleet_view.select(chat.agent);
        }
    }

    fn field_text(&self) -> bool {
        match &self.chat {
            Some(chat) => {
                let state = chat.session.state();
                chat.view.field_text(&state)
            }
            None => self.fleet_view.field_text(),
        }
    }

    pub fn input(&mut self, event: Event) -> Flow {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => {
                if let Some(chat) = &mut self.chat {
                    chat.view.editor.paste(&text);
                }
                Flow::Continue
            }
            Event::Mouse(mouse) => {
                let theme = self.theme();
                if let Some(chat) = &mut self.chat {
                    let state = chat.session.state();
                    chat.view.mouse(&state, mouse, theme);
                }
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.help {
            self.help = false;
            return Flow::Continue;
        }
        if key.code == KeyCode::Char('c') && ctrl {
            if self.field_text() {
                self.quit_armed = None;
                match &mut self.chat {
                    Some(chat) => chat.view.kill_field(),
                    None => self.fleet_view.kill_field(),
                };
                return Flow::Continue;
            }
            if self.quit_armed.take().is_some() {
                return Flow::Quit;
            }
            self.quit_armed = Some(Instant::now());
            return Flow::Continue;
        }
        self.quit_armed = None;
        if key.code == KeyCode::Char(self.config.leader) && ctrl {
            self.leader_pending = true;
            return Flow::Continue;
        }
        if std::mem::take(&mut self.leader_pending) {
            return self.chord(key);
        }
        match self.chat.is_some() {
            true => self.chat_key(key),
            false => self.fleet_key(key),
        }
    }

    fn chord(&mut self, key: KeyEvent) -> Flow {
        let theme = self.theme();
        let Some(chat) = &mut self.chat else {
            return Flow::Continue;
        };
        let state = chat.session.state();
        match key.code {
            KeyCode::Char('s') => {
                drop(state);
                self.close_chat();
            }
            KeyCode::Char('d') => return Flow::Quit,
            KeyCode::Char('k') => chat.view.move_focus(&state, true, theme),
            KeyCode::Char('j') => chat.view.move_focus(&state, false, theme),
            KeyCode::Char('o') => chat.view.toggle_expanded(&state),
            KeyCode::Char('y') => {
                if let Some(text) = chat.view.copy_text() {
                    drop(state);
                    match crate::terminal::write_osc52(&mut std::io::stdout(), &text) {
                        Ok(Some(notice)) => self.notice(notice, Tone::Info),
                        Ok(None) => self.notice("copied", Tone::Info),
                        Err(error) => self.notice(format!("could not copy: {error}"), Tone::Warn),
                    }
                }
            }
            KeyCode::Char('n') => {
                drop(state);
                self.next_in_family();
            }
            KeyCode::Char('t') if self.config.attach => {
                let entry = self.fleet.state().agent(&chat.agent).cloned();
                if let Some(entry) = entry {
                    return Flow::Attach(Box::new(entry));
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    /// `<leader> n`: the next agent in this agent's family, wrapping.
    fn next_in_family(&mut self) {
        let Some(chat) = &self.chat else {
            return;
        };
        let next = {
            let fleet = self.fleet.state();
            let root = fleet.root(&chat.agent);
            let mut members = vec![root.clone()];
            let mut at = 0;
            while at < members.len() {
                let children: Vec<AgentKey> =
                    fleet.families().children(&members[at]).cloned().collect();
                members.extend(children);
                at += 1;
            }
            let here = members
                .iter()
                .position(|member| *member == chat.agent)
                .unwrap_or(0);
            (members.len() > 1).then(|| members[(here + 1) % members.len()].clone())
        };
        if let Some(next) = next {
            self.open(next);
        }
    }

    fn fleet_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Char('q') if !self.fleet_view.field_text() => return Flow::Quit,
            KeyCode::Char('?') if !self.fleet_view.field_text() => {
                self.help = true;
                return Flow::Continue;
            }
            _ => {}
        }
        let effects = {
            let fleet = self.fleet.state();
            self.fleet_view.key(&fleet, key)
        };
        for effect in effects {
            if let Some(flow) = self.fleet_effect(effect) {
                return flow;
            }
        }
        Flow::Continue
    }

    fn fleet_effect(&mut self, effect: FleetEffect) -> Option<Flow> {
        let client = self.client.clone();
        match effect {
            FleetEffect::Open(agent) => self.open(agent),
            FleetEffect::Attach(agent) => {
                let entry = self.fleet.state().agent(&agent).cloned();
                return entry.map(|entry| Flow::Attach(Box::new(entry)));
            }
            FleetEffect::Create { kind } => {
                let cwd = self.config.working_dir.to_string_lossy().into_owned();
                self.spawn(async move {
                    let request = CreateAgentRequest {
                        agent_id: inputs::input_id(),
                        cwd,
                        kind: kind as i32,
                        ..CreateAgentRequest::default()
                    };
                    Some(match client.create_agent(request).await {
                        Ok(agent) => AppEvent::Created(agent),
                        Err(error) => AppEvent::Notice(
                            format!("could not start the agent: {error}"),
                            Tone::Warn,
                        ),
                    })
                });
            }
            FleetEffect::Rename { agent, name } => {
                self.spawn(async move {
                    let request = RenameAgentRequest {
                        agent_id: agent.agent,
                        name,
                    };
                    client.rename_agent(request).await.err().map(|error| {
                        AppEvent::Notice(format!("could not rename: {error}"), Tone::Warn)
                    })
                })
            }
            FleetEffect::Stop(agent) => {
                self.spawn(async move {
                    let request = StopAgentRequest {
                        agent_id: agent.agent,
                        mode: StopMode::Graceful as i32,
                    };
                    client.stop_agent(request).await.err().map(|error| {
                        AppEvent::Notice(format!("could not stop: {error}"), Tone::Warn)
                    })
                })
            }
            FleetEffect::Delete(agent) => self.spawn(async move {
                let request = DeleteAgentRequest {
                    agent_id: agent.agent,
                };
                Some(match client.delete_agent(request).await {
                    Ok(response) if !response.unreachable_children.is_empty() => AppEvent::Notice(
                        format!(
                            "deleted; {} child agent(s) on unreachable hosts remain",
                            response.unreachable_children.len()
                        ),
                        Tone::Warn,
                    ),
                    Ok(_) => AppEvent::Notice("deleted".into(), Tone::Info),
                    Err(error) => {
                        AppEvent::Notice(format!("could not delete: {error}"), Tone::Warn)
                    }
                })
            }),
        }
        None
    }

    fn chat_key(&mut self, key: KeyEvent) -> Flow {
        let theme = self.theme();
        let Some(chat) = &mut self.chat else {
            return Flow::Continue;
        };
        let effects = {
            let state = chat.session.state();
            let composing = ui_view::ask_card(&state).is_none() && chat.view.tray.is_none();
            if key.code == KeyCode::Char('?') && composing && chat.view.editor.is_empty() {
                self.help = true;
                return Flow::Continue;
            }
            chat.view.key(&state, key, theme)
        };
        for effect in effects {
            if let Some(flow) = self.chat_effect(effect) {
                return flow;
            }
        }
        Flow::Continue
    }

    fn chat_effect(&mut self, effect: ChatEffect) -> Option<Flow> {
        let chat = self.chat.as_mut()?;
        let session = chat.session.clone();
        let agent_id = chat.view.agent_id.clone();
        match effect {
            ChatEffect::Prompt { text, attachments } => self.spawn(async move {
                let sent = session.send_prompt(&text, attachments.clone()).await;
                match sent.outcome {
                    // A send that raced the exit: the composer turns to
                    // Resume and the draft goes back into it.
                    InputOutcome::Reply(SendInputResponse {
                        of: Some(send_input_response::Of::Rejected(rejected)),
                    }) if rejected.reason == "exited" => {
                        session.discard(&sent.id);
                        Some(AppEvent::Restore {
                            agent_id,
                            text,
                            attachments,
                            notice: None,
                        })
                    }
                    _ => None,
                }
            }),
            ChatEffect::Resume { text, attachments } => self.spawn(async move {
                let kind = session.state().kind();
                let input = inputs::prompt(kind, &text, attachments.clone())?;
                match session.resume_with(input).await {
                    Ok(_) => None,
                    Err(error) => Some(AppEvent::Restore {
                        agent_id,
                        text,
                        attachments,
                        notice: Some(format!("could not resume: {error}")),
                    }),
                }
            }),
            ChatEffect::Answer(input) => self.spawn(async move {
                session
                    .answer(input)
                    .await
                    .err()
                    .and_then(|error| match error {
                        // The card itself says it was rejected or not confirmed.
                        InputError::Rejected(_) | InputError::Uncertain => None,
                    })
            }),
            ChatEffect::Interrupt => self.spawn(async move {
                session
                    .interrupt()
                    .await
                    .err()
                    .map(|error| AppEvent::Notice(format!("stop {}", plain(error)), Tone::Warn))
            }),
            ChatEffect::Withdraw {
                id,
                text,
                attachments,
            } => self.spawn(async move {
                match session.withdraw(&id).await {
                    Ok(()) => Some(AppEvent::Restore {
                        agent_id,
                        text,
                        attachments,
                        notice: None,
                    }),
                    Err(error) => Some(AppEvent::Notice(
                        format!("withdraw {}", plain(error)),
                        Tone::Warn,
                    )),
                }
            }),
            ChatEffect::SendNow { id } => {
                self.spawn(async move {
                    session.send_now(&id).await.err().map(|error| {
                        AppEvent::Notice(format!("send now {}", plain(error)), Tone::Warn)
                    })
                })
            }
            ChatEffect::Resend { id } => {
                let input = session
                    .state()
                    .inputs()
                    .get(&id)
                    .map(|sent| sent.input.clone());
                if let Some(mut input) = input {
                    session.discard(&id);
                    input.input_id.clear();
                    self.spawn(async move {
                        session.send(input).await;
                        None
                    });
                }
            }
            ChatEffect::Discard { id } => session.discard(&id),
            ChatEffect::Page(n) => {
                {
                    let state = session.state();
                    chat.view.page_sent(&state);
                }
                self.spawn(async move {
                    session
                        .page_older(n)
                        .await
                        .err()
                        .map(|error| AppEvent::Notice(error.to_string(), Tone::Warn))
                });
            }
            ChatEffect::Paste => match chat.view.paste(read_clipboard()) {
                Ok(Some(effect)) => return self.chat_effect(effect),
                Ok(None) => {}
                Err(error) => self.notice(error, Tone::Warn),
            },
            ChatEffect::Attach { name, mime, bytes } => {
                self.notice(format!("attaching {name}…"), Tone::Info);
                self.spawn(async move {
                    Some(match session.put_blob(&name, &mime, bytes).await {
                        Ok(blob) => AppEvent::Attached { agent_id, blob },
                        Err(error) => AppEvent::Notice(
                            format!("could not attach {name}: {error}"),
                            Tone::Warn,
                        ),
                    })
                });
            }
            ChatEffect::Copy(text) => {
                let _ = crate::terminal::write_osc52(&mut std::io::stdout(), &text);
            }
            ChatEffect::RawAttach => {
                let entry = self.fleet.state().agent(&chat.agent).cloned();
                return entry.map(|entry| Flow::Attach(Box::new(entry)));
            }
        }
        None
    }

    /// A deleted agent's open chat closes to the fleet.
    pub fn check_ended(&mut self) {
        let ended = self.chat.as_ref().and_then(|chat| chat.session.ended());
        if let Some(error) = ended {
            self.close_chat();
            self.notice(format!("the chat closed: {error}"), Tone::Warn);
        }
    }

    fn footer(&self, width: usize) -> Option<Line<'static>> {
        let theme = self.theme();
        if self.quit_armed.is_some() {
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, "press ctrl+c again to quit", theme.warn(), width);
            return Some(line);
        }
        if self.leader_pending {
            let mut line = Line::from(Span::raw("  "));
            let words = if self.chat.is_some() {
                "s fleet · d leave to the shell · k/j focus · o open · y copy · n next in family"
            } else {
                "leader: nothing here"
            };
            push(&mut line, words, theme.muted(), width);
            return Some(line);
        }
        let (words, tone, _) = self.notice.as_ref()?;
        let mut line = Line::from(Span::raw("  "));
        let style = match tone {
            Tone::Info => theme.muted(),
            Tone::Warn => theme.warn(),
        };
        push(&mut line, words.clone(), style, width);
        Some(line)
    }

    /// Paints the screen and asks for an older page when the reader is
    /// within one of the oldest held row.
    pub fn draw(&mut self, paint: &mut Paint<'_>) {
        let area = paint.area();
        let theme = self.theme();
        let width = usize::from(area.width);
        if self.help {
            paint.render_widget(
                Paragraph::new(help_lines(self.config.leader, width, theme)),
                area,
            );
            return;
        }
        let footer = self.footer(width);
        let now = now_ms();
        let page = match &mut self.chat {
            Some(chat) => {
                let family = family_header(&self.fleet.state(), &chat.agent.agent);
                let state = chat.session.state();
                chat.view
                    .draw(paint, area, &state, family.as_ref(), footer, now, theme)
            }
            None => {
                let fleet = self.fleet.state();
                self.fleet_view
                    .draw(paint, area, &fleet, footer, now, theme);
                None
            }
        };
        if let Some(n) = page {
            self.chat_effect(ChatEffect::Page(n));
        }
    }
}

fn help_lines(leader: char, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let leader = format!("ctrl+{leader}");
    let rows: Vec<(&str, String)> = vec![
        ("Fleet", String::new()),
        ("enter", "open the chat".into()),
        (
            "o / ctrl+enter",
            "the agent's own terminal (this machine)".into(),
        ),
        ("n / r / s / d", "new · rename · stop · delete".into()),
        ("z", "show or hide a family's agents".into()),
        ("h", "hosts: trusted and found nearby".into()),
        ("q", "quit".into()),
        ("", String::new()),
        ("Chat", String::new()),
        (
            "enter",
            "send; queue while it works; resume an exited agent".into(),
        ),
        ("ctrl+j", "new line".into()),
        (
            "ctrl+v",
            "attach an image or file from the clipboard".into(),
        ),
        (
            "↑ on the first line",
            "queued and unconfirmed messages: send now, withdraw, resend, discard".into(),
        ),
        ("ctrl+x", "stop the turn; the agent stays".into()),
        (
            "pgup / pgdn, wheel",
            "scroll; ctrl+end follows the newest".into(),
        ),
        (
            "esc",
            "close, clear focus, then follow; never answers".into(),
        ),
        ("", String::new()),
        ("Leader", String::new()),
        ("s", format!("{leader} s  back to the fleet")),
        (
            "d",
            format!("{leader} d  leave to the shell; agents keep running"),
        ),
        (
            "k / j",
            format!("{leader} k/j  focus an older or newer row"),
        ),
        ("o", format!("{leader} o  open the focused row or run")),
        ("y", format!("{leader} y  copy the focused row")),
        ("n", format!("{leader} n  next agent in this family")),
        ("", String::new()),
        ("ctrl+c", "clear the field; twice on nothing quits".into()),
    ];
    let mut lines = vec![
        Line::from(Span::styled("  Keys", theme.emphasis())),
        Line::default(),
    ];
    for (key, words) in rows {
        let mut line = Line::from(Span::raw("  "));
        if words.is_empty() {
            push(&mut line, key, theme.emphasis(), width);
        } else {
            push(&mut line, format!("{key:<22}"), theme.code(), width);
            push(&mut line, words, theme.muted(), width);
        }
        lines.push(line);
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("  any key closes", theme.muted())));
    lines
}
