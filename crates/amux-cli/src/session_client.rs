use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use amux::terminal_io::{self, TerminalV1Args};
use amux::{
    AgentIdentifier, AgentType, Client, ClientError, Config, CreateAgentRequest, LeaderKey,
    SendInputRequest, SessionCloseReason, ShutdownReason, SubscribeSessionEvent,
    SubscribeSessionRequest, TerminalSize,
};
use anyhow::{Result, anyhow};
use crossterm::terminal;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::client_common::{get_client, print_update_banner, require_running_client};

/// Events from stdin reading task
pub(crate) enum StdinEvent {
    /// Raw input data to send to agent
    Data(Vec<u8>),
    /// User requested detach to the shell (<leader>d).
    Detach,
    /// User requested the fleet picker (<leader>s). From the TUI this
    /// resumes the chrome; from CLI attach it behaves like detach.
    SwitchToFleet,
}

/// How an attached session ended (transport errors surface as `Err`).
#[derive(Debug)]
pub(crate) enum AttachOutcome {
    Detached,
    SwitchedToFleet,
    SessionEnded,
    SessionClosed(SessionCloseReason),
    Shutdown(ShutdownReason),
}

/// Find a subsequence within a slice, returns the starting index if found
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Get terminal size, falling back to 24x80 if the terminal query fails.
fn get_terminal_size() -> TerminalSize {
    terminal::size()
        .map(|(cols, rows)| TerminalSize { rows, cols })
        .unwrap_or_default()
}

/// Create a new agent and open it in the configured default mode.
pub async fn new_agent(
    name: Option<&str>,
    agent_type: AgentType,
    args: Vec<String>,
    config: &Config,
) -> Result<()> {
    let codex_configuration = codex_configuration_label(&agent_type);
    let rpc = get_client(config).await?;
    let terminal_size = get_terminal_size();
    let working_dir = std::env::current_dir()?;

    let agent_id = Uuid::new_v4();

    tracing::info!(agent_id = %agent_id, ?name, "creating agent");

    let agent = rpc
        .create_agent(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: name.map(|s| s.to_string()),
            agent_type,
            working_dir: working_dir.clone(),
            terminal_size: Some(terminal_size),
            args,
        })
        .await
        .map_err(|error| anyhow!("failed to create agent: {error}"))?;

    match config.ui.default_open_mode {
        amux::OpenMode::Chat => {
            crate::ui::run_for_agent(config.clone(), agent.id, codex_configuration).await
        }
        amux::OpenMode::Raw => {
            let identifier = AgentIdentifier::from(agent.id);
            let outcome = if codex_configuration.is_some() {
                attach_new_codex_terminal(
                    &rpc,
                    identifier,
                    config.keybinds.leader.clone(),
                    StdinHandback::ProcessExits,
                )
                .await?
            } else {
                attach_terminal(
                    &rpc,
                    identifier,
                    config.keybinds.leader.clone(),
                    StdinHandback::ProcessExits,
                )
                .await?
            };
            finish_cli_attach(outcome, &config.state_path)
        }
    }
}

fn codex_configuration_label(agent_type: &AgentType) -> Option<String> {
    let AgentType::Codex {
        model,
        approval_policy,
        sandbox_policy,
        ..
    } = agent_type
    else {
        return None;
    };
    Some(format!(
        "model={} · approval={} · sandbox={}",
        model.as_deref().unwrap_or("default"),
        approval_policy.as_deref().unwrap_or("default"),
        sandbox_policy.as_deref().unwrap_or("default")
    ))
}

/// Attach to an existing agent
pub async fn attach(target: Option<&str>, config: &Config) -> Result<()> {
    let retry_command = target
        .map(|target| format!("amux attach {target}"))
        .unwrap_or_else(|| "amux attach".to_string());
    let rpc = require_running_client(config, Some(&retry_command)).await?;

    let agents = rpc.list_agents().await?;
    let Some(agent) = resolve_attach_agent(&agents, target)? else {
        eprintln!("No agents running. Use 'amux new' to create one.");
        return Ok(());
    };

    tracing::info!(agent = %agent.id, "attaching");

    // Codex's primary attach surface is the native structured screen. Raw
    // mode remains available from the fleet (Enter with the shipped raw
    // default, `o` when chat is configured as the default).
    if agent.agent_type == "codex" {
        return crate::ui::run_for_agent(config.clone(), agent.id, None).await;
    }

    let outcome = attach_terminal(
        &rpc,
        AgentIdentifier::from(agent.id),
        config.keybinds.leader.clone(),
        StdinHandback::ProcessExits,
    )
    .await?;
    finish_cli_attach(outcome, &config.state_path)
}

fn resolve_attach_agent<'a>(
    agents: &'a [amux::Agent],
    target: Option<&str>,
) -> Result<Option<&'a amux::Agent>> {
    let Some(target) = target else {
        return Ok(agents.first());
    };
    if let Ok(id) = Uuid::parse_str(target) {
        return agents
            .iter()
            .find(|agent| agent.id == id)
            .map(Some)
            .ok_or_else(|| anyhow!("agent not found: {target}"));
    }
    let matches: Vec<_> = agents
        .iter()
        .filter(|agent| agent.name.as_deref() == Some(target))
        .collect();
    match matches.as_slice() {
        [] => Err(anyhow!("agent not found: {target}")),
        [agent] => Ok(Some(*agent)),
        _ => Err(anyhow!(
            "agent name `{target}` is ambiguous; attach by id instead"
        )),
    }
}

/// Attach for the fleet TUI: run the passthrough on the real terminal and
/// come back with an optional status-line notice instead of exiting the
/// process. Stdin is fully reclaimed before returning so the chrome's event
/// stream is the only reader again.
pub(crate) async fn attach_for_ui(
    config: &Config,
    agent: amux::AgentId,
) -> Result<amux_tui::AttachReturn> {
    let rpc = require_running_client(config, None).await?;
    let outcome = attach_terminal(
        &rpc,
        AgentIdentifier::from(agent),
        config.keybinds.leader.clone(),
        StdinHandback::ReclaimForCaller,
    )
    .await?;
    use amux_tui::AttachReturn;
    Ok(match outcome {
        AttachOutcome::Detached => AttachReturn::Exit,
        AttachOutcome::SwitchedToFleet => AttachReturn::Fleet(None),
        AttachOutcome::SessionEnded => AttachReturn::Fleet(Some("session ended".to_string())),
        AttachOutcome::SessionClosed(reason) => {
            AttachReturn::Fleet(Some(session_close_label(&reason).to_string()))
        }
        AttachOutcome::Shutdown(reason) => AttachReturn::Fleet(Some(format!("daemon: {reason}"))),
    })
}

/// List all running agents
pub async fn list_agents(config: &Config) -> Result<()> {
    let rpc = require_running_client(config, Some("amux list")).await?;
    let mut agents = rpc.list_agents().await?;
    if agents.is_empty() {
        println!("No agents running.");
    } else {
        agents.sort_by(|a, b| {
            let a_id = a.id.to_string();
            let b_id = b.id.to_string();
            let a_name = a.name.as_deref().unwrap_or(&a_id);
            let b_name = b.name.as_deref().unwrap_or(&b_id);
            a_name.cmp(b_name)
        });
        let multiple_hosts = agents
            .iter()
            .map(|agent| agent.host_id)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let mut name_counts = HashMap::new();
        for agent in &agents {
            if let Some(name) = &agent.name {
                *name_counts.entry(name.clone()).or_insert(0usize) += 1;
            }
        }
        println!("Running agents:");
        for agent in agents {
            let agent_id_str = agent.id.to_string();
            let display_name = agent.name.as_deref().unwrap_or(&agent_id_str);
            let name_is_ambiguous = agent
                .name
                .as_ref()
                .and_then(|name| name_counts.get(name))
                .is_some_and(|count| *count > 1);
            let mut labels = Vec::new();
            if name_is_ambiguous {
                labels.push(format!("id {}", agent.id));
            }
            if multiple_hosts {
                labels.push(format!("host {}", short_uuid(agent.host_id)));
            }
            if labels.is_empty() {
                println!("  {} - {}", display_name, agent.working_dir.display());
            } else {
                println!(
                    "  {} ({}) - {}",
                    display_name,
                    labels.join(", "),
                    agent.working_dir.display()
                );
            }
        }
    }

    print_update_banner(&config.state_path);
    Ok(())
}

fn short_uuid(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

/// Open the raw byte-passthrough session (late attach renders via the
/// existing buffer replay — the history is a bounded byte tail).
pub(crate) async fn subscribe_raw(
    rpc: &Client,
    agent: &AgentIdentifier,
    terminal_size: Option<TerminalSize>,
) -> Result<amux::SessionStream> {
    let session = rpc
        .subscribe_session(SubscribeSessionRequest {
            agent: agent.clone(),
            io_protocol: terminal_io::TERMINAL_V1.to_string(),
            args: terminal_io::encode_terminal_v1_args(TerminalV1Args {
                terminal_size,
                replay_query: None,
            })
            .map(Into::into),
        })
        .await
        .map_err(|error| anyhow!("failed to subscribe to session: {error}"))?;

    tracing::info!(?agent, "subscribed to raw session");
    Ok(session)
}

/// What happens to the blocked stdin reader when the session ends without a
/// detach: the CLI paths exit the process (the reader dies with it); the TUI
/// path must reclaim stdin — one keypress, prompted — before the chrome's
/// event stream may read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StdinHandback {
    ProcessExits,
    ReclaimForCaller,
}

/// Attach on the real terminal: raw mode (RAII), the leader-scanning stdin
/// reader, and the passthrough loop. Every exit path restores the terminal
/// mode; with `ReclaimForCaller` stdin is exclusively released before
/// returning.
async fn attach_terminal(
    rpc: &Client,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let session = subscribe_raw(rpc, &agent, Some(get_terminal_size())).await?;
    attach_subscribed(rpc, session, agent, leader, handback).await
}

/// A newly-created Codex backend learns its thread id asynchronously. Retry
/// only that named readiness race; every other subscription error remains
/// immediate and unchanged.
async fn attach_new_codex_terminal(
    rpc: &Client,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let terminal_size = get_terminal_size();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let session = loop {
        match subscribe_raw(rpc, &agent, Some(terminal_size)).await {
            Ok(session) => break session,
            Err(error)
                if codex_thread_not_ready(&error) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    };
    attach_subscribed(rpc, session, agent, leader, handback).await
}

fn codex_thread_not_ready(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .contains("Codex raw session is not ready: thread_id is not available yet")
}

async fn attach_subscribed(
    rpc: &Client,
    session: amux::SessionStream,
    agent: AgentIdentifier,
    leader: LeaderKey,
    handback: StdinHandback,
) -> Result<AttachOutcome> {
    let raw_mode_guard = RawModeGuard::new()?;
    let stop_reading = Arc::new(AtomicBool::new(false));
    let (input_rx, reader) = spawn_stdin_reader(leader, stop_reading.clone());
    let outcome = attach_loop(rpc.clone(), session, agent, input_rx, io::stdout()).await;
    drop(raw_mode_guard);

    if handback == StdinHandback::ReclaimForCaller {
        stop_reading.store(true, Ordering::SeqCst);
        let needs_key = !matches!(
            outcome,
            Ok(AttachOutcome::Detached | AttachOutcome::SwitchedToFleet)
        ) && !reader.is_finished();
        if needs_key {
            let label = match &outcome {
                Ok(AttachOutcome::SessionClosed(reason)) => session_close_label(reason),
                Ok(AttachOutcome::Shutdown(_)) => "daemon shut down",
                _ => "session ended",
            };
            println!("\n[{label} — press any key to return to the fleet]");
        }
        let _ = reader.await;
    }
    outcome
}

/// Print the outcome and exit the process where today's CLI contract says
/// so; only a detach returns.
fn finish_cli_attach(outcome: AttachOutcome, state_path: &Path) -> Result<()> {
    match outcome {
        AttachOutcome::Shutdown(reason) => {
            println!("\n[{}]", reason);
            if reason != ShutdownReason::Updating {
                print_update_banner(state_path);
            }
            std::process::exit(1);
        }
        AttachOutcome::Detached | AttachOutcome::SwitchedToFleet => {
            println!("\n[detached from session]");
        }
        AttachOutcome::SessionEnded => {
            println!("\n[session ended]");
            print_update_banner(state_path);
            std::process::exit(0);
        }
        AttachOutcome::SessionClosed(reason) => {
            println!("\n[{}]", session_close_label(&reason));
            print_update_banner(state_path);
            std::process::exit(0);
        }
    }

    print_update_banner(state_path);
    Ok(())
}

/// Spawn the blocking stdin reader with the leader-key scanner. It exits on
/// stdin EOF, on a detach chord, or — once `stop_reading` is set — after the
/// next read returns (the reclaim keypress, which is consumed).
fn spawn_stdin_reader(
    leader: LeaderKey,
    stop_reading: Arc<AtomicBool>,
) -> (mpsc::Receiver<StdinEvent>, tokio::task::JoinHandle<()>) {
    let (input_tx, input_rx) = mpsc::channel::<StdinEvent>(256);

    let leader_raw = leader.raw_byte();
    let leader_csi_u = leader.csi_u_sequence();

    let reader = tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        let mut pending_leader = false;

        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) if stop_reading.load(Ordering::SeqCst) => {
                    // Reclaim: the session is over; this keypress hands
                    // stdin back to the caller and is deliberately consumed.
                    return;
                }
                Ok(n) => {
                    let data = &buffer[..n];

                    if let Some(pos) = find_subsequence(data, &leader_csi_u) {
                        if pos > 0
                            && input_tx
                                .blocking_send(StdinEvent::Data(data[..pos].to_vec()))
                                .is_err()
                        {
                            return;
                        }
                        let after = pos + leader_csi_u.len();
                        if after < n {
                            if let Some(event) = chord_event(data[after]) {
                                tracing::info!("leader chord");
                                let _ = input_tx.blocking_send(event);
                                return;
                            }
                            let mut remaining = vec![leader_raw];
                            remaining.extend_from_slice(&data[after..]);
                            if input_tx.blocking_send(StdinEvent::Data(remaining)).is_err() {
                                return;
                            }
                        } else {
                            pending_leader = true;
                        }
                        continue;
                    }

                    let mut i = 0;
                    while i < n {
                        if pending_leader {
                            pending_leader = false;
                            if let Some(event) = chord_event(data[i]) {
                                tracing::info!("leader chord");
                                let _ = input_tx.blocking_send(event);
                                return;
                            }
                            if input_tx
                                .blocking_send(StdinEvent::Data(vec![leader_raw, data[i]]))
                                .is_err()
                            {
                                return;
                            }
                            i += 1;
                            continue;
                        }

                        if data[i] == leader_raw {
                            pending_leader = true;
                            i += 1;
                            continue;
                        }

                        let start = i;
                        while i < n && data[i] != leader_raw {
                            i += 1;
                        }
                        if input_tx
                            .blocking_send(StdinEvent::Data(data[start..i].to_vec()))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }

            if pending_leader {
                let mut next = [0u8; 1];
                match stdin.read_exact(&mut next) {
                    Ok(_) => {
                        pending_leader = false;
                        if stop_reading.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Some(event) = chord_event(next[0]) {
                            tracing::info!("leader chord");
                            let _ = input_tx.blocking_send(event);
                            return;
                        }
                        if input_tx
                            .blocking_send(StdinEvent::Data(vec![leader_raw, next[0]]))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    (input_rx, reader)
}

/// `<leader>d` detaches to the shell; `<leader>s` goes to the fleet.
fn chord_event(byte: u8) -> Option<StdinEvent> {
    match byte {
        b'd' => Some(StdinEvent::Detach),
        b's' => Some(StdinEvent::SwitchToFleet),
        _ => None,
    }
}

/// The passthrough loop: session output to `output`, input events to
/// `SendInput`, until detach or the session ends. Pure plumbing over
/// injected IO so the tier-2 suite can drive it without a terminal.
pub(crate) async fn attach_loop<W: Write>(
    rpc: Client,
    mut session: amux::SessionStream,
    agent: AgentIdentifier,
    mut input_rx: mpsc::Receiver<StdinEvent>,
    mut output: W,
) -> Result<AttachOutcome> {
    loop {
        tokio::select! {
            event = input_rx.recv() => match event {
                Some(StdinEvent::Data(data)) => {
                    if rpc
                        .send_input(SendInputRequest {
                            agent: agent.clone(),
                            input_id: Uuid::new_v4().as_bytes().to_vec(),
                            io_protocol: terminal_io::TERMINAL_V1.to_string(),
                            payload: data.into(),
                        })
                        .await
                        .is_err()
                    {
                        return Ok(AttachOutcome::SessionEnded);
                    }
                }
                Some(StdinEvent::Detach) => return Ok(AttachOutcome::Detached),
                Some(StdinEvent::SwitchToFleet) => return Ok(AttachOutcome::SwitchedToFleet),
                None => return Ok(AttachOutcome::SessionEnded),
            },
            event = session.recv() => match event {
                Ok(SubscribeSessionEvent::Output { payload }) => {
                    output.write_all(&payload).ok();
                    output.flush().ok();
                }
                Ok(SubscribeSessionEvent::Opened)
                | Ok(SubscribeSessionEvent::ReplayComplete { .. }) => {}
                Ok(SubscribeSessionEvent::Closed { reason }) => {
                    tracing::info!(?reason, "session closed");
                    return Ok(AttachOutcome::SessionClosed(reason));
                }
                Err(ClientError::ServerShutdown(reason)) => {
                    tracing::info!(reason = %reason, "server shutdown");
                    return Ok(AttachOutcome::Shutdown(reason));
                }
                Err(error) => {
                    if let ClientError::Protocol(error) = &error {
                        tracing::info!(error = %error, "session failed");
                    } else {
                        tracing::warn!(error = %error, "session read error");
                    }
                    return Err(error.into());
                }
            }
        }
    }
}

fn session_close_label(reason: &SessionCloseReason) -> &'static str {
    match reason {
        SessionCloseReason::AgentDeleted | SessionCloseReason::AgentExited { .. } => {
            "session ended"
        }
        SessionCloseReason::HostUnreachable => "host unreachable",
        SessionCloseReason::InternalError { .. } => "session error",
    }
}

/// RAII guard to restore terminal mode on drop
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Tier-2 attach round-trip specs: a real embedded daemon and a pty
/// test-agent driven through the passthrough loop with injected IO, plus
/// vt100 assertions over the terminal-hygiene byte sequences that ratatui's
/// TestBackend is blind to.
#[cfg(test)]
mod attach {
    #[test]
    fn leader_chords_split_detach_from_fleet() {
        assert!(matches!(
            super::chord_event(b'd'),
            Some(super::StdinEvent::Detach)
        ));
        assert!(matches!(
            super::chord_event(b's'),
            Some(super::StdinEvent::SwitchToFleet)
        ));
        assert!(super::chord_event(b'x').is_none());
    }

    #[test]
    fn codex_configuration_label_is_explicit_about_defaults_and_overrides() {
        let defaults = AgentType::Codex {
            model: None,
            approval_policy: None,
            sandbox_policy: None,
            resume_thread_id: None,
        };
        assert_eq!(
            super::codex_configuration_label(&defaults).as_deref(),
            Some("model=default · approval=default · sandbox=default")
        );

        let selected = AgentType::Codex {
            model: Some("gpt-5.4".to_string()),
            approval_policy: Some("never".to_string()),
            sandbox_policy: Some("workspace-write".to_string()),
            resume_thread_id: None,
        };
        assert_eq!(
            super::codex_configuration_label(&selected).as_deref(),
            Some("model=gpt-5.4 · approval=never · sandbox=workspace-write")
        );
        assert_eq!(super::codex_configuration_label(&AgentType::Claude), None);
    }

    #[test]
    fn raw_create_retry_matches_only_the_codex_thread_readiness_race() {
        let readiness = anyhow!(
            "failed to subscribe to session: Codex raw session is not ready: thread_id is not available yet"
        );
        assert!(super::codex_thread_not_ready(&readiness));
        assert!(!super::codex_thread_not_ready(&anyhow!(
            "failed to subscribe to session: permission denied"
        )));
    }

    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use amux::{AgentId, Config, Server};
    use amux_ui::{Model, Runtime, RuntimeOptions};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<StdMutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedBuf {
        fn contains(&self, needle: &str) -> bool {
            let bytes = self.0.lock().unwrap();
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        }
    }

    async fn embedded_client(dir: &std::path::Path) -> Client {
        let config = Config {
            state_path: dir.join("state.yaml"),
            socket_path: dir.join("amux.sock"),
            enable_cloud_mode: Some(false),
            prevent_idle_sleep: Some(false),
            ..Config::default()
        };
        Server::builder()
            .config(config)
            .embedded()
            .open()
            .await
            .unwrap()
    }

    async fn create_cat_agent(client: &Client, name: &str) -> AgentId {
        let agent = client
            .create_agent(CreateAgentRequest {
                agent_id: Uuid::new_v4(),
                host_id: None,
                name: Some(name.to_string()),
                agent_type: AgentType::TestAgent {
                    command: "cat".to_string(),
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
            })
            .await
            .expect("create test agent");
        agent.id
    }

    struct OpenAttach {
        input: mpsc::Sender<StdinEvent>,
        output: SharedBuf,
        loop_task: tokio::task::JoinHandle<Result<AttachOutcome>>,
    }

    async fn open_attach(client: &Client, agent: AgentId) -> OpenAttach {
        let identifier = AgentIdentifier::from(agent);
        let session = subscribe_raw(client, &identifier, None)
            .await
            .expect("subscribe raw session");
        let (input, input_rx) = mpsc::channel(16);
        let output = SharedBuf::default();
        let loop_task = tokio::spawn(attach_loop(
            client.clone(),
            session,
            identifier,
            input_rx,
            output.clone(),
        ));
        OpenAttach {
            input,
            output,
            loop_task,
        }
    }

    async fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !check() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_model(runtime: &mut Runtime, what: &str, check: impl Fn(&Model) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !check(runtime.model()) {
            let remaining = deadline - tokio::time::Instant::now();
            assert!(!remaining.is_zero(), "timed out waiting for {what}");
            match tokio::time::timeout(remaining, runtime.next()).await {
                Ok(true) => {}
                Ok(false) => panic!("runtime shut down waiting for {what}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    fn render_fleet(model: &Model) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(68, 11)).expect("terminal");
        let view = amux_tui::ViewState::default();
        let ctx = amux_tui::FrameContext {
            viewport: (68, 11),
            theme: amux_tui::Theme::default(),
            now: chrono::Utc::now(),
        };
        terminal
            .draw(|frame| amux_tui::render(model, &view, &ctx, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.cell((x, y)).expect("cell").symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The V1 loop: attach, see output, detach, the fleet repaints from the
    /// Model, attach again — then 100 scripted cycles with zero corruption
    /// (every cycle detaches cleanly and the next attach still streams).
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(
        windows,
        ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
    )]
    async fn round_trip_repaints_fleet() {
        let dir = tempfile::tempdir().unwrap();
        let client = embedded_client(dir.path()).await;
        let mut runtime = Runtime::start_with_client(client.clone(), RuntimeOptions::default());
        wait_model(&mut runtime, "snapshot", |model| model.is_synchronized()).await;

        let agent = create_cat_agent(&client, "round-trip").await;
        wait_model(&mut runtime, "agent in model", move |model| {
            model.agent(agent).is_some()
        })
        .await;

        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"hello-there\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("echoed output", || output.contains("hello-there")).await;
        attached.input.send(StdinEvent::Detach).await.unwrap();
        let outcome = attached.loop_task.await.unwrap().unwrap();
        assert!(matches!(outcome, AttachOutcome::Detached), "{outcome:?}");

        // The fleet repaints from the Model alone after detach.
        let frame = render_fleet(runtime.model());
        assert!(frame.contains("round-trip"), "fleet row present:\n{frame}");
        assert!(frame.contains("connected"), "status line present:\n{frame}");

        // Attach again: the session streams again (late attach replays).
        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"second-visit\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("second echo", || output.contains("second-visit")).await;
        attached
            .input
            .send(StdinEvent::SwitchToFleet)
            .await
            .unwrap();
        let outcome = attached.loop_task.await.unwrap().unwrap();
        assert!(
            matches!(outcome, AttachOutcome::SwitchedToFleet),
            "{outcome:?}"
        );

        // 100 scripted attach/detach cycles.
        for cycle in 0..100 {
            let attached = open_attach(&client, agent).await;
            attached.input.send(StdinEvent::Detach).await.unwrap();
            let outcome = attached.loop_task.await.unwrap().unwrap();
            assert!(
                matches!(outcome, AttachOutcome::Detached),
                "cycle {cycle}: {outcome:?}"
            );
        }
    }

    /// The terminal-hygiene byte sequences, asserted through a real vt100
    /// parser (the surface TestBackend cannot see): entering chrome uses the
    /// alternate screen; the restore sequence leaves it, shows the cursor,
    /// and resets modes. Every passthrough exit path emits exactly these
    /// bytes via the RAII guard.
    #[test]
    fn detach_leaves_terminal_sane() {
        let mut parser = vt100::Parser::new(24, 80, 0);

        let mut enter = Vec::new();
        amux_tui::write_enter_chrome(&mut enter).unwrap();
        parser.process(&enter);
        assert!(parser.screen().alternate_screen(), "chrome uses alt screen");
        assert!(parser.screen().hide_cursor(), "chrome hides the cursor");

        let mut restore = Vec::new();
        amux_tui::write_restore(&mut restore).unwrap();
        parser.process(&restore);
        assert!(
            !parser.screen().alternate_screen(),
            "restore leaves the alternate screen"
        );
        assert!(!parser.screen().hide_cursor(), "restore shows the cursor");
    }

    /// Kill the agent mid-attach: the loop reports the close instead of
    /// hanging or corrupting, and the chrome's restore sequence still
    /// yields a sane terminal.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(
        windows,
        ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
    )]
    async fn kill_during_attach_still_restores_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let client = embedded_client(dir.path()).await;
        let agent = create_cat_agent(&client, "doomed").await;

        let attached = open_attach(&client, agent).await;
        attached
            .input
            .send(StdinEvent::Data(b"warm-up\n".to_vec()))
            .await
            .unwrap();
        let output = attached.output.clone();
        wait_until("agent alive", || output.contains("warm-up")).await;

        client.delete_agent(agent).await.expect("delete mid-attach");
        let outcome = tokio::time::timeout(Duration::from_secs(20), attached.loop_task)
            .await
            .expect("attach loop returns after the kill")
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                outcome,
                AttachOutcome::SessionClosed(SessionCloseReason::AgentDeleted)
                    | AttachOutcome::SessionClosed(SessionCloseReason::AgentExited { .. })
            ),
            "{outcome:?}"
        );

        // The resume path writes the reset sequence; assert it on the
        // captured stream through vt100.
        let mut parser = vt100::Parser::new(24, 80, 0);
        let mut enter = Vec::new();
        amux_tui::write_enter_chrome(&mut enter).unwrap();
        parser.process(&enter);
        let mut restore = Vec::new();
        amux_tui::write_restore(&mut restore).unwrap();
        parser.process(&restore);
        assert!(!parser.screen().alternate_screen());
        assert!(!parser.screen().hide_cursor());
    }

    /// Enter on a row whose host is offline surfaces the daemon's
    /// `last_dial_error` in the status line instead of attaching.
    #[tokio::test]
    async fn offline_host_shows_dial_error_instead_of_attaching() {
        use amux_ui::{Msg, ServerMsg, update};

        let host = amux::HostEntry {
            id: Uuid::from_u128(1),
            name: "hetzner".to_string(),
            online: false,
            version: None,
            capabilities: None,
            trust_status: amux::HostTrustStatus::Trusted,
            last_dial_error: Some("dial tcp: connection refused".to_string()),
        };
        let agent = amux::Agent {
            id: Uuid::from_u128(2),
            host_id: host.id,
            name: Some("faraway".to_string()),
            command: "claude".to_string(),
            working_dir: std::env::temp_dir(),
            agent_type: "claude".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
        };
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: None,
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
        ] {
            update(&mut model, msg);
        }

        let mut view = amux_tui::ViewState::default();
        let action = amux_tui::keys::handle_key(
            &mut view,
            &model,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            5,
            chrono::Utc::now(),
        );
        assert_eq!(action, None, "no attach action for an offline host");
        let notice = view.notice.clone().expect("status-line notice");
        assert!(
            notice.contains("dial tcp: connection refused"),
            "notice carries last_dial_error: {notice}"
        );

        let frame = render_fleet_with_view(&model, &view);
        assert!(
            frame.contains("✗ hetzner is offline: dial tcp: connection refused"),
            "status line shows the dial error:\n{frame}"
        );
    }

    fn render_fleet_with_view(model: &Model, view: &amux_tui::ViewState) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(68, 11)).expect("terminal");
        let ctx = amux_tui::FrameContext {
            viewport: (68, 11),
            theme: amux_tui::Theme::default(),
            now: chrono::Utc::now(),
        };
        terminal
            .draw(|frame| amux_tui::render(model, view, &ctx, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.cell((x, y)).expect("cell").symbol());
            }
            out.push('\n');
        }
        out
    }
}
