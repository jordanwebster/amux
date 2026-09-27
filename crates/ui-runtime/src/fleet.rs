//! The fleet driver: the inventory stream into [`FleetState`], with the
//! same reconnect and trace as a session, one level up. No per-agent
//! subscriptions: agent rows carry their phase.

use std::collections::BTreeSet;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use client::{Client, Clock, EventStream, RpcError};
use futures_util::StreamExt as _;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use ui_state::{AgentKey, Connection, FleetMsg, FleetState};
use wire::{DumpFile, DumpPart, InventoryEvent, inventory_event};

use crate::Backoff;
use crate::trace::{DriverEvent, DriverTrace, Ring, Structure as _, TraceEvent};

struct Model {
    state: FleetState,
    trace: Ring<FleetState, FleetMsg>,
    changed: BTreeSet<AgentKey>,
    ended: Option<RpcError>,
}

struct Inner {
    client: Arc<dyn Client>,
    clock: Arc<dyn Clock>,
    model: Mutex<Model>,
    changed: watch::Sender<()>,
    closed: AtomicBool,
}

impl Inner {
    fn model(&self) -> MutexGuard<'_, Model> {
        self.model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn apply(&self, msg: FleetMsg) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model { state, trace, .. } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Msg(msg.clone()));
        let connection = matches!(msg, FleetMsg::Connection(_));
        let keys = state.update(msg);
        let moved = connection || !keys.is_empty();
        model.changed.extend(keys);
        drop(model);
        if moved {
            self.changed.send_replace(());
        }
    }

    fn note(&self, event: DriverEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model { state, trace, .. } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Driver(event));
    }

    fn event(&self, event: InventoryEvent) {
        let caught_up = matches!(event.of, Some(inventory_event::Of::CaughtUp(_)));
        self.apply(FleetMsg::Event(Box::new(event)));
        if caught_up {
            // A caught-up fleet is news even when no row moved.
            self.changed.send_replace(());
        }
    }
}

/// The fleet's state while the guard lives; never hold it across an await.
pub struct FleetGuard<'a>(MutexGuard<'a, Model>);

impl Deref for FleetGuard<'_> {
    type Target = FleetState;

    fn deref(&self) -> &FleetState {
        &self.0.state
    }
}

pub struct Fleet {
    inner: Arc<Inner>,
    pump: JoinHandle<()>,
}

impl Fleet {
    /// Subscribes to the inventory and resolves once it has caught up, or
    /// once its stream ended first, in which case the pump reconnects.
    pub async fn open(client: Arc<dyn Client>, clock: impl Clock) -> Result<Fleet, RpcError> {
        let state = FleetState::new();
        let (changed, _) = watch::channel(());
        let inner = Arc::new(Inner {
            client,
            clock: Arc::new(clock),
            model: Mutex::new(Model {
                trace: Ring::new(state.clone()),
                state,
                changed: BTreeSet::new(),
                ended: None,
            }),
            changed,
            closed: AtomicBool::new(false),
        });
        let mut stream = inner.client.subscribe_inventory().await?;
        inner.note(DriverEvent::Subscribed { tail: 0 });
        inner.apply(FleetMsg::Connection(Connection::Live));
        let mut ended = None;
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let caught_up = matches!(event.of, Some(inventory_event::Of::CaughtUp(_)));
                    inner.event(event);
                    if caught_up {
                        break;
                    }
                }
                Some(Err(error)) => {
                    ended = Some(Some(error));
                    break;
                }
                None => {
                    ended = Some(None);
                    break;
                }
            }
        }
        let pump = tokio::spawn(pump(inner.clone(), stream, ended));
        Ok(Fleet { inner, pump })
    }

    pub fn state(&self) -> FleetGuard<'_> {
        FleetGuard(self.inner.model())
    }

    /// Level-triggered, as a session's.
    pub fn changed(&self) -> watch::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// The agents whose card or family attention may have changed since the
    /// last call.
    pub fn take_changed(&self) -> Vec<AgentKey> {
        std::mem::take(&mut self.inner.model().changed)
            .into_iter()
            .collect()
    }

    /// Set when the runtime refused to serve the inventory again.
    pub fn ended(&self) -> Option<RpcError> {
        self.inner.model().ended.clone()
    }

    pub fn trace(&self) -> DriverTrace<FleetState, FleetMsg> {
        self.inner.model().trace.trace()
    }

    /// The fleet's part of a dump bundle: the structure of its state and
    /// its trace, with no names, paths or status text.
    pub fn dump_part(&self) -> DumpPart {
        let (state, trace) = {
            let model = self.inner.model();
            (model.state.structure(), model.trace.trace())
        };
        DumpPart {
            dump_id: Vec::new(),
            files: vec![
                DumpFile {
                    name: "client/fleet/state.txt".into(),
                    contents: state.into_bytes(),
                },
                DumpFile {
                    name: "client/fleet/trace.txt".into(),
                    contents: trace.render().into_bytes(),
                },
            ],
        }
    }

    pub fn close(self) {}
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        self.pump.abort();
    }
}

impl DriverTrace<FleetState, FleetMsg> {
    pub fn replay(&self) -> FleetState {
        let mut state = self.start.clone();
        for msg in self.msgs() {
            state.update(msg.clone());
        }
        state
    }
}

/// Reads the inventory until it ends, then reconnects with backoff; the
/// state re-lists and drops what the new stream no longer names at its
/// CaughtUp.
async fn pump(
    inner: Arc<Inner>,
    stream: EventStream<InventoryEvent>,
    ended: Option<Option<RpcError>>,
) {
    let mut backoff = Backoff::default();
    let mut stream = Some(stream);
    let mut ended = ended;
    loop {
        let error = match ended.take() {
            Some(error) => error,
            None => {
                let Some(open) = stream.as_mut() else { return };
                loop {
                    match open.next().await {
                        Some(Ok(event)) => {
                            if matches!(event.of, Some(inventory_event::Of::CaughtUp(_))) {
                                backoff.reset();
                            }
                            inner.event(event);
                        }
                        Some(Err(error)) => break Some(error),
                        None => break None,
                    }
                }
            }
        };
        stream = None;
        inner.note(DriverEvent::StreamEnded {
            error: error.map(|error| error.to_string()),
        });
        inner.apply(FleetMsg::Connection(Connection::Reconnecting));
        while stream.is_none() {
            let until_ms = inner.clock.now_ms() + backoff.next_ms();
            inner.note(DriverEvent::Backoff { until_ms });
            inner.clock.sleep_until(until_ms).await;
            match inner.client.subscribe_inventory().await {
                Ok(opened) => {
                    inner.note(DriverEvent::Subscribed { tail: 0 });
                    inner.apply(FleetMsg::Connection(Connection::Live));
                    stream = Some(opened);
                }
                Err(RpcError::Transport(error)) => {
                    inner.note(DriverEvent::SubscribeFailed { error });
                }
                Err(error) => {
                    inner.note(DriverEvent::Ended {
                        error: error.to_string(),
                    });
                    inner.model().ended = Some(error);
                    inner.changed.send_replace(());
                    return;
                }
            }
        }
    }
}
