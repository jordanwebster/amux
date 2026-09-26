//! One profile runtime: a complete amux with its own host id, store and
//! agents. This module is its agent registry: spawn, resume, stop, delete
//! and rename, and the startup sweep that rebuilds the registry from disk.
//!
//! The registry keeps one [`AgentHandle`] per agent whose process is live
//! (or whose directory is locked by one): the directory, the control
//! connection, and the tools socket the agent's MCP server dials. Nothing
//! here is persisted; after a restart the sweep rebuilds it from the agent
//! directories and the store's rows.
//!
//! Each handle has one watcher task. It dials `ctl.sock`, reads the Hello,
//! ingests on every Nudge, and when the connection ends works out whether
//! the process is gone (its lock is free) or the connection merely
//! dropped (it redials). A process that is gone has its journal remainder
//! ingested, its row marked exited and its tools socket removed.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agent_dir::Clock;
use agent_dir::local_socket::{self, LocalListener, LocalStream};
use store::{
    AgentKey, AgentRow, Backend as _, CommitClock, Committed, Marker, Record, Sqlite, Store as _,
    StoreError,
};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Agent, AgentHello, AgentParent, AgentRemoved, CaughtUp, CreateAgentRequest, CtlFrame,
    DeleteAgentResponse, Input, Kind, Lifecycle, Stop, StopMode, WorkingOn, ctl_frame,
    inventory_event, session_event,
};

use crate::fanout::Fanout;
use crate::install::{AGENTS, private_dir};
use crate::profiles::ProfileId;
use crate::serve::{event, inventory};
use crate::spec;

pub type AgentId = Uuid;

/// What [`ProfileRuntime::set_join_hook`] installs.
pub type JoinHook =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// How long between looks at a directory whose process is starting,
/// stopping or not answering: short, because a start and a resume wait on
/// it, and each look is one lock attempt and one dial.
const PROBE_INTERVAL: Duration = Duration::from_millis(20);
/// How long the sweep waits for a locked directory's process to answer
/// before leaving it to its watcher.
const LOOK_PATIENCE: Duration = Duration::from_secs(2);
/// How long a connected agent has to send its Hello.
const HELLO_PATIENCE: Duration = Duration::from_secs(5);
/// How long a stop waits after killing the process group before it gives
/// up on seeing the lock released.
const KILL_PATIENCE: Duration = Duration::from_secs(5);
/// Fully ingested journal segments kept below the cursor, for dumps.
pub const KEPT_SEGMENTS: usize = 2;

/// The exit causes the daemon records. It knows only what it asked for and
/// what it saw; the agent's own account of its exit is its final boundary
/// item.
pub const CAUSE_STOPPED: &str = "stopped";
pub const CAUSE_ABORTED: &str = "aborted";
pub const CAUSE_KILLED: &str = "killed";
pub const CAUSE_EXITED: &str = "exited";
pub const CAUSE_EXITED_AWAY: &str = "exited while the daemon was away";
pub const CAUSE_NO_DIRECTORY: &str = "its directory is gone";
pub const CAUSE_UNSTARTED: &str = "the agent process could not start";

/// What the daemon starts agents with; read at every spawn and resume, so
/// a change affects the next incarnation and never a running agent.
#[derive(Clone, Debug)]
pub struct Launch {
    /// The canonical install path: `amux agent <dir>` runs from it, and the
    /// agent's hooks and tool server run from it too.
    pub install_path: PathBuf,
    pub claude_command: String,
    pub codex_command: String,
    /// Environment additions for every provider child.
    pub provider_env: HashMap<String, String>,
    pub agent: settings::AgentSettings,
    /// The agent's journal segment size; its default when zero.
    pub journal_segment_bytes: u64,
    /// How long a new process has to take its lock and send its Hello.
    pub start_deadline_ms: i64,
    /// How long a stop may take before the daemon kills the process group.
    pub stop_deadline_ms: i64,
    /// How long a notification waits before it is sent, so an answer from
    /// another client cancels it.
    pub notify_delay_ms: i64,
    /// How far a subscription may fall behind its agent's broadcast before
    /// it is closed with Lagged. Read when a runtime opens.
    pub fanout_capacity: usize,
    /// The same for inventory subscriptions.
    pub inventory_capacity: usize,
}

impl Default for Launch {
    fn default() -> Self {
        Self {
            install_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("amux")),
            claude_command: "claude".to_owned(),
            codex_command: "codex".to_owned(),
            provider_env: HashMap::new(),
            agent: settings::AgentSettings::default(),
            journal_segment_bytes: 0,
            start_deadline_ms: 20_000,
            stop_deadline_ms: 30_000,
            notify_delay_ms: 30_000,
            fanout_capacity: 512,
            inventory_capacity: 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no agent {0}")]
    NotFound(AgentId),
    #[error("an agent with id {0} exists")]
    AlreadyExists(AgentId),
    #[error("agent {0} is live; stop it before resuming it")]
    Live(AgentId),
    #[error("an agent id is 16 bytes, not {0}")]
    BadId(usize),
    #[error("agents of kind {0} are unknown")]
    UnknownKind(i32),
    #[error("the working directory {0} is not a directory on this host")]
    BadCwd(String),
    #[error("spawning on another host goes through that host's daemon")]
    OtherHost,
    #[error("agent {0} did not start within the start deadline")]
    StartTimeout(AgentId),
    #[error("agent {0} did not stop within the stop deadline")]
    StopTimeout(AgentId),
    #[error("agent {0}'s previous process still holds its directory")]
    StillLocked(AgentId),
    #[error("agent {0} has no spec to resume from")]
    NoSpec(AgentId),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
    #[error("the agent directory: {0}")]
    Io(#[from] io::Error),
}

/// What the sweep found, each agent listed once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Locked directories whose process answered with a Hello.
    pub live: Vec<AgentId>,
    /// Locked directories whose process did not answer yet; their watcher
    /// keeps trying.
    pub unanswered: Vec<AgentId>,
    /// Rows the sweep found with no process: marked exited, their journal
    /// remainder ingested and their CaughtUp flag set.
    pub exited: Vec<AgentId>,
    /// Directories with no row: what a delete left when it stopped between
    /// removing the rows and the directory. Removed.
    pub removed: Vec<AgentId>,
}

/// The read-only half of the sweep, done before activation.
pub struct Looked {
    agents: Vec<(AgentId, Look)>,
    orphans: Vec<AgentId>,
}

enum Look {
    Live(Box<Connected>),
    Unanswered,
    Exited,
    NoDirectory,
}

struct Connected {
    reader: ReadHalf<LocalStream>,
    writer: WriteHalf<LocalStream>,
    hello: AgentHello,
}

pub struct ProfileRuntime {
    profile: ProfileId,
    host: Uuid,
    /// The installation's generation, carried on this host's entry.
    generation: u64,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    launch: Mutex<Launch>,
    /// Every write publishes to `fanout` while it still holds this lock, so
    /// subscribers see changes in commit order and a subscribe that holds
    /// it reads one point in that order.
    pub(crate) store: tokio::sync::Mutex<Sqlite>,
    pub(crate) fanout: Fanout,
    /// For tests: awaited by a subscribe between joining the channel and
    /// reading the cut, with the store held.
    pub(crate) join_hook: Mutex<Option<JoinHook>>,
    agents: Mutex<HashMap<AgentId, Arc<AgentHandle>>>,
    /// One operation at a time per agent: spawn, resume, stop and delete
    /// each finish before the next starts.
    operations: Mutex<HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>,
    me: Weak<ProfileRuntime>,
}

struct AgentHandle {
    dir: PathBuf,
    /// The write half of the control connection, while one is open.
    ctl: tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>,
    hello: watch::Sender<Option<AgentHello>>,
    /// The stop the daemon asked for, which names the exit's cause.
    stopping: Mutex<Option<StopMode>>,
    /// The agent answered an input "exiting": it is on its way out.
    exiting: AtomicBool,
    /// The process id, when this daemon started the process.
    pid: Option<u32>,
    /// Set at each Hello and cleared when ingest next reaches the end of
    /// the journal, which is when CaughtUp is broadcast: once per Hello.
    caught_up_due: AtomicBool,
    /// Whether the process this daemon started is still running. A new
    /// process has not taken its lock yet, so a free lock says nothing
    /// about it until it has exited.
    running: watch::Sender<bool>,
    exited: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AgentHandle {
    fn new(dir: PathBuf, pid: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            ctl: tokio::sync::Mutex::new(None),
            hello: watch::Sender::new(None),
            stopping: Mutex::new(None),
            exiting: AtomicBool::new(false),
            caught_up_due: AtomicBool::new(false),
            pid,
            running: watch::Sender::new(pid.is_some()),
            exited: watch::Sender::new(false),
            tasks: Mutex::new(Vec::new()),
        })
    }

    fn abort(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

impl Drop for ProfileRuntime {
    fn drop(&mut self) {
        for handle in self.agents.get_mut().unwrap().values() {
            handle.abort();
        }
    }
}

impl ProfileRuntime {
    /// Opens the profile's store, applying any migrations it lacks. Writes
    /// nothing else.
    pub fn open(
        profile: ProfileId,
        host: Uuid,
        generation: u64,
        dir: PathBuf,
        store: Sqlite,
        launch: Launch,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            profile,
            host,
            generation,
            dir,
            clock,
            fanout: Fanout::new(launch.fanout_capacity, launch.inventory_capacity),
            join_hook: Mutex::new(None),
            launch: Mutex::new(launch),
            store: tokio::sync::Mutex::new(store),
            agents: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            me: me.clone(),
        })
    }

    pub fn profile(&self) -> ProfileId {
        self.profile
    }

    pub fn host(&self) -> Uuid {
        self.host
    }

    /// The installation's generation as of this run's start.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The store, held exclusively while the guard lives.
    pub async fn store(&self) -> tokio::sync::MutexGuard<'_, Sqlite> {
        self.store.lock().await
    }

    /// For tests: runs `hook` inside every subscribe between joining the
    /// agent's channel and reading the cut, so a test can try to commit in
    /// that window.
    #[doc(hidden)]
    pub fn set_join_hook(&self, hook: Option<JoinHook>) {
        *self.join_hook.lock().unwrap() = hook;
    }

    /// Replaces what the next spawn or resume starts with.
    pub fn set_launch(&self, launch: Launch) {
        *self.launch.lock().unwrap() = launch;
    }

    pub fn launch(&self) -> Launch {
        self.launch.lock().unwrap().clone()
    }

    pub fn agent_dir(&self, id: AgentId) -> PathBuf {
        self.dir.join(AGENTS).join(id.to_string())
    }

    fn key(&self, id: AgentId) -> AgentKey {
        AgentKey::new(self.host.as_bytes().to_vec(), id.as_bytes().to_vec())
    }

    /// The inventory row for an agent.
    pub async fn agent(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let row = self
            .store
            .lock()
            .await
            .agent(&self.key(id))?
            .ok_or(RegistryError::NotFound(id))?;
        Ok(to_wire(&row))
    }

    /// Own agents whose process this runtime is connected to or waiting on.
    pub fn live(&self) -> Vec<AgentId> {
        self.agents.lock().unwrap().keys().copied().collect()
    }

    /// The Hello of an agent's current connection, once it has one.
    pub fn hello(&self, id: AgentId) -> Option<AgentHello> {
        let handle = self.agents.lock().unwrap().get(&id).cloned()?;
        handle.hello.borrow().clone()
    }

    /// Marks a live agent as leaving: it answered an input "exiting", so a
    /// resume waits for its lock instead of refusing it as live.
    pub fn mark_exiting(&self, id: AgentId) {
        if let Some(handle) = self.agents.lock().unwrap().get(&id) {
            handle.exiting.store(true, Ordering::SeqCst);
        }
    }

    fn operation(&self, id: AgentId) -> Arc<tokio::sync::Mutex<()>> {
        self.operations
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .clone()
    }

    // --- spawn and resume ------------------------------------------------

    /// Creates an agent: its row, its directory and `spec.1`, its tools
    /// socket, and its process. `caller` is the agent whose tools socket
    /// the request arrived on; it becomes the parent. Returns once the
    /// process has sent its Hello, or has already exited.
    pub async fn spawn(
        &self,
        request: CreateAgentRequest,
        caller: Option<AgentId>,
    ) -> Result<Agent, RegistryError> {
        let id = match request.agent_id.len() {
            0 => Uuid::new_v4(),
            16 => Uuid::from_slice(&request.agent_id).expect("sixteen bytes"),
            n => return Err(RegistryError::BadId(n)),
        };
        if request
            .host_id
            .as_ref()
            .is_some_and(|host| host.as_slice() != self.host.as_bytes())
        {
            return Err(RegistryError::OtherHost);
        }
        let kind = Kind::try_from(request.kind).unwrap_or(Kind::Unspecified);
        let Some(kind_name) = spec::kind_name(kind) else {
            return Err(RegistryError::UnknownKind(request.kind));
        };
        if request.cwd.is_empty() || !Path::new(&request.cwd).is_dir() {
            return Err(RegistryError::BadCwd(request.cwd));
        }
        let parent = match caller {
            Some(caller) => Some(AgentParent {
                host_id: self.host.as_bytes().to_vec(),
                agent_id: caller.as_bytes().to_vec(),
            }),
            None => request.parent.clone(),
        };

        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        let now = self.clock.now_ms();
        {
            let mut store = self.store.lock().await;
            if store.agent(&key)?.is_some() {
                return Err(RegistryError::AlreadyExists(id));
            }
            // The row comes first: a crash after it leaves a row the sweep
            // marks exited, never a directory nothing lists.
            let mut row = AgentRow::new(key.clone(), kind_name, request.cwd.clone());
            row.name = request.name.clone().filter(|name| !name.is_empty());
            row.parent = parent
                .as_ref()
                .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
            row.created_at = now;
            row.incarnation = 1;
            self.put_row(&mut store, &row)?;
        }

        let dir = self.agent_dir(id);
        private_dir(&dir)?;
        let launch = self.launch();
        let resolved = spec::resolve(&request);
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind,
                cwd: &request.cwd,
                name: request.name.as_deref().unwrap_or_default(),
                parent,
                resolved: &resolved,
                created_at_ms: now,
                incarnation: 1,
                initial_prompt: request.initial_prompt.clone(),
            },
        );
        spec::write(&dir, &spec)?;
        self.start_process(id, &dir, &launch).await
    }

    /// Starts the agent's next incarnation: waits for a dying process to
    /// release the directory, writes `spec.<n+1>` from the current
    /// configuration with `prompt` as its first input, and spawns it.
    pub async fn resume(&self, id: AgentId, prompt: Option<Input>) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        let mut row = self
            .store
            .lock()
            .await
            .agent(&key)?
            .ok_or(RegistryError::NotFound(id))?;
        let launch = self.launch();
        let handle = self.agents.lock().unwrap().get(&id).cloned();
        if let Some(handle) = handle {
            let leaving =
                handle.exiting.load(Ordering::SeqCst) || handle.stopping.lock().unwrap().is_some();
            if !leaving {
                return Err(RegistryError::Live(id));
            }
            self.wait_exit(&handle, launch.stop_deadline_ms)
                .await
                .map_err(|_| RegistryError::StillLocked(id))?;
        }
        let dir = self.agent_dir(id);
        // The process may be gone from the registry and still dying: the
        // new one cannot take a lock the old one holds.
        self.wait_unlocked(&dir, launch.stop_deadline_ms)
            .await
            .map_err(|_| RegistryError::StillLocked(id))?;

        let previous = spec::newest(&dir)?.ok_or(RegistryError::NoSpec(id))?;
        let next = previous.incarnation + 1;
        let resolved = spec::Resolved {
            provider_args: previous.provider_args.clone(),
            model: previous.config.as_ref().and_then(|c| c.model.clone()),
            permission_mode: previous
                .config
                .as_ref()
                .and_then(|c| c.permission_mode.clone()),
        };
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind: spec::kind_from_name(&previous.kind),
                cwd: &previous.cwd,
                name: &previous.name,
                parent: previous.parent.clone(),
                resolved: &resolved,
                created_at_ms: self.clock.now_ms(),
                incarnation: next,
                initial_prompt: prompt,
            },
        );
        spec::write(&dir, &spec)?;
        {
            let mut store = self.store.lock().await;
            row.lifecycle = Lifecycle::Live as i32;
            row.exit_cause = None;
            row.incarnation = next;
            self.put_row(&mut store, &row)?;
            // The next CaughtUp comes from this incarnation's journal.
            store.set_marker(&key, None);
        }
        self.start_process(id, &dir, &launch).await
    }

    /// Binds the tools socket, spawns `amux agent <dir>`, and waits for its
    /// Hello or its exit.
    async fn start_process(
        &self,
        id: AgentId,
        dir: &Path,
        launch: &Launch,
    ) -> Result<Agent, RegistryError> {
        let tools = self.bind_tools(id, dir)?;
        let child = match spawn_agent(&launch.install_path, dir) {
            Ok(child) => child,
            Err(error) => {
                drop(tools);
                self.mark_exited(id, CAUSE_UNSTARTED).await?;
                return Err(error.into());
            }
        };
        let pid = child.id();
        let handle = AgentHandle::new(dir.to_owned(), pid);
        handle.tasks.lock().unwrap().push(tools);
        let running = handle.running.clone();
        handle.tasks.lock().unwrap().push(tokio::spawn(async move {
            reap(child).await;
            running.send_replace(false);
        }));
        self.agents.lock().unwrap().insert(id, handle.clone());
        self.watch(id, handle.clone(), None);

        let mut hello = handle.hello.subscribe();
        let mut exited = handle.exited.subscribe();
        let deadline = self
            .clock
            .sleep_until(self.clock.now_ms() + launch.start_deadline_ms);
        tokio::select! {
            _ = hello.wait_for(Option::is_some) => {}
            _ = exited.wait_for(|exited| *exited) => {}
            () = deadline => {
                self.kill(&handle);
                return Err(RegistryError::StartTimeout(id));
            }
        }
        self.agent(id).await
    }

    /// Listens on `<dir>/tools.sock` for the agent's MCP server. The socket
    /// is its caller's identity: whatever arrives on it is from `id`.
    fn bind_tools(&self, id: AgentId, dir: &Path) -> io::Result<JoinHandle<()>> {
        let mut listener = LocalListener::bind(&dir.join(agent_dir::TOOLS_SOCK))?;
        let runtime = self.me.clone();
        Ok(tokio::spawn(async move {
            while let Ok(stream) = listener.accept().await {
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                runtime.tools_connection(id, stream);
            }
        }))
    }

    /// One connection from an agent's tool server.
    fn tools_connection(&self, caller: AgentId, stream: LocalStream) {
        // No service answers on the tools socket yet; closing the
        // connection makes the tool server report the daemon unavailable
        // rather than wait on a call nobody reads.
        tracing::debug!(agent = %caller, "closing a tools connection with no service to serve it");
        drop(stream);
    }

    // --- stop, delete, rename --------------------------------------------

    /// Stops a live agent's process: graceful finishes the turn, abort
    /// cancels it, kill ends the process group at once. A stop that
    /// overruns the stop deadline kills. An exited agent is left as it is.
    pub async fn stop(&self, id: AgentId, mode: StopMode) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        self.stop_locked(id, mode).await
    }

    async fn stop_locked(&self, id: AgentId, mode: StopMode) -> Result<Agent, RegistryError> {
        let Some(handle) = self.agents.lock().unwrap().get(&id).cloned() else {
            return self.agent(id).await;
        };
        *handle.stopping.lock().unwrap() = Some(mode);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Stop(Stop { mode: mode as i32 })),
        };
        if let Some(ctl) = handle.ctl.lock().await.as_mut() {
            // A failed write means the connection is already going; the
            // deadline below still applies.
            let _ = agent_dir::write_frame(ctl, &frame).await;
        }
        let deadline = self.launch().stop_deadline_ms;
        if self.wait_exit(&handle, deadline).await.is_err() {
            *handle.stopping.lock().unwrap() = Some(StopMode::Kill);
            if !self.kill(&handle) {
                return Err(RegistryError::StopTimeout(id));
            }
            self.wait_exit(&handle, KILL_PATIENCE.as_millis() as i64)
                .await
                .map_err(|_| RegistryError::StopTimeout(id))?;
        }
        self.agent(id).await
    }

    /// Deletes an agent: aborts its process if live, deletes its children
    /// on this host the same way, then removes its rows and its directory.
    /// Children on other hosts are reported, not reached.
    pub async fn delete(&self, id: AgentId) -> Result<DeleteAgentResponse, RegistryError> {
        let mut response = DeleteAgentResponse::default();
        self.delete_into(id, &mut response).await?;
        Ok(response)
    }

    async fn delete_into(
        &self,
        id: AgentId,
        response: &mut DeleteAgentResponse,
    ) -> Result<(), RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        if self.store.lock().await.agent(&key)?.is_none() {
            return Err(RegistryError::NotFound(id));
        }
        // Abort, not graceful: a turn allowed to finish into a directory
        // about to be removed is work thrown away, and a delete must not
        // wait on an ask nobody will answer.
        self.stop_locked(id, StopMode::Abort).await?;

        let children: Vec<AgentRow> = self
            .store
            .lock()
            .await
            .agents()?
            .into_iter()
            .filter(|row| row.parent.as_ref() == Some(&key))
            .collect();
        for child in children {
            let wire = to_wire(&child);
            if child.agent.host != self.host.as_bytes() {
                response.unreachable_children.push(wire);
                continue;
            }
            let Ok(child_id) = Uuid::from_slice(&child.agent.agent) else {
                continue;
            };
            // Best effort: a child that cannot be deleted stays listed as an
            // orphan the person can delete.
            match Box::pin(self.delete_into(child_id, response)).await {
                Ok(()) => response.removed_children.push(wire),
                Err(error) => {
                    tracing::warn!(child = %child_id, %error, "a cascaded delete failed");
                    response.unreachable_children.push(wire);
                }
            }
        }

        // Rows first: a crash between the two leaves a directory with no
        // row, which the next sweep removes.
        {
            let mut store = self.store.lock().await;
            store.delete_agent(&key)?;
            self.fanout.close(&key);
            self.fanout
                .publish_inventory(inventory(inventory_event::Of::AgentRemoved(AgentRemoved {
                    host_id: key.host.clone(),
                    agent_id: key.agent.clone(),
                    reason: Some("deleted".to_owned()),
                })));
        }
        match std::fs::remove_dir_all(self.agent_dir(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.operations.lock().unwrap().remove(&id);
        Ok(())
    }

    /// Renames an agent. The name lives on the row only; specs keep the
    /// name the agent was spawned with.
    pub async fn rename(&self, id: AgentId, name: &str) -> Result<Agent, RegistryError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let mut row = store.agent(&key)?.ok_or(RegistryError::NotFound(id))?;
        row.name = Some(name.to_owned()).filter(|name| !name.is_empty());
        self.put_row(&mut store, &row)?;
        Ok(to_wire(&row))
    }

    // --- the sweep ---------------------------------------------------------

    /// The read-only half of the sweep: try the lock of every agent
    /// directory, dial the live ones and read their Hello. Writes nothing.
    pub async fn look(&self) -> Result<Looked, RegistryError> {
        let rows: HashMap<AgentId, AgentRow> = self
            .store
            .lock()
            .await
            .agents()?
            .into_iter()
            .filter(|row| row.agent.host == self.host.as_bytes())
            .filter_map(|row| Some((Uuid::from_slice(&row.agent.agent).ok()?, row)))
            .collect();
        let mut dirs = HashSet::new();
        let agents_dir = self.dir.join(AGENTS);
        match std::fs::read_dir(&agents_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| Uuid::parse_str(name).ok())
                        && entry.file_type()?.is_dir()
                    {
                        dirs.insert(id);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let mut agents = Vec::new();
        for &id in rows.keys() {
            if !dirs.contains(&id) {
                agents.push((id, Look::NoDirectory));
                continue;
            }
            let dir = self.agent_dir(id);
            let look = match look_once(&dir, LOOK_PATIENCE).await {
                Probe::Connected(connected) => Look::Live(connected),
                Probe::Unlocked => Look::Exited,
                Probe::Waiting => Look::Unanswered,
            };
            agents.push((id, look));
        }
        let orphans = dirs
            .into_iter()
            .filter(|id| !rows.contains_key(id))
            .collect();
        Ok(Looked { agents, orphans })
    }

    /// The writing half of the sweep, after activation: adopts the live
    /// agents and ingests from their stored cursors, marks the rest
    /// exited once their journal remainder is ingested and sets their
    /// CaughtUp flag, and removes directories no row lists.
    pub async fn finish_sweep(&self, looked: Looked) -> Result<SweepReport, RegistryError> {
        let mut report = SweepReport::default();
        for (id, look) in looked.agents {
            match look {
                Look::Live(connected) => {
                    let handle = AgentHandle::new(self.agent_dir(id), None);
                    self.agents.lock().unwrap().insert(id, handle.clone());
                    let reader = self.adopt(id, &handle, *connected).await;
                    self.watch(id, handle, Some(reader));
                    report.live.push(id);
                }
                Look::Unanswered => {
                    let handle = AgentHandle::new(self.agent_dir(id), None);
                    self.agents.lock().unwrap().insert(id, handle.clone());
                    self.watch(id, handle, None);
                    report.unanswered.push(id);
                }
                Look::Exited => {
                    self.ingest(id).await?;
                    self.mark_exited_if_live(id, CAUSE_EXITED_AWAY).await?;
                    report.exited.push(id);
                }
                Look::NoDirectory => {
                    self.mark_exited_if_live(id, CAUSE_NO_DIRECTORY).await?;
                    report.exited.push(id);
                }
            }
        }
        for id in looked.orphans {
            match std::fs::remove_dir_all(self.agent_dir(id)) {
                Ok(()) => report.removed.push(id),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for list in [
            &mut report.live,
            &mut report.unanswered,
            &mut report.exited,
            &mut report.removed,
        ] {
            list.sort();
        }
        Ok(report)
    }

    /// Stops every watcher and closes every control connection, so nothing
    /// writes the store any more. The agents keep running.
    pub async fn stop_watching(&self) {
        let handles: Vec<_> = self
            .agents
            .lock()
            .unwrap()
            .drain()
            .map(|(_, h)| h)
            .collect();
        for handle in handles {
            let tasks: Vec<_> = handle.tasks.lock().unwrap().drain(..).collect();
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            handle.ctl.lock().await.take();
        }
    }

    // --- ingest --------------------------------------------------------------

    /// Reads the agent's journal from its row's cursor to the end, commits
    /// what it finds in one transaction, then broadcasts each record once.
    /// At the end of the journal, the first time after each Hello, it
    /// broadcasts CaughtUp. Segments that lie wholly below a cursor that
    /// has reached the drive are deleted, but for the newest few.
    pub async fn ingest(&self, id: AgentId) -> Result<Committed, RegistryError> {
        let key = self.key(id);
        let journal_dir = self.agent_dir(id).join(agent_dir::JOURNAL);
        // Held through the broadcast: commit order is broadcast order, and
        // a subscribe waiting on the lock reads a cut either wholly before
        // this batch or wholly after it.
        let mut store = self.store.lock().await;
        let cursor = store.cursor(&key)?;
        let mut reader = journal::Reader::new(&journal_dir, cursor);
        let batch = reader.read_to_end()?;
        // A torn frame in the newest segment is a write still under way:
        // the journal has not ended yet, and its Nudge will come.
        let at_end = !matches!(batch.torn, Some(journal::Torn { skipped: false, .. }));
        let committed = if batch.frames.is_empty() {
            Committed {
                cursor,
                ..Committed::default()
            }
        } else {
            let clock = CommitClock {
                now_ms: self.clock.now_ms(),
                notify_delay_ms: self.launch.lock().unwrap().notify_delay_ms,
            };
            let committed = store.commit(&key, &batch.frames, clock)?;
            // Only now, with the transaction committed: a subscriber never
            // sees a revision the store could not serve it.
            let mut envelope = false;
            for record in &committed.records {
                let of = match record {
                    Record::Item(item) => session_event::Of::Item(item.clone()),
                    Record::Append(append) => session_event::Of::Append(append.clone()),
                    Record::Snapshot(snapshot) => {
                        envelope = true;
                        session_event::Of::Snapshot(snapshot.clone())
                    }
                };
                self.fanout.publish(&key, event(of));
            }
            if envelope {
                // Phase, working_on and last activity ride the inventory row.
                self.publish_row(&store, &key)?;
            }
            self.reclaim(&store, &journal_dir, committed.cursor);
            committed
        };
        let due = self
            .agents
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|handle| handle.caught_up_due.load(Ordering::SeqCst));
        if at_end && due {
            self.announce_caught_up(&mut store, &key)?;
            if let Some(handle) = self.agents.lock().unwrap().get(&id) {
                handle.caught_up_due.store(false, Ordering::SeqCst);
            }
        }
        Ok(committed)
    }

    /// Deletes the segments that lie wholly below `cursor`, keeping the
    /// newest [`KEPT_SEGMENTS`] of them. The cursor was committed without a
    /// sync, and a power cut could take the store back to before it while
    /// the deleted frames were its only copy, so the store is flushed to the
    /// drive first and nothing is deleted if the flush fails. That costs one
    /// flush per segment the agent fills.
    fn reclaim(&self, store: &Sqlite, journal_dir: &Path, cursor: u64) {
        let starts = match journal::reclaimable(journal_dir, cursor, KEPT_SEGMENTS) {
            Ok(starts) if !starts.is_empty() => starts,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%error, "listing journal segments failed");
                return;
            }
        };
        if let Err(error) = store.flush_to_drive() {
            tracing::warn!(%error, "flushing the store before reclaiming segments failed");
            return;
        }
        for start in starts {
            if let Err(error) = std::fs::remove_file(journal::segment_path(journal_dir, start)) {
                tracing::warn!(%error, start, "deleting an ingested journal segment failed");
            }
        }
    }

    // --- the watcher ---------------------------------------------------------

    /// Takes a connection as the agent's current one: records its Hello,
    /// keeps its write half for stops and inputs, and ingests what the
    /// journal holds beyond the stored cursor. Returns the read half.
    async fn adopt(
        &self,
        id: AgentId,
        handle: &AgentHandle,
        connected: Connected,
    ) -> ReadHalf<LocalStream> {
        let Connected {
            reader,
            writer,
            hello,
        } = connected;
        if let Err(error) = self.hello_received(id, &hello).await {
            tracing::warn!(agent = %id, %error, "recording an agent's Hello failed");
        }
        *handle.ctl.lock().await = Some(writer);
        handle.hello.send_replace(Some(hello));
        handle.caught_up_due.store(true, Ordering::SeqCst);
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingest failed");
        }
        reader
    }

    /// Runs the agent's watcher: from `reader` if a connection is already
    /// adopted, else from the first successful dial.
    fn watch(&self, id: AgentId, handle: Arc<AgentHandle>, reader: Option<ReadHalf<LocalStream>>) {
        let runtime = self.me.clone();
        let watched = handle.clone();
        let task = tokio::spawn(async move {
            let mut adopted = reader;
            loop {
                let mut reader = match adopted.take() {
                    Some(reader) => reader,
                    None => {
                        let Some(connected) = connect(&watched.dir, &watched.running).await else {
                            break;
                        };
                        let Some(me) = runtime.upgrade() else { return };
                        me.adopt(id, &watched, connected).await
                    }
                };
                loop {
                    match agent_dir::read_frame(&mut reader).await {
                        Ok(Some(CtlFrame {
                            of: Some(ctl_frame::Of::Nudge(_)),
                        })) => {
                            let Some(me) = runtime.upgrade() else { return };
                            if let Err(error) = me.ingest(id).await {
                                tracing::warn!(agent = %id, %error, "ingest failed");
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
                if let Some(mut ctl) = watched.ctl.lock().await.take() {
                    let _ = ctl.shutdown().await;
                }
            }
            if let Some(me) = runtime.upgrade() {
                me.exited(id, &watched).await;
            }
        });
        handle.tasks.lock().unwrap().push(task);
    }

    async fn hello_received(&self, id: AgentId, hello: &AgentHello) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        if let Some(mut row) = store.agent(&key)?
            && row.producer_version != hello.agent_version
        {
            row.producer_version = hello.agent_version.clone();
            self.put_row(&mut store, &row)?;
        }
        Ok(())
    }

    /// The process is gone: ingest what it left, record the exit, close its
    /// tools socket and let whoever waits on it go.
    async fn exited(&self, id: AgentId, handle: &Arc<AgentHandle>) {
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingesting an exited agent's journal failed");
        }
        let cause = match *handle.stopping.lock().unwrap() {
            Some(StopMode::Graceful) => CAUSE_STOPPED,
            Some(StopMode::Abort) => CAUSE_ABORTED,
            Some(StopMode::Kill) => CAUSE_KILLED,
            None => CAUSE_EXITED,
        };
        if let Err(error) = self.mark_exited(id, cause).await {
            tracing::warn!(agent = %id, %error, "marking an agent exited failed");
        }
        {
            let mut agents = self.agents.lock().unwrap();
            if agents
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, handle))
            {
                agents.remove(&id);
            }
        }
        // Every task but this watcher: the tools listener's socket goes with
        // it, and the reaper has nothing left to wait for.
        let tasks: Vec<_> = handle.tasks.lock().unwrap().drain(..).collect();
        let current = tokio::task::try_id();
        for task in tasks {
            if Some(task.id()) != current {
                task.abort();
                let _ = task.await;
            }
        }
        let _ = std::fs::remove_file(handle.dir.join(agent_dir::TOOLS_SOCK));
        handle.exited.send_replace(true);
    }

    async fn mark_exited(&self, id: AgentId, cause: &str) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(mut row) = store.agent(&key)? else {
            return Ok(());
        };
        row.lifecycle = Lifecycle::Exited as i32;
        row.exit_cause = Some(cause.to_owned());
        self.put_row(&mut store, &row)?;
        // Everything the process wrote is committed: nothing more comes.
        self.caught_up(&mut store, &key)
    }

    async fn mark_exited_if_live(&self, id: AgentId, cause: &str) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(mut row) = store.agent(&key)? else {
            return Ok(());
        };
        if row.lifecycle != Lifecycle::Exited as i32 {
            row.lifecycle = Lifecycle::Exited as i32;
            row.exit_cause = Some(cause.to_owned());
            self.put_row(&mut store, &row)?;
        }
        self.caught_up(&mut store, &key)
    }

    /// Writes a row's registry fields and tells inventory subscribers,
    /// with the store still held so they see changes in store order.
    fn put_row(&self, store: &mut Sqlite, row: &AgentRow) -> Result<(), StoreError> {
        store.put_agent(row)?;
        self.publish_row(store, &row.agent)
    }

    /// Tells inventory subscribers what the row now says.
    fn publish_row(&self, store: &Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if let Some(row) = store.agent(key)? {
            self.fanout
                .publish_inventory(inventory(inventory_event::Of::Agent(to_wire(&row))));
        }
        Ok(())
    }

    /// Sets the agent's CaughtUp flag and broadcasts the marker, unless the
    /// flag is already set: a subscriber that joined since read it at its
    /// cut, and one that joined before got the broadcast then.
    fn caught_up(&self, store: &mut Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if store.markers().get(key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        self.announce_caught_up(store, key)
    }

    fn announce_caught_up(&self, store: &mut Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        let Some(row) = store.agent(key)? else {
            return Ok(());
        };
        store.set_marker(key, Some(Marker::CaughtUp));
        self.fanout.publish(
            key,
            event(session_event::Of::CaughtUp(CaughtUp {
                revision: row.next_revision.saturating_sub(1),
            })),
        );
        Ok(())
    }

    // --- waiting on processes ----------------------------------------------

    async fn wait_exit(&self, handle: &AgentHandle, deadline_ms: i64) -> Result<(), ()> {
        let mut exited = handle.exited.subscribe();
        let deadline = self.clock.sleep_until(self.clock.now_ms() + deadline_ms);
        tokio::select! {
            _ = exited.wait_for(|exited| *exited) => Ok(()),
            () = deadline => Err(()),
        }
    }

    async fn wait_unlocked(&self, dir: &Path, deadline_ms: i64) -> Result<(), ()> {
        let deadline = self.clock.sleep_until(self.clock.now_ms() + deadline_ms);
        tokio::pin!(deadline);
        loop {
            if !locked(dir) {
                return Ok(());
            }
            tokio::select! {
                () = tokio::time::sleep(PROBE_INTERVAL) => {}
                () = &mut deadline => return Err(()),
            }
        }
    }

    /// Kills the agent's process group, when this daemon started it and so
    /// knows it. Returns whether it could.
    fn kill(&self, handle: &AgentHandle) -> bool {
        let Some(pid) = handle.pid else { return false };
        kill_group(pid)
    }
}

pub use agent_dir::locked;

enum Probe {
    Connected(Box<Connected>),
    Unlocked,
    Waiting,
}

/// Looks at a directory until its process answers or its lock is free, or
/// `patience` runs out.
async fn look_once(dir: &Path, patience: Duration) -> Probe {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        if !locked(dir) {
            return Probe::Unlocked;
        }
        if let Some(connected) = dial(dir).await {
            return Probe::Connected(Box::new(connected));
        }
        if tokio::time::Instant::now() >= deadline {
            return Probe::Waiting;
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Dials the directory's process until it answers with a Hello, or returns
/// None once its lock is free: the process is gone.
async fn connect(dir: &Path, running: &watch::Sender<bool>) -> Option<Connected> {
    let mut wait = PROBE_INTERVAL;
    let mut exits = running.subscribe();
    loop {
        if !*exits.borrow_and_update() && !locked(dir) {
            return None;
        }
        if let Some(connected) = dial(dir).await {
            return Some(connected);
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            // The process ended: look again at once.
            _ = exits.changed() => {}
        }
        wait = (wait * 2).min(Duration::from_secs(1));
    }
}

async fn dial(dir: &Path) -> Option<Connected> {
    let stream = local_socket::connect(&dir.join(agent_dir::CTL_SOCK))
        .await
        .ok()?;
    let (mut reader, writer) = tokio::io::split(stream);
    let hello = tokio::time::timeout(HELLO_PATIENCE, agent_dir::read_frame(&mut reader))
        .await
        .ok()?
        .ok()??;
    match hello.of {
        Some(ctl_frame::Of::Hello(hello)) => Some(Connected {
            reader,
            writer,
            hello,
        }),
        _ => None,
    }
}

/// Starts `amux agent <dir>` detached from the daemon: its own process
/// group, so the daemon's terminal and death do not reach it, and no pipe
/// to the daemon, which would break when the daemon goes.
fn spawn_agent(install_path: &Path, dir: &Path) -> io::Result<tokio::process::Child> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(AGENT_LOG))?;
    let mut command = tokio::process::Command::new(install_path);
    command
        .arg("agent")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    command.spawn()
}

/// What the agent process writes to stderr, beside its directory's other
/// files: it has no terminal and no daemon pipe to write to.
pub const AGENT_LOG: &str = "agent.log";

/// Waits on a child this daemon started, so it never lingers as a zombie.
async fn reap(mut child: tokio::process::Child) {
    let _ = child.wait().await;
}

#[cfg(unix)]
fn kill_group(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: a plain signal to the process group the agent leads.
    unsafe { libc::killpg(pid, libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
fn kill_group(pid: u32) -> bool {
    std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The inventory row for an agents row.
pub fn to_wire(row: &AgentRow) -> Agent {
    Agent {
        agent_id: row.agent.agent.clone(),
        host_id: row.agent.host.clone(),
        kind: spec::kind_from_name(&row.kind) as i32,
        name: row.name.clone(),
        cwd: row.cwd.clone(),
        parent: row.parent.as_ref().map(|parent| AgentParent {
            host_id: parent.host.clone(),
            agent_id: parent.agent.clone(),
        }),
        created_at_ms: row.created_at,
        lifecycle: row.lifecycle,
        exit_cause: row.exit_cause.clone(),
        phase: row.phase,
        working_on: row.working_on.as_ref().map(|text| WorkingOn {
            text: text.clone(),
            updated_at_ms: row.last_activity.unwrap_or_default(),
        }),
        last_activity_ms: row.last_activity.unwrap_or_default(),
        producer_version: row.producer_version.clone(),
        incarnation: row.incarnation,
    }
}
