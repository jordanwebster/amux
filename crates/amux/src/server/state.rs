use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

use crate::agent::AgentSession;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::link::Link;
use crate::protocol::message::{Host, Message, SubscriptionId};
use crate::protocol::route::Route;
use crate::server::registry::AgentRegistry;

pub(in crate::server) const LOCAL_USER_ID: Uuid = Uuid::nil();
pub(in crate::server) const SUBSCRIPTION_LEASE_DURATION: Duration = Duration::from_secs(300);

/// Request from a connection handler to shut down or suspend the server.
pub(in crate::server) enum ShutdownRequest {
    Shutdown {
        reply: mpsc::Sender<Message>,
        link: Link,
    },
    Suspend {
        reply: mpsc::Sender<Message>,
        link: Link,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::server) enum SubscriptionMode {
    Raw,
    Structured,
}

impl SubscriptionMode {
    pub(in crate::server) fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Structured => "structured",
        }
    }
}

#[derive(Clone)]
pub(in crate::server) struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    next_request_id: Arc<AtomicU64>,
}

impl ConnectionHandle {
    pub(in crate::server) fn new(
        tx: mpsc::Sender<Message>,
        next_request_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            tx,
            next_request_id,
        }
    }

    pub(in crate::server) fn sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    pub(in crate::server) fn request_counter(&self) -> Arc<AtomicU64> {
        self.next_request_id.clone()
    }

    pub(in crate::server) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(in crate::server) async fn send(
        &self,
        msg: Message,
    ) -> std::result::Result<(), mpsc::error::SendError<Message>> {
        self.tx.send(msg).await
    }

    pub(in crate::server) fn try_send(&self, msg: Message) -> bool {
        self.tx.try_send(msg).is_ok()
    }
}

/// An active subscription that can be cancelled.
pub(in crate::server) struct SubscriptionEntry {
    pub subscription_id: SubscriptionId,
    pub agent_id: Uuid,
    pub mode: SubscriptionMode,
    /// Dropping this sender signals the stream task's paired `oneshot::Receiver`
    /// to resolve with `Err(Canceled)`, cancelling the stream.
    #[allow(dead_code)]
    pub cancel: oneshot::Sender<()>,
    pub dst: Route,
    pub lease_deadline: Instant,
}

pub(in crate::server) struct ServerUserState {
    pub(in crate::server) agents: HashMap<Uuid, AgentSession>,
    pub(in crate::server) routes: HashMap<Link, ConnectionHandle>,
    pub(in crate::server) registry: AgentRegistry,
    pub(in crate::server) peer_links: HashSet<Link>,
    pub(in crate::server) hosts: HashMap<Uuid, Host>,
    pub(in crate::server) active_subscriptions: HashMap<SubscriptionId, SubscriptionEntry>,
}

impl ServerUserState {
    pub(in crate::server) fn new() -> Self {
        Self {
            agents: HashMap::new(),
            routes: HashMap::new(),
            registry: AgentRegistry::new(),
            peer_links: HashSet::new(),
            hosts: HashMap::new(),
            active_subscriptions: HashMap::new(),
        }
    }

    /// Atomically register a connection under `link`, returning the
    /// handle and the receive half of its outgoing channel.
    ///
    /// Fails with `Err(link)` when the link is already in use — the
    /// original name is returned so the caller can reply to the peer without
    /// reconstructing it.
    pub(in crate::server) fn try_reserve_link(
        &mut self,
        link: Link,
    ) -> Result<(ConnectionHandle, mpsc::Receiver<Message>), Link> {
        use std::sync::atomic::AtomicU64;

        if self.routes.contains_key(&link) {
            return Err(link);
        }
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
        let next_request_id = Arc::new(AtomicU64::new(1));
        let handle = ConnectionHandle::new(outgoing_tx, next_request_id);
        self.routes.insert(link, handle.clone());
        Ok((handle, outgoing_rx))
    }
}

pub(in crate::server) fn subscription_lease_ms() -> u64 {
    SUBSCRIPTION_LEASE_DURATION
        .as_millis()
        .try_into()
        .expect("subscription lease should fit in u64 milliseconds")
}

pub(in crate::server) struct ServerState {
    pub(in crate::server) config: Config,
    pub(in crate::server) host_id: Uuid,
    pub(in crate::server) is_cloud_server: bool,
    pub(in crate::server) jwt_validator: Option<Arc<JwtValidator>>,
    pub(in crate::server) users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    pub(in crate::server) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

impl ServerState {
    pub(in crate::server) fn new(
        config: Config,
        host_id: Uuid,
        shutdown_tx: mpsc::Sender<ShutdownRequest>,
    ) -> Self {
        let mut users = HashMap::new();
        users.insert(LOCAL_USER_ID, Arc::new(RwLock::new(ServerUserState::new())));
        Self {
            config,
            host_id,
            is_cloud_server: false,
            jwt_validator: None,
            users,
            shutdown_tx,
        }
    }

    pub(in crate::server) fn user_state(
        &self,
        user_id: &Uuid,
    ) -> Option<Arc<RwLock<ServerUserState>>> {
        self.users.get(user_id).cloned()
    }
}

pub(in crate::server) async fn ensure_user_state(
    state: &Arc<RwLock<ServerState>>,
    user_id: Uuid,
) -> Arc<RwLock<ServerUserState>> {
    {
        let s = state.read().await;
        if let Some(us) = s.users.get(&user_id) {
            return us.clone();
        }
    }

    let mut s = state.write().await;
    s.users
        .entry(user_id)
        .or_insert_with(|| Arc::new(RwLock::new(ServerUserState::new())))
        .clone()
}
