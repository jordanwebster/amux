//! One agent's life: the interpreter between the provider and the journal,
//! the daemon's control connection, and the lifecycle that decides when the
//! process ends.
//!
//! Starting and running are the interpreter's business. The host adds what
//! only a process can know: whether a daemon is connected, how long it has
//! been gone, and whether this incarnation is on its way out.
//!
//! - With no daemon connected, at start or after ctl.sock's end of stream,
//!   a grace timer runs; a daemon dialling in cancels it. When it runs out
//!   the agent drains: it finishes the running turn, writes its final
//!   boundary and exits. An ask left open with nobody to answer it gets the
//!   drain deadline and then exits "orphaned while waiting for you".
//! - Stop graceful drains the same way; stop abort cancels the turn the
//!   provider's way first; stop kill ends the process group at once and
//!   writes nothing. A draining agent answers new input rejected{draining}.
//! - An agent with a parent is one-shot: at a turn end with nothing queued,
//!   running or accepted-but-unconsumed it exits, and input arriving while
//!   it does is answered rejected{exiting}.
//! - The provider exiting ends the agent, whatever state it was in.

use std::path::PathBuf;
use std::sync::Arc;

use interpret::{Effect, Event, Interpreter, Stepped, reason, reply};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use wire::{AgentHello, AgentSpec, CtlFrame, InputReply, Nudge, Phase, StopMode, ctl_frame};

use crate::clock::Clock;
use crate::dir::{self, PtyLog};
use crate::local_socket::{LocalListener, LocalStream};
use crate::provider::{Provider, ProviderEvent};
use crate::{AgentError, ExitCause, VERSION, ctl};

/// Grace after the daemon leaves, when the spec leaves it unset.
const DEFAULT_GRACE_MS: i64 = 5 * 60 * 1000;
/// Drain deadline for an orphaned ask, when the spec leaves it unset.
const DEFAULT_DRAIN_MS: i64 = 10 * 60 * 1000;
/// How long a provider asked to finish gets before its group is killed.
const PROVIDER_STOP_MS: i64 = 10 * 1000;
/// Journal segment size, when the spec leaves it unset.
const DEFAULT_SEGMENT_BYTES: u64 = 1024 * 1024;
/// Terminal log segments: enough to rebuild a screen.
const PTY_SEGMENT_BYTES: u64 = 256 * 1024;
const PTY_SEGMENTS_KEPT: usize = 4;

enum Mode {
    Running,
    Draining {
        why: Drain,
        /// Set while an ask is open during a drain with no daemon.
        ask_until: Option<i64>,
    },
    Exiting {
        cause: ExitCause,
        /// When the provider's group is killed if it has not exited.
        until: i64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drain {
    DaemonLost,
    Graceful,
    Abort,
}

impl Drain {
    fn cause(self) -> ExitCause {
        match self {
            Drain::DaemonLost => ExitCause::DaemonLost,
            Drain::Graceful => ExitCause::Stopped,
            Drain::Abort => ExitCause::Aborted,
        }
    }
}

/// The daemon's connection: its id, so frames from a replaced one are
/// ignored, and its outgoing frames.
struct Daemon {
    id: u64,
    frames: mpsc::UnboundedSender<CtlFrame>,
}

pub(crate) struct Host<I: Interpreter> {
    dir: PathBuf,
    spec: AgentSpec,
    clock: Arc<dyn Clock>,
    state: I::State,
    journal: journal::Writer,
    pty_log: Option<PtyLog>,
    provider: Option<Provider>,
    daemon: Option<Daemon>,
    connections: u64,
    grace_until: Option<i64>,
    mode: Mode,
    phase: Phase,
    queue_empty: bool,
    turn_ended: bool,
    /// The interpreter asked the process to exit.
    asked_to_exit: Option<ExitCause>,
    last_tick: i64,
    done: Option<ExitCause>,
}

/// Everything the host listens to.
struct Channels {
    provider: mpsc::Receiver<ProviderEvent>,
    provider_tx: mpsc::Sender<ProviderEvent>,
    connections: mpsc::Receiver<LocalStream>,
    frames: mpsc::UnboundedReceiver<(u64, Option<CtlFrame>)>,
    frames_tx: mpsc::UnboundedSender<(u64, Option<CtlFrame>)>,
}

pub(crate) async fn run<I: Interpreter>(
    dir: PathBuf,
    spec: AgentSpec,
    clock: Arc<dyn Clock>,
) -> Result<ExitCause, AgentError> {
    let config = spec.config.clone().unwrap_or_default();
    let terminal = spec.kind == "claude_pty";

    // Listen first, so a daemon can dial in as soon as the process exists.
    let ctl = LocalListener::bind(&dir.join(dir::CTL_SOCK)).map_err(AgentError::Socket)?;
    let pty = if terminal || spec.kind == "codex" {
        Some(LocalListener::bind(&dir.join(dir::PTY_SOCK)).map_err(AgentError::Socket)?)
    } else {
        None
    };
    let hooks = if terminal {
        Some(
            LocalListener::bind(&dir.join(dir::PRIVATE).join(dir::HOOKS_SOCK))
                .map_err(AgentError::Socket)?,
        )
    } else {
        None
    };

    let (provider_tx, provider_rx) = mpsc::channel(256);
    let (connections_tx, connections_rx) = mpsc::channel(4);
    let (frames_tx, frames_rx) = mpsc::unbounded_channel();
    let mut tasks = vec![tokio::spawn(accept(ctl, connections_tx))];
    if let Some(pty) = pty {
        // Raw attach is served by a later change; until then a terminal
        // client's connection is closed at once.
        tasks.push(tokio::spawn(refuse(pty)));
    }
    if let Some(hooks) = hooks {
        tasks.push(tokio::spawn(hook_payloads(hooks, provider_tx.clone())));
    }

    let segment_bytes = match config.journal_segment_bytes {
        0 => DEFAULT_SEGMENT_BYTES,
        bytes => bytes,
    };
    let journal = journal::Writer::open(dir.join(dir::JOURNAL), segment_bytes)
        .map_err(AgentError::Journal)?;
    let pty_log = if terminal {
        Some(
            PtyLog::open(dir.join(dir::PTY), PTY_SEGMENT_BYTES, PTY_SEGMENTS_KEPT)
                .map_err(AgentError::Journal)?,
        )
    } else {
        None
    };
    let (state, first) = I::initial(&spec, VERSION);
    let now = clock.now_ms();
    let mut host = Host::<I> {
        dir,
        grace_until: Some(now + ms(config.grace_ms, DEFAULT_GRACE_MS)),
        spec,
        clock,
        state,
        journal,
        pty_log,
        provider: None,
        daemon: None,
        connections: 0,
        mode: Mode::Running,
        phase: Phase::Starting,
        queue_empty: true,
        turn_ended: false,
        asked_to_exit: None,
        last_tick: i64::MIN,
        done: None,
    };
    host.apply(Stepped {
        step: first,
        effects: Vec::new(),
    })
    .await?;

    let mut channels = Channels {
        provider: provider_rx,
        provider_tx,
        connections: connections_rx,
        frames: frames_rx,
        frames_tx,
    };
    let result = host.run(&mut channels).await;
    for task in tasks {
        task.abort();
    }
    result
}

impl<I: Interpreter> Host<I> {
    async fn run(&mut self, channels: &mut Channels) -> Result<ExitCause, AgentError> {
        match Provider::spawn(&self.spec, &self.dir, channels.provider_tx.clone()).await {
            Ok(provider) => self.provider = Some(provider),
            Err(error) => {
                let cause = ExitCause::Unstarted(error.to_string());
                self.feed(Event::Exiting {
                    cause: cause.to_string(),
                })
                .await?;
                return Ok(cause);
            }
        }
        loop {
            let sleep = match self.deadline() {
                Some(at) => self.clock.sleep_until(at),
                None => Box::pin(std::future::pending()),
            };
            tokio::select! {
                event = channels.provider.recv() => {
                    // The host holds a sender, so the channel never closes.
                    if let Some(event) = event {
                        self.on_provider(event).await?;
                    }
                }
                Some(stream) = channels.connections.recv() => {
                    self.on_connect(stream, &channels.frames_tx);
                }
                Some((id, frame)) = channels.frames.recv() => {
                    self.on_frame(id, frame).await?;
                }
                () = sleep => self.on_deadline().await?,
            }
            if self.done.is_none() {
                self.settle().await?;
            }
            if let Some(cause) = self.done.take() {
                return Ok(cause);
            }
        }
    }

    // --- events ----------------------------------------------------------

    async fn on_provider(&mut self, event: ProviderEvent) -> Result<(), AgentError> {
        match event {
            ProviderEvent::Output(bytes) => {
                if let Some(log) = &mut self.pty_log {
                    log.append(&bytes).map_err(AgentError::Journal)?;
                }
            }
            ProviderEvent::Fact(fact) => {
                if matches!(self.mode, Mode::Exiting { .. }) {
                    // The final boundary is written; what the provider says
                    // while it shuts down is not part of the incarnation.
                    return Ok(());
                }
                self.feed(Event::Fact(fact)).await?;
            }
            ProviderEvent::Exited(code) => {
                if let Mode::Exiting { cause, .. } = &self.mode {
                    self.done = Some(cause.clone());
                    return Ok(());
                }
                self.feed(Event::ProviderExit { code }).await?;
                self.done = Some(ExitCause::ProviderExited(code));
            }
        }
        Ok(())
    }

    fn on_connect(
        &mut self,
        stream: LocalStream,
        frames_tx: &mpsc::UnboundedSender<(u64, Option<CtlFrame>)>,
    ) {
        self.connections += 1;
        let id = self.connections;
        let (mut reader, mut writer) = tokio::io::split(stream);
        let incoming = frames_tx.clone();
        tokio::spawn(async move {
            loop {
                match ctl::read_frame(&mut reader).await {
                    Ok(Some(frame)) => {
                        if incoming.send((id, Some(frame))).is_err() {
                            return;
                        }
                    }
                    Ok(None) | Err(_) => {
                        let _ = incoming.send((id, None));
                        return;
                    }
                }
            }
        });
        let (frames, mut outgoing) = mpsc::unbounded_channel::<CtlFrame>();
        tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if ctl::write_frame(&mut writer, &frame).await.is_err() {
                    return;
                }
            }
        });
        // A new connection replaces the old: a restarted daemon dials in
        // while the old one's socket may not have closed yet.
        self.daemon = Some(Daemon { id, frames });
        self.grace_until = None;
        self.send(ctl_frame::Of::Hello(AgentHello {
            agent_id: self.spec.agent_id.clone(),
            agent_version: VERSION.to_owned(),
            journal_offset: self.journal.offset(),
        }));
    }

    async fn on_frame(&mut self, id: u64, frame: Option<CtlFrame>) -> Result<(), AgentError> {
        if self.daemon.as_ref().is_none_or(|daemon| daemon.id != id) {
            return Ok(());
        }
        let Some(frame) = frame else {
            // End of stream: the daemon is gone. Deliberate stops are Stop
            // frames, so this starts the grace timer.
            self.daemon = None;
            if matches!(self.mode, Mode::Running) {
                self.grace_until = Some(self.clock.now_ms() + self.grace_ms());
            }
            return Ok(());
        };
        match frame.of {
            Some(ctl_frame::Of::Input(input)) => match self.mode {
                Mode::Running => self.feed(Event::Input(input)).await?,
                Mode::Draining { .. } => {
                    self.reply(input.input_id, reply::rejected(reason::DRAINING))
                }
                Mode::Exiting { .. } => {
                    self.reply(input.input_id, reply::rejected(reason::EXITING))
                }
            },
            Some(ctl_frame::Of::Stop(stop)) => self.stop(stop.mode()).await?,
            // Hello, nudges, dump parts and replies travel the other way.
            _ => {}
        }
        Ok(())
    }

    async fn stop(&mut self, mode: StopMode) -> Result<(), AgentError> {
        match mode {
            StopMode::Kill => {
                if let Some(provider) = &mut self.provider {
                    provider.kill();
                }
                self.done = Some(ExitCause::Killed);
            }
            StopMode::Graceful => {
                if matches!(self.mode, Mode::Running) {
                    self.mode = Mode::Draining {
                        why: Drain::Graceful,
                        ask_until: None,
                    };
                }
            }
            StopMode::Abort => {
                let aborting = matches!(
                    self.mode,
                    Mode::Draining {
                        why: Drain::Abort,
                        ..
                    } | Mode::Exiting { .. }
                );
                if !aborting {
                    self.feed(Event::StopRequested(StopMode::Abort)).await?;
                    self.mode = Mode::Draining {
                        why: Drain::Abort,
                        ask_until: None,
                    };
                }
            }
        }
        Ok(())
    }

    async fn on_deadline(&mut self) -> Result<(), AgentError> {
        let now = self.clock.now_ms();
        match &self.mode {
            Mode::Running => {
                if self.grace_until.is_some_and(|until| until <= now) {
                    self.grace_until = None;
                    self.feed(Event::DaemonLost).await?;
                    self.mode = Mode::Draining {
                        why: Drain::DaemonLost,
                        ask_until: None,
                    };
                }
            }
            Mode::Draining { ask_until, .. } => {
                if ask_until.is_some_and(|until| until <= now) {
                    self.exit(ExitCause::Orphaned).await?;
                }
            }
            Mode::Exiting { cause, until } => {
                if *until <= now {
                    self.done = Some(cause.clone());
                    if let Some(provider) = &mut self.provider {
                        provider.kill();
                    }
                }
            }
        }
        Ok(())
    }

    fn deadline(&self) -> Option<i64> {
        match &self.mode {
            Mode::Running if self.daemon.is_none() => self.grace_until,
            Mode::Running => None,
            Mode::Draining { ask_until, .. } => *ask_until,
            Mode::Exiting { until, .. } => Some(*until),
        }
    }

    /// Decides, after anything happened, whether this incarnation ends.
    async fn settle(&mut self) -> Result<(), AgentError> {
        let turn_ended = std::mem::take(&mut self.turn_ended);
        let asked = self.asked_to_exit.take();
        let now = self.clock.now_ms();
        let drain_ms = self.drain_ms();
        let quiescent = self.quiescent();
        let one_shot = self.spec.parent.is_some();
        let turn_running = matches!(self.phase, Phase::Working | Phase::NeedsYou);
        let phase = self.phase;
        let cause = match &mut self.mode {
            Mode::Exiting { .. } => None,
            _ if asked.is_some() => asked,
            Mode::Running => (turn_ended && one_shot && quiescent).then_some(ExitCause::Finished),
            Mode::Draining { why, ask_until } => {
                if turn_ended || !turn_running {
                    Some(why.cause())
                } else {
                    if *why == Drain::DaemonLost && phase == Phase::NeedsYou {
                        ask_until.get_or_insert(now + drain_ms);
                    } else {
                        *ask_until = None;
                    }
                    None
                }
            }
        };
        if let Some(cause) = cause {
            self.exit(cause).await?;
        }
        Ok(())
    }

    /// Nothing runs, nothing is queued and no accepted agent message waits
    /// to be consumed.
    fn quiescent(&self) -> bool {
        self.phase == Phase::Idle && self.queue_empty && I::pending_messages(&self.state).is_empty()
    }

    /// Writes the final boundary and asks the provider to finish.
    async fn exit(&mut self, cause: ExitCause) -> Result<(), AgentError> {
        self.feed(Event::Exiting {
            cause: cause.to_string(),
        })
        .await?;
        if let Some(provider) = &mut self.provider {
            provider.close();
        }
        self.mode = Mode::Exiting {
            cause,
            until: self.clock.now_ms() + PROVIDER_STOP_MS,
        };
        Ok(())
    }

    // --- the interpreter -------------------------------------------------

    async fn feed(&mut self, event: Event) -> Result<(), AgentError> {
        let now = self.clock.now_ms();
        if now > self.last_tick {
            self.last_tick = now;
            let stepped = I::step(&mut self.state, Event::Tick { at_ms: now });
            self.apply(stepped).await?;
        }
        let stepped = I::step(&mut self.state, event);
        self.apply(stepped).await
    }

    /// Blobs first, then the journal, then the effects: a reply is sent and
    /// a provider written to only once the step that explains it is on disk.
    async fn apply(&mut self, stepped: Stepped) -> Result<(), AgentError> {
        let Stepped { step, effects } = stepped;
        for effect in &effects {
            if let Effect::WriteBlob { hash, bytes } = effect {
                dir::write_blob(&self.dir, hash, bytes).map_err(AgentError::Journal)?;
            }
        }
        let before = self.journal.offset();
        self.journal.append(&step).map_err(AgentError::Journal)?;
        if let Some(snapshot) = &step.snapshot {
            self.phase = snapshot.phase();
            self.queue_empty = snapshot.queue.is_empty();
        }
        self.turn_ended |= step.turn_end.is_some();
        if self.journal.offset() > before {
            self.send(ctl_frame::Of::Nudge(Nudge {}));
        }
        for effect in effects {
            match effect {
                Effect::WriteBlob { .. } => {}
                Effect::Reply { input_id, verdict } => self.reply(input_id, verdict),
                Effect::Exit { cause } => {
                    self.asked_to_exit
                        .get_or_insert(ExitCause::Interpreter(cause));
                }
                effect => {
                    if let Some(provider) = &mut self.provider
                        && let Err(error) = provider.perform(effect).await
                    {
                        // A provider that stopped reading is about to report
                        // its exit, which is what ends the agent.
                        eprintln!("amux agent: writing to the provider: {error}");
                    }
                }
            }
        }
        Ok(())
    }

    // --- the daemon ------------------------------------------------------

    fn reply(&self, input_id: Vec<u8>, verdict: wire::SendInputResponse) {
        self.send(ctl_frame::Of::Reply(InputReply {
            input_id,
            verdict: Some(verdict),
        }));
    }

    /// Frames for a daemon that is not connected are dropped: whoever sent
    /// the input is gone with it.
    fn send(&self, of: ctl_frame::Of) {
        if let Some(daemon) = &self.daemon {
            let _ = daemon.frames.send(CtlFrame { of: Some(of) });
        }
    }

    fn grace_ms(&self) -> i64 {
        ms(
            self.spec
                .config
                .as_ref()
                .map_or(0, |config| config.grace_ms),
            DEFAULT_GRACE_MS,
        )
    }

    fn drain_ms(&self) -> i64 {
        ms(
            self.spec
                .config
                .as_ref()
                .map_or(0, |config| config.drain_ms),
            DEFAULT_DRAIN_MS,
        )
    }
}

fn ms(configured: u32, default: i64) -> i64 {
    match configured {
        0 => default,
        ms => i64::from(ms),
    }
}

async fn accept(mut listener: LocalListener, connections: mpsc::Sender<LocalStream>) {
    while let Ok(stream) = listener.accept().await {
        if connections.send(stream).await.is_err() {
            return;
        }
    }
}

async fn refuse(mut listener: LocalListener) {
    while let Ok(stream) = listener.accept().await {
        drop(stream);
    }
}

/// Each connection on hooks.sock carries one hook payload, then closes.
async fn hook_payloads(mut listener: LocalListener, facts: mpsc::Sender<ProviderEvent>) {
    while let Ok(mut stream) = listener.accept().await {
        let facts = facts.clone();
        tokio::spawn(async move {
            let mut payload = Vec::new();
            if stream.read_to_end(&mut payload).await.is_ok() && !payload.is_empty() {
                let _ = facts
                    .send(ProviderEvent::Fact(interpret::Fact {
                        channel: interpret::Channel::Hook,
                        payload,
                    }))
                    .await;
            }
        });
    }
}
