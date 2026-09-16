//! Profile-local execution edge for the shared SQLite client store.
//!
//! The reducer owns operation identity and all lifecycle decisions. This
//! worker only opens the store, executes operations in order, and returns the
//! corresponding recorded message with the original freshness envelope.

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use store::{CommitOutcome, Store};
use ui_state::{
    HeadDto, LoadedDto, Msg, MutationBatchDto, PageDto, ProfileGeneration, StoreMsg, StoreOp,
    StoreOpKind,
};

use crate::runtime::MsgSink;

const DATA_VERSION_POLL: Duration = Duration::from_secs(1);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const MAINTENANCE_DEADLINE: Duration = Duration::from_millis(100);

pub(crate) struct StoreWorker {
    handle: StoreWorkerHandle,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    maintenance_runs: Arc<AtomicUsize>,
    #[cfg(test)]
    data_version_polls: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub(crate) struct StoreWorkerHandle {
    sender: Sender<Command>,
    retired: Arc<AtomicBool>,
}

enum Command {
    Execute(Box<StoreOp>),
    RecordChatOpened(model::AgentId),
    AfterFirstFrame,
    MaintenanceFinished(Result<store::MaintenanceReport, store::StoreError>),
    Shutdown,
}

impl StoreWorker {
    pub(crate) fn spawn(path: PathBuf, profile: ProfileGeneration, sink: MsgSink) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker_sender = sender.clone();
        let retired = Arc::new(AtomicBool::new(false));
        let worker_retired = Arc::clone(&retired);
        #[cfg(test)]
        let maintenance_runs = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let worker_maintenance_runs = Arc::clone(&maintenance_runs);
        #[cfg(test)]
        let data_version_polls = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let worker_data_version_polls = Arc::clone(&data_version_polls);
        let thread = std::thread::Builder::new()
            .name("amux-ui-store".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("store executor runtime");
                let store = match runtime.block_on(Store::open(&path)) {
                    Ok(store) => store,
                    Err(error) => {
                        // Establish the reducer's profile generation even when
                        // SQLite cannot open, then enter its explicit live-only
                        // state before the network is allowed to start.
                        let _ = sink.blocking_send(Msg::StoreStartup {
                            profile,
                            generations: store::Generations {
                                fleet: 0,
                                chat: 0,
                                provider: 0,
                            },
                        });
                        let _ = sink
                            .blocking_send(Msg::Store(StoreMsg::Unavailable { profile, error }));
                        while !worker_retired.load(Ordering::Acquire) {
                            let Ok(command) = receiver.recv() else {
                                break;
                            };
                            match command {
                                Command::Execute(_) | Command::RecordChatOpened(_) => {
                                    let _ = sink.blocking_send(Msg::Store(StoreMsg::Unavailable {
                                        profile,
                                        error,
                                    }));
                                }
                                Command::AfterFirstFrame | Command::MaintenanceFinished(_) => {}
                                Command::Shutdown => break,
                            }
                        }
                        return;
                    }
                };

                let store = Arc::new(store);
                let generations = store
                    .generations()
                    .for_provider("codex")
                    .expect("codex is a registered provider");
                let _ = sink.blocking_send(Msg::StoreStartup {
                    profile,
                    generations,
                });
                let mut data_version = runtime.block_on(store.data_version()).ok();
                let mut maintenance_enabled = false;
                let mut last_maintenance = None;
                let mut maintenance_thread: Option<JoinHandle<()>> = None;
                let mut next_poll = Instant::now() + DATA_VERSION_POLL;

                while !worker_retired.load(Ordering::Acquire) {
                    let wait = next_poll.saturating_duration_since(Instant::now());
                    match receiver.recv_timeout(wait) {
                        Ok(Command::Execute(op)) => {
                            let message = runtime.block_on(execute(&store, *op));
                            let _ = sink.blocking_send(Msg::Store(message));
                        }
                        Ok(Command::RecordChatOpened(agent)) => {
                            if let Err(error) = runtime.block_on(store.record_chat_opened(agent))
                                && error == store::StoreError::Corrupt
                            {
                                let _ = sink.blocking_send(Msg::Store(StoreMsg::Unavailable {
                                    profile,
                                    error,
                                }));
                            }
                        }
                        Ok(Command::AfterFirstFrame) => maintenance_enabled = true,
                        Ok(Command::MaintenanceFinished(result)) => {
                            if let Some(thread) = maintenance_thread.take() {
                                let _ = thread.join();
                            }
                            #[cfg(test)]
                            worker_maintenance_runs.fetch_add(1, Ordering::Release);
                            if result == Err(store::StoreError::Corrupt) {
                                let _ = sink.blocking_send(Msg::Store(StoreMsg::Unavailable {
                                    profile,
                                    error: store::StoreError::Corrupt,
                                }));
                            }
                        }
                        Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    if worker_retired.load(Ordering::Acquire) {
                        break;
                    }
                    let now = Instant::now();
                    if now < next_poll {
                        continue;
                    }
                    while next_poll <= now {
                        next_poll += DATA_VERSION_POLL;
                    }
                    #[cfg(test)]
                    worker_data_version_polls.fetch_add(1, Ordering::Release);
                    if let Ok(current) = runtime.block_on(store.data_version()) {
                        if data_version.is_some_and(|previous| previous != current) {
                            let _ =
                                sink.blocking_send(Msg::Store(StoreMsg::FleetChanged { profile }));
                        }
                        data_version = Some(current);
                    }
                    let maintenance_due = maintenance_enabled
                        && maintenance_thread.is_none()
                        && last_maintenance
                            .is_none_or(|last: Instant| last.elapsed() >= MAINTENANCE_INTERVAL);
                    if maintenance_due {
                        let maintenance_store = Arc::clone(&store);
                        let completion = worker_sender.clone();
                        let spawned = std::thread::Builder::new()
                            .name("amux-ui-store-maintenance".to_owned())
                            .spawn(move || {
                                let runtime = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .expect("store maintenance runtime");
                                let result = runtime.block_on(
                                    maintenance_store
                                        .maintain(store::Budget::default(), MAINTENANCE_DEADLINE),
                                );
                                let _ = completion.send(Command::MaintenanceFinished(result));
                            });
                        if let Ok(thread) = spawned {
                            last_maintenance = Some(Instant::now());
                            maintenance_thread = Some(thread);
                        }
                    }
                }
                if let Some(thread) = maintenance_thread {
                    let _ = thread.join();
                }
                if let Ok(store) = Arc::try_unwrap(store) {
                    runtime.block_on(store.close());
                }
            })
            .expect("spawn profile store executor");
        Self {
            handle: StoreWorkerHandle { sender, retired },
            thread: Some(thread),
            #[cfg(test)]
            maintenance_runs,
            #[cfg(test)]
            data_version_polls,
        }
    }

    pub(crate) fn execute(&self, op: StoreOp) {
        self.handle.execute(op);
    }

    pub(crate) fn handle(&self) -> StoreWorkerHandle {
        self.handle.clone()
    }

    pub(crate) fn after_first_frame(&self) {
        let _ = self.handle.sender.send(Command::AfterFirstFrame);
    }

    pub(crate) fn record_chat_opened(&self, agent: model::AgentId) {
        let _ = self.handle.sender.send(Command::RecordChatOpened(agent));
    }

    #[cfg(test)]
    pub(crate) fn maintenance_runs(&self) -> usize {
        self.maintenance_runs.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn data_version_poll_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.data_version_polls)
    }
}

impl StoreWorkerHandle {
    pub(crate) fn execute(&self, op: StoreOp) {
        if !self.retired.load(Ordering::Acquire) {
            let _ = self.sender.send(Command::Execute(Box::new(op)));
        }
    }
}

impl Drop for StoreWorker {
    fn drop(&mut self) {
        self.handle.retired.store(true, Ordering::Release);
        let _ = self.handle.sender.send(Command::Shutdown);
        // A store result may be blocked behind the bounded UI channel. The
        // retired worker owns no UI state, so joining it here would turn a
        // profile switch into an unbounded synchronous wait. Dropping a
        // JoinHandle detaches the cooperative shutdown instead.
        self.thread.take();
    }
}

async fn execute(store: &Store, op: StoreOp) -> StoreMsg {
    match op {
        StoreOp::Load {
            profile,
            attempt,
            op,
            agent,
            protocol,
            window,
        } => {
            macro_rules! load {
                ($fold:ty, $variant:ident) => {
                    match store.load::<$fold>(agent, window).await {
                        Ok(loaded) => StoreMsg::Loaded {
                            profile,
                            attempt,
                            op,
                            agent,
                            loaded: Box::new(LoadedDto::$variant(loaded)),
                        },
                        Err(error) => {
                            failed(profile, attempt, op, Some(agent), StoreOpKind::Load, error)
                        }
                    }
                };
            }
            match protocol {
                model::StructuredProtocol::ClaudePtyTranscript => {
                    load!(store::claude_pty::ClaudeFold, Claude)
                }
                model::StructuredProtocol::ClaudeSdk => {
                    load!(store::claude_sdk::ClaudeSdkFold, ClaudeSdk)
                }
                model::StructuredProtocol::Codex => load!(store::codex::CodexFold, Codex),
            }
        }
        StoreOp::Commit {
            profile,
            attempt,
            op,
            agent,
            generations,
            expected,
            head,
            transition,
            mutations,
            interest,
        } => {
            macro_rules! commit {
                ($head:expr, $mutations:expr, $variant:ident) => {
                    commit_message(
                        store
                            .commit(
                                agent,
                                generations,
                                expected,
                                $head,
                                transition,
                                $mutations,
                                interest,
                            )
                            .await,
                        profile,
                        attempt,
                        op,
                        agent,
                        StoreOpKind::Commit,
                        LoadedDto::$variant,
                    )
                };
            }
            match (*head, mutations) {
                (HeadDto::Claude(head), MutationBatchDto::Claude(mutations)) => {
                    commit!(head, mutations, Claude)
                }
                (HeadDto::ClaudeSdk(head), MutationBatchDto::ClaudeSdk(mutations)) => {
                    commit!(head, mutations, ClaudeSdk)
                }
                (HeadDto::Codex(head), MutationBatchDto::Codex(mutations)) => {
                    commit!(head, mutations, Codex)
                }
                _ => failed(
                    profile,
                    attempt,
                    op,
                    Some(agent),
                    StoreOpKind::Commit,
                    store::StoreError::Corrupt,
                ),
            }
        }
        StoreOp::Page {
            profile,
            attempt,
            op,
            agent,
            protocol,
            token,
            n,
        } => {
            macro_rules! page {
                ($fold:ty, $variant:ident) => {
                    match store.page::<$fold>(agent, token, n).await {
                        Ok(page) => StoreMsg::Paged {
                            profile,
                            attempt,
                            op,
                            agent,
                            page: PageDto::$variant(page),
                        },
                        Err(error) => {
                            failed(profile, attempt, op, Some(agent), StoreOpKind::Page, error)
                        }
                    }
                };
            }
            match protocol {
                model::StructuredProtocol::ClaudePtyTranscript => {
                    page!(store::claude_pty::ClaudeFold, Claude)
                }
                model::StructuredProtocol::ClaudeSdk => {
                    page!(store::claude_sdk::ClaudeSdkFold, ClaudeSdk)
                }
                model::StructuredProtocol::Codex => page!(store::codex::CodexFold, Codex),
            }
        }
        StoreOp::Invalidate {
            profile,
            attempt,
            op,
            agent,
            protocol,
            generations,
            expected,
            reason,
        } => {
            macro_rules! invalidate {
                ($fold:ty, $variant:ident) => {
                    commit_message(
                        store
                            .invalidate::<$fold>(agent, generations, expected, reason)
                            .await,
                        profile,
                        attempt,
                        op,
                        agent,
                        StoreOpKind::Invalidate,
                        LoadedDto::$variant,
                    )
                };
            }
            match protocol {
                model::StructuredProtocol::ClaudePtyTranscript => {
                    invalidate!(store::claude_pty::ClaudeFold, Claude)
                }
                model::StructuredProtocol::ClaudeSdk => {
                    invalidate!(store::claude_sdk::ClaudeSdkFold, ClaudeSdk)
                }
                model::StructuredProtocol::Codex => {
                    invalidate!(store::codex::CodexFold, Codex)
                }
            }
        }
        StoreOp::FleetLoad {
            profile,
            op,
            generations,
        } => match store.fleet(generations).await {
            Ok(fleet) => StoreMsg::FleetLoaded { profile, op, fleet },
            Err(error) => failed(
                profile,
                store::AttemptId(0),
                op,
                None,
                StoreOpKind::FleetLoad,
                error,
            ),
        },
        StoreOp::FleetApply {
            profile,
            op,
            generations,
            delta,
        } => match store.apply_fleet(generations, *delta).await {
            Ok(_) => StoreMsg::FleetApplied { profile, op },
            Err(error) => failed(
                profile,
                store::AttemptId(0),
                op,
                None,
                StoreOpKind::FleetApply,
                error,
            ),
        },
        StoreOp::ViewGet {
            profile,
            op,
            kind,
            key,
        } => match store.view_get(&kind, &key).await {
            Ok(value) => StoreMsg::ViewLoaded { profile, op, value },
            Err(error) => failed(
                profile,
                store::AttemptId(0),
                op,
                None,
                StoreOpKind::ViewGet,
                error,
            ),
        },
        StoreOp::ViewSet {
            profile,
            op,
            kind,
            key,
            value,
        } => match store.view_set(&kind, &key, &value).await {
            Ok(()) => StoreMsg::ViewSet { profile, op },
            Err(error) => failed(
                profile,
                store::AttemptId(0),
                op,
                None,
                StoreOpKind::ViewSet,
                error,
            ),
        },
    }
}

fn commit_message<F>(
    outcome: CommitOutcome<F>,
    profile: ProfileGeneration,
    attempt: store::AttemptId,
    op: store::OpId,
    agent: model::AgentId,
    kind: StoreOpKind,
    loaded: impl FnOnce(store::Loaded<F>) -> LoadedDto,
) -> StoreMsg
where
    F: store::ProviderFold,
{
    match outcome {
        CommitOutcome::Committed(result) => StoreMsg::Committed {
            profile,
            attempt,
            op,
            agent,
            result,
        },
        CommitOutcome::Conflict(value) => StoreMsg::Conflict {
            profile,
            attempt,
            op,
            agent,
            loaded: Box::new(loaded(value)),
        },
        CommitOutcome::Refused(error) => failed(profile, attempt, op, Some(agent), kind, error),
    }
}

fn failed(
    profile: ProfileGeneration,
    attempt: store::AttemptId,
    op: store::OpId,
    agent: Option<model::AgentId>,
    kind: StoreOpKind,
    error: store::StoreError,
) -> StoreMsg {
    if error == store::StoreError::Corrupt {
        return StoreMsg::Unavailable { profile, error };
    }
    StoreMsg::Failed {
        profile,
        attempt,
        op,
        agent,
        kind,
        error,
    }
}
