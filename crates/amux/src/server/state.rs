use crate::agent::AgentSession;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::message::{Host, Message, SubscriptionId};
use crate::protocol::route::Route;
use crate::server::registry::AgentRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

pub(crate) const LOCAL_USER_ID: Uuid = Uuid::nil();
pub(crate) const SUBSCRIPTION_LEASE_DURATION: Duration = Duration::from_secs(300);

/// Request from a connection handler to shut down or suspend the server.
pub(crate) enum ShutdownRequest {
    Shutdown {
        reply: mpsc::Sender<Message>,
        link_name: String,
    },
    Suspend {
        reply: mpsc::Sender<Message>,
        link_name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscriptionMode {
    Raw,
    Structured,
}

impl SubscriptionMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Structured => "structured",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionHandle {
    tx: mpsc::Sender<Message>,
    next_request_id: Arc<AtomicU64>,
}

impl ConnectionHandle {
    pub(crate) fn new(tx: mpsc::Sender<Message>, next_request_id: Arc<AtomicU64>) -> Self {
        Self {
            tx,
            next_request_id,
        }
    }

    pub(crate) fn sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    pub(crate) fn request_counter(&self) -> Arc<AtomicU64> {
        self.next_request_id.clone()
    }

    pub(crate) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) async fn send(
        &self,
        msg: Message,
    ) -> std::result::Result<(), mpsc::error::SendError<Message>> {
        self.tx.send(msg).await
    }

    pub(crate) fn try_send(&self, msg: Message) -> bool {
        self.tx.try_send(msg).is_ok()
    }
}

/// An active subscription that can be cancelled.
pub(crate) struct SubscriptionEntry {
    pub subscription_id: SubscriptionId,
    pub agent_id: Uuid,
    pub mode: SubscriptionMode,
    #[allow(dead_code)]
    pub cancel: oneshot::Sender<()>,
    pub dst: Route,
    pub lease_deadline: Instant,
}

pub(crate) struct ServerUserState {
    pub(crate) agents: HashMap<Uuid, AgentSession>,
    pub(crate) routes: HashMap<String, ConnectionHandle>,
    pub(crate) registry: AgentRegistry,
    pub(crate) peer_links: HashSet<String>,
    pub(crate) hosts: HashMap<Uuid, Host>,
    pub(crate) active_subscriptions: HashMap<SubscriptionId, SubscriptionEntry>,
}

impl ServerUserState {
    pub(crate) fn new() -> Self {
        Self {
            agents: HashMap::new(),
            routes: HashMap::new(),
            registry: AgentRegistry::new(),
            peer_links: HashSet::new(),
            hosts: HashMap::new(),
            active_subscriptions: HashMap::new(),
        }
    }
}

pub(crate) fn subscription_lease_ms() -> u64 {
    SUBSCRIPTION_LEASE_DURATION
        .as_millis()
        .try_into()
        .expect("subscription lease should fit in u64 milliseconds")
}

pub(crate) struct ServerState {
    pub(crate) config: Config,
    pub(crate) host_id: Uuid,
    pub(crate) is_cloud_server: bool,
    pub(crate) jwt_validator: Option<Arc<JwtValidator>>,
    pub(crate) users: HashMap<Uuid, Arc<RwLock<ServerUserState>>>,
    pub(crate) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

impl ServerState {
    pub(crate) fn new(
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

    pub(crate) fn user_state(&self, user_id: &Uuid) -> Option<Arc<RwLock<ServerUserState>>> {
        self.users.get(user_id).cloned()
    }
}

pub(crate) async fn ensure_user_state(
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
