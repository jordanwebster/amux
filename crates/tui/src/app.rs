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
use ui_state::{AgentKey, Composer, InputOutcome, PhaseView};
use ui_view::family_header;
use wire::{
    Agent, Attachment, CreateAgentRequest, DeleteAgentRequest, Diff, RenameAgentRequest,
    SendInputResponse, StopAgentRequest, StopMode, send_input_response,
};

use crate::chat::layout::{CAP, PAGE};
use crate::chat::{ChatEffect, ChatView};
use crate::clipboard::read_clipboard;
use crate::fleet::{FleetEffect, FleetView};
use crate::text::push;
use crate::theme::Theme;

/// How long a second Ctrl+C has to arrive to quit.
const QUIT_WINDOW: Duration = Duration::from_secs(3);
/// How long a notice stays in the footer.
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// How long the leader waits before its panel shows: typed quickly, a
/// chord never draws it.
const PANEL_AFTER: Duration = Duration::from_millis(400);

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
    /// This build's version, compared with the daemon's.
    pub version: String,
    /// The profile's host on this machine: only its agents' terminals are
    /// here to attach to.
    pub local_host: Vec<u8>,
    /// Where this client keeps its layout between runs; None keeps it for
    /// this run only.
    pub layout: Option<std::path::PathBuf>,
}

/// How this client lays its screens out, kept between runs: a person's
/// preference for this terminal, not state any other client shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    /// The chat's overview pane stays open across chats until closed.
    #[serde(default)]
    pub overview: bool,
}

impl Layout {
    fn load(path: Option<&std::path::Path>) -> Layout {
        path.and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Best effort: a layout that cannot be written is only forgotten.
    fn save(self, path: Option<&std::path::Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self) {
            let _ = std::fs::write(path, bytes);
        }
    }
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
    /// An agent created with its first prompt: its chat opens, or home
    /// stays and says it started.
    Started {
        agent: Agent,
        open: bool,
    },
    /// Creating an agent from home's draft failed; the draft is kept.
    StartFailed(String),
    /// A working-tree diff frozen for this agent's review page.
    Review {
        agent_id: Vec<u8>,
        diff: Diff,
        patch: String,
        /// The file to open the page at.
        at: Option<String>,
    },
    /// The working tree's lines added and removed, for the chat's header,
    /// and each changed file's, for its overview.
    DiffStat {
        agent_id: Vec<u8>,
        added: u32,
        removed: u32,
        files: Vec<crate::chat::pane::FileLine>,
    },
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

impl OpenChat {
    /// Tells the session when the reader left or returned to the newest row.
    fn tell_following(&mut self) {
        if let Some(following) = self.view.following_moved() {
            self.session.follow(following);
        }
    }
}

pub struct App {
    client: Arc<dyn Client>,
    pub fleet: Fleet,
    pub fleet_view: FleetView,
    pub chat: Option<OpenChat>,
    pub config: TuiConfig,
    leader_pending: bool,
    /// When the leader was pressed, for its panel's pause.
    leader_since: Option<Instant>,
    quit_armed: Option<Instant>,
    help: bool,
    notice: Option<(String, Tone, Instant)>,
    opening: Option<AgentKey>,
    layout: Layout,
    events: mpsc::UnboundedSender<AppEvent>,
    pub receiver: mpsc::UnboundedReceiver<AppEvent>,
}

fn now_ms() -> i64 {
    use client::Clock as _;
    SystemClock.now_ms()
}

/// Why an agent's own terminal cannot be attached from here: raw attach
/// reads the agent's directory on this machine, and headless Claude has no
/// terminal at all. Its chat is the way in either way.
pub(crate) fn terminal_refusal(agent: &Agent, config: &TuiConfig) -> Option<&'static str> {
    if agent.host_id != config.local_host {
        Some("its terminal is on another machine; enter opens its chat")
    } else if agent.kind() == wire::Kind::ClaudeSdk {
        Some("headless Claude has no terminal; enter opens its chat")
    } else {
        None
    }
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
        fleet_view.version = config.version.clone();
        fleet_view.local_host = config.local_host.clone();
        fleet_view.working_dir = config.working_dir.to_string_lossy().into_owned();
        let layout = Layout::load(config.layout.as_deref());
        App {
            layout,
            client,
            fleet,
            fleet_view,
            chat: None,
            config,
            leader_pending: false,
            leader_since: None,
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
        if self.leader_pending
            && let Some(since) = self.leader_since
            && since.elapsed() < PANEL_AFTER
        {
            at(since + PANEL_AFTER);
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
                        let terminal = self.fleet.state().agent(&agent).is_some_and(|entry| {
                            self.config.attach && terminal_refusal(entry, &self.config).is_none()
                        });
                        let mut view = ChatView::new(agent.agent.clone(), now_ms(), terminal);
                        view.leader = self.config.leader;
                        view.local = agent.host == self.config.local_host;
                        // The overview opens where it was left; the
                        // composer has the keys either way.
                        view.pane_open = self.layout.overview;
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
            AppEvent::Review {
                agent_id,
                diff,
                patch,
                at,
            } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.open_review_at(diff, patch, at.as_deref());
                    // The page answers "reading the working tree…".
                    self.notice = None;
                }
            }
            AppEvent::DiffStat {
                agent_id,
                added,
                removed,
                files,
            } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.diff_stat = Some((added, removed));
                    chat.view.diff_files = Some(files);
                }
            }
            AppEvent::Created(agent) => {
                let key = ui_state::agent_key(&agent);
                self.fleet_view.select(key.clone());
                self.open(key);
            }
            AppEvent::Started { agent, open } => {
                self.fleet_view.home.started();
                let key = ui_state::agent_key(&agent);
                self.fleet_view.select(key.clone());
                if open {
                    // The inventory may not list it yet; the chat opens
                    // once it does.
                    self.config.initial_chat = Some(agent.agent_id.clone());
                    self.fleet_changed();
                } else {
                    let name = agent.name.as_deref().unwrap_or("the agent");
                    self.notice(format!("started {name}"), Tone::Info);
                }
            }
            AppEvent::StartFailed(error) => {
                self.fleet_view.home.start_failed();
                self.notice(format!("could not start the agent: {error}"), Tone::Warn);
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
            let result = Session::open(client, entry, PAGE, CAP, SystemClock)
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
                if self.help {
                    return Flow::Continue;
                }
                match &mut self.chat {
                    Some(chat) => {
                        let state = chat.session.state();
                        chat.view.paste_text(&state, &text);
                    }
                    None => self.fleet_view.paste(&text),
                }
                Flow::Continue
            }
            Event::Mouse(mouse) => {
                if self.help {
                    return Flow::Continue;
                }
                let theme = self.theme();
                if let Some(chat) = &mut self.chat {
                    let effects = {
                        let state = chat.session.state();
                        chat.view.mouse(&state, mouse, theme)
                    };
                    chat.tell_following();
                    for effect in effects {
                        if let Some(flow) = self.chat_effect(effect) {
                            return flow;
                        }
                    }
                    return Flow::Continue;
                }
                let effects = {
                    let fleet = self.fleet.state();
                    self.fleet_view.mouse(&fleet, mouse)
                };
                for effect in effects {
                    if let Some(flow) = self.fleet_effect(effect) {
                        return flow;
                    }
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
                    Some(chat) => {
                        let state = chat.session.state();
                        chat.view.kill_field(&state)
                    }
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
            self.leader_since = Some(Instant::now());
            return Flow::Continue;
        }
        if std::mem::take(&mut self.leader_pending) {
            self.leader_since = None;
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
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('s') => {
                drop(state);
                self.close_chat();
            }
            KeyCode::Char('d') => return Flow::Quit,
            KeyCode::Char('k') => chat.view.move_focus(&state, true, theme),
            KeyCode::Char('j') => chat.view.move_focus(&state, false, theme),
            KeyCode::Char('o') => chat.view.toggle_expanded(&state),
            KeyCode::Char(PANE_CHORD) => chat.view.pane_toggle_key(),
            KeyCode::Char('t') if ctrl_held(&key) => chat.view.pane_toggle_key(),
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
            KeyCode::Char('r') => {
                drop(state);
                self.review(None);
            }
            KeyCode::Char('t') if self.config.attach => {
                let agent = chat.agent.clone();
                drop(state);
                return self.raw_attach(&agent).unwrap_or(Flow::Continue);
            }
            _ => {}
        }
        Flow::Continue
    }

    /// `<leader> r`, or a `[diff]` clicked: the review page over the
    /// agent's working tree, or back to the one already in the draft.
    /// The review page, at the file `at` when one is named.
    fn review(&mut self, at: Option<String>) {
        let Some(chat) = &mut self.chat else {
            return;
        };
        if chat.view.resume_review() {
            if let Some(path) = &at {
                chat.view.review_file(path);
            }
            return;
        }
        let state = chat.session.state();
        let writable = matches!(state.composer(), Composer::Send | Composer::Resume);
        let agent_id = chat.view.agent_id.clone();
        drop(state);
        if !writable {
            self.notice("review waits until the chat is current", Tone::Warn);
            return;
        }
        {
            let client = self.client.clone();
            self.notice("reading the working tree…", Tone::Info);
            self.spawn(async move {
                Some(
                    match ui_runtime::review::working_tree_review(client.as_ref(), &agent_id).await
                    {
                        Ok((_, patch)) if patch.trim().is_empty() => {
                            AppEvent::Notice("no changes in the working tree".into(), Tone::Info)
                        }
                        Ok((diff, patch)) => AppEvent::Review {
                            agent_id,
                            diff,
                            patch,
                            at,
                        },
                        Err(error) => AppEvent::Notice(
                            format!("could not read the working tree: {error}"),
                            Tone::Warn,
                        ),
                    },
                )
            });
        }
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
            FleetEffect::Quit => return Some(Flow::Quit),
            FleetEffect::Help => self.help = true,
            FleetEffect::Open(agent) => self.open(agent),
            FleetEffect::Attach(agent) => return self.raw_attach(&agent),
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
            FleetEffect::Start {
                kind,
                text,
                attachments,
                open,
            } => {
                let cwd = self.config.working_dir.to_string_lossy().into_owned();
                self.spawn(async move {
                    let request = CreateAgentRequest {
                        agent_id: inputs::input_id(),
                        cwd,
                        kind: kind as i32,
                        initial_prompt: inputs::prompt(kind, &text, attachments),
                        ..CreateAgentRequest::default()
                    };
                    Some(match client.create_agent(request).await {
                        Ok(agent) => AppEvent::Started { agent, open },
                        Err(error) => AppEvent::StartFailed(error.to_string()),
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
            if chat.view.opens_help(&state, key) {
                self.help = true;
                return Flow::Continue;
            }
            chat.view.key(&state, key, theme)
        };
        // Before any send the key made: sending a prompt is a return to
        // the newest row.
        chat.tell_following();
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
            ChatEffect::Review => self.review(None),
            ChatEffect::ReviewAt(path) => self.review(Some(path)),
            ChatEffect::Home => self.close_chat(),
            ChatEffect::Press(key) => {
                // The leader clicked shows its panel at once: the click is
                // the pause.
                if key.code == KeyCode::Char(self.config.leader)
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.leader_pending = true;
                    self.leader_since = Instant::now().checked_sub(PANEL_AFTER);
                    return None;
                }
                return Some(self.key(key));
            }
            ChatEffect::Chord(letter) => {
                self.leader_pending = false;
                self.leader_since = None;
                return Some(self.chord(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE)));
            }
            ChatEffect::RawAttach => {
                let agent = chat.agent.clone();
                return self.raw_attach(&agent);
            }
        }
        None
    }

    /// Hands the terminal to the agent's own interface when it is on this
    /// machine and has one; says why not otherwise.
    fn raw_attach(&mut self, agent: &AgentKey) -> Option<Flow> {
        let entry = self.fleet.state().agent(agent).cloned()?;
        match terminal_refusal(&entry, &self.config) {
            None => Some(Flow::Attach(Box::new(entry))),
            Some(why) => {
                self.notice(why, Tone::Warn);
                None
            }
        }
    }

    /// What the loop does before each frame: a deleted agent's open chat
    /// closes to the fleet, and the open chat's working-tree totals are
    /// read when it asks (after opening and after each turn ends).
    pub fn housekeep(&mut self) {
        let ended = self.chat.as_ref().and_then(|chat| chat.session.ended());
        if let Some(error) = ended {
            self.close_chat();
            self.notice(format!("the chat closed: {error}"), Tone::Warn);
            return;
        }
        let Some(chat) = &mut self.chat else {
            return;
        };
        let wanted = {
            let state = chat.session.state();
            chat.view.wants_diff_stat(&state)
        };
        if wanted {
            let client = self.client.clone();
            let agent_id = chat.view.agent_id.clone();
            self.spawn(async move {
                let (diff, patch) =
                    ui_runtime::review::working_tree_review(client.as_ref(), &agent_id)
                        .await
                        .ok()?;
                let doc = ui_view::review_doc(&diff, &patch, &[]);
                Some(AppEvent::DiffStat {
                    agent_id,
                    added: doc.added,
                    removed: doc.removed,
                    files: doc
                        .files
                        .iter()
                        .map(|file| crate::chat::pane::FileLine {
                            path: file.path.clone(),
                            added: file.added,
                            removed: file.removed,
                        })
                        .collect(),
                })
            });
        }
    }

    fn footer(&self, width: usize) -> Option<Line<'static>> {
        let theme = self.theme();
        if self.quit_armed.is_some() {
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, "press ctrl+c again to quit", theme.warn(), width);
            return Some(line);
        }
        let panel_chat = self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.view.redesigned());
        if self.leader_pending && !panel_chat {
            let mut line = Line::from(Span::raw("  "));
            let words = if self.chat.is_some() {
                "s fleet · d leave to the shell · k/j focus · o open · y copy · r review · n next in family"
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
        self.keep_layout();
        let area = paint.area();
        let theme = self.theme();
        let width = usize::from(area.width);
        if self.help {
            paint.render_widget(
                Paragraph::new(help_lines(
                    self.config.leader,
                    self.fleet_view.redesigned(),
                    width,
                    theme,
                )),
                area,
            );
            return;
        }
        let footer = self.footer(width);
        let now = now_ms();
        let page = match &mut self.chat {
            Some(chat) => {
                let (family, away) = {
                    let fleet = self.fleet.state();
                    (
                        family_header(&fleet, &chat.agent.agent),
                        ui_view::away(&fleet, &self.config.local_host, &chat.agent.host),
                    )
                };
                chat.view.away = away;
                let panel_due = self.leader_pending
                    && self
                        .leader_since
                        .is_none_or(|since| since.elapsed() >= PANEL_AFTER);
                chat.view.panel = panel_due.then(|| panel_entries(self.config.attach));
                let page = {
                    let state = chat.session.state();
                    chat.view
                        .draw(paint, area, &state, family.as_ref(), footer, now, theme)
                };
                // A reload's swap takes the reader to the newest row.
                chat.tell_following();
                page
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

/// The chord the leader's panel names for the in-flight pane: Ctrl+T, which
/// also works on its own.
const PANE_CHORD: char = '\u{14}';

impl App {
    /// Takes in a layout change the open chat made, and keeps it.
    fn keep_layout(&mut self) {
        let Some(chat) = &self.chat else {
            return;
        };
        if chat.view.pane_open != self.layout.overview {
            self.layout.overview = chat.view.pane_open;
            self.layout.save(self.config.layout.as_deref());
        }
    }
}

/// The leader's panel in a chat: every chord and what it does.
fn panel_entries(attach: bool) -> Vec<crate::chat::PanelEntry> {
    let entry = |key: &str, action: &str, chord: char| (key.to_owned(), action.to_owned(), chord);
    let mut entries = vec![
        entry("s", "home", 's'),
        entry("r", "review the working tree", 'r'),
        entry("n", "next agent in this family", 'n'),
        entry("k", "focus an older row", 'k'),
        entry("j", "focus a newer row", 'j'),
        entry("o", "open the focused row", 'o'),
        entry("y", "copy the focused row", 'y'),
        entry("ctrl+t", "overview", PANE_CHORD),
    ];
    if attach {
        entries.push(entry("t", "the agent's own terminal", 't'));
    }
    entries.push(entry("d", "leave to the shell", 'd'));
    entries.push(entry("?", "all keys", '?'));
    entries
}

fn help_lines(leader: char, redesigned: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let leader = format!("ctrl+{leader}");
    let mut rows: Vec<(&str, String)> = if redesigned {
        crate::home::help_rows()
    } else {
        vec![
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
        ]
    };
    rows.extend([
        ("", String::new()),
        ("Chat", String::new()),
        (
            "enter",
            "send; queue while it works; resume an exited agent".into(),
        ),
        ("shift+enter / ctrl+j", "new line".into()),
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
            "ctrl+t",
            "the overview: tasks, background jobs, changes; again to switch or close".into(),
        ),
        (
            "shift+tab",
            "the agent's next mode, where it has one to move to".into(),
        ),
        (
            "pgup / pgdn, wheel",
            "scroll; ctrl+end (or the ↓ button) jumps to the newest".into(),
        ),
        (
            "click",
            "hints, [Diff], [Home], the mode, a pinned message, a folded line".into(),
        ),
        (
            "shift+drag",
            "select text to copy (option+drag in iTerm2 and Ghostty)".into(),
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
        (
            "r",
            format!("{leader} r  review the working tree; comments go in the draft"),
        ),
        ("n", format!("{leader} n  next agent in this family")),
        ("?", format!("{leader} ?  these keys")),
        ("", String::new()),
        ("ctrl+c", "clear the field; twice on nothing quits".into()),
    ]);
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

fn ctrl_held(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}
