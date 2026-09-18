//! One account's place in a rich client, and the set of them.
//!
//! An account is a whole device as far as the machines it pairs with are
//! concerned: its own key, its own trust store, its own relay link. Signing
//! in to a second account therefore adds a second device rather than a second
//! name on the first one, and nothing an account knows is visible to its
//! neighbour. This module owns that set and which member of it is on screen;
//! how a session came to exist (an embedded installation, a daemon this
//! process attached to) is the embedder's business and reaches here only
//! through the traits below.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use client::{Client, DeviceIdentity, PeerEntry, PendingPeer};
use futures_util::future::BoxFuture;
use model::{HostId, ProfileId, RelayConnection};
use tokio::sync::{mpsc, watch};
use ui_runtime::{HostInventory, Runtime, RuntimeOptions, ShellEdge, StoreRecovery};
use ui_state::Attention;

/// The account's link to its relay, as far as a screen can influence it.
pub trait Link: Send + Sync {
    /// Stop waiting out the backoff and dial now. Whether the wait was
    /// actually shortened is the connection's to decide.
    fn retry_now(&self);
    /// Whether anybody is looking. A link nobody is reading is put away.
    fn set_active(&self, active: bool);
    /// Every dial since the link was created.
    fn attempts(&self) -> u64;
    /// How many of them happened early because somebody asked.
    fn shortened(&self) -> u64;
}

/// What an account can do to its own device: read its identity and the
/// machines it trusts, and change that trust.
///
/// Every method answers with plain values or one diagnostic sentence. A
/// screen has to say whether pairing was refused, not why in the transport's
/// words; the sentence goes to the log.
pub trait AccountAdmin: Send + Sync {
    fn device_identity(&self) -> BoxFuture<'_, Result<DeviceIdentity, String>>;
    fn list_peers(&self) -> BoxFuture<'_, Result<Vec<PeerEntry>, String>>;
    fn unpair(&self, host: HostId, reason: String) -> BoxFuture<'_, Result<PeerEntry, String>>;
    /// Authenticate a six-digit code against the machine that issued it,
    /// writing no trust yet.
    fn begin_pair_pin(
        &self,
        host: HostId,
        pin: String,
    ) -> BoxFuture<'_, Result<PendingPeer, String>>;
    /// Authenticate the payload an `amux://pair` link carries, writing no
    /// trust yet. A payload that does not parse is a refusal like any other.
    fn begin_pair_link(&self, payload: String) -> BoxFuture<'_, Result<PendingPeer, String>>;
    fn confirm_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<PeerEntry, String>>;
    fn abandon_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<(), String>>;
    /// Pair with the machine a link payload names in one step, trusting it
    /// without the confirmation a person would give. A harness affordance:
    /// a driver proving what a paired device shows needs the trust before
    /// the screens that confirm it exist.
    fn pair_link_now(&self, payload: String) -> BoxFuture<'_, Result<String, String>>;
}

/// One signed-in account: its identity on the network, its link, and the
/// ways in.
pub struct Session {
    /// The identifier the application gave this account. Never parsed here;
    /// it comes back on every event that concerns the account so a reply
    /// cannot be credited to the wrong one.
    pub account: String,
    /// Which profile of the installation this account is, where the
    /// embedder keeps one; used only to give each account its own cache.
    pub profile: ProfileId,
    /// This device, as the account's machines know it.
    pub host: HostId,
    pub relay: watch::Receiver<RelayConnection>,
    pub link: Arc<dyn Link>,
    pub client: Client,
    pub admin: Arc<dyn AccountAdmin>,
    pub inventory: Arc<dyn HostInventory>,
}

/// Where this application keeps what a runtime writes.
pub struct Places {
    pub cache_dir: PathBuf,
    pub report_dir: PathBuf,
    pub log_path: PathBuf,
}

impl Places {
    /// How an account's runtime is configured.
    ///
    /// Artifacts are the account's, so each caches its own; reports are this
    /// application's diagnostics rather than any account's, so the screen's
    /// runtime writes them where a report is looked for. An account nobody is
    /// reading writes neither: it exists to be counted.
    fn options_for(&self, session: &Session, on_screen: bool) -> RuntimeOptions {
        let store_path =
            on_screen.then(|| crate::cache::store_path(&self.cache_dir, &session.account));
        let store_first_frame_seen = store_path
            .as_deref()
            .is_some_and(crate::cache::take_first_frame);
        RuntimeOptions {
            host_inventory: Some(session.inventory.clone()),
            // Which host is this device. Nothing infers it for an embedded
            // runtime, and without it a device cannot tell itself from the
            // machines it is paired with.
            local_host_id: Some(session.host),
            // The account on screen keeps its fleet and conversations in its
            // own store; an account nobody is reading only counts.
            store_path,
            report_dir: on_screen.then(|| self.report_dir.clone()),
            log_path: on_screen.then(|| self.log_path.clone()),
            artifact_cache: on_screen.then(|| {
                self.cache_dir
                    .join("artifacts")
                    .join(session.profile.to_string())
            }),
            chat_window_max_entries: ui_state::store::PHONE_WINDOW_MAX_ENTRIES,
            store_maintenance_budget: store::Budget::phone(),
            store_recovery: StoreRecovery::Relaunch,
            store_first_frame_seen,
            ..Default::default()
        }
    }
}

/// The folds of every account that is not on screen.
///
/// Each runs as a task of its own rather than beside the screen's own fold:
/// an account with machines answering produces messages continuously, and a
/// screen sharing one loop with it would spend every turn on the account
/// nobody is reading. Dropping this stops them all.
pub struct Watchers {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Watchers {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// How many of an account's agents are waiting, as its own fold counts them.
pub type Waiting = (String, usize);

/// Every account this client holds, and the one on screen.
pub struct Sessions {
    pub ui: Runtime,
    /// The active account's link, its way to dial now, and its administration
    /// handle. Copied out of the session so the run loop can await one and
    /// read another without borrowing the whole set.
    pub relay: watch::Receiver<RelayConnection>,
    pub link: Arc<dyn Link>,
    pub admin: Arc<dyn AccountAdmin>,
    pub sessions: Vec<Session>,
    active: usize,
    /// The shell edge of the account most recently switched away from, held
    /// so a diagnostic can produce the one thing a switch cannot recall: a
    /// result that was already in flight for the account being left.
    pub previous_edge: Option<ShellEdge>,
    places: Places,
}

impl Sessions {
    /// Put the named account on screen and start its runtime.
    pub fn open(sessions: Vec<Session>, active: &str, places: Places) -> Result<Self, String> {
        if sessions.is_empty() {
            return Err("a client runs at least one account".into());
        }
        let active = sessions
            .iter()
            .position(|session| session.account == active)
            .ok_or("the active account has no session")?;
        Ok(Self {
            ui: Runtime::start_with_client(
                sessions[active].client.clone(),
                places.options_for(&sessions[active], true),
            ),
            relay: sessions[active].relay.clone(),
            link: sessions[active].link.clone(),
            admin: sessions[active].admin.clone(),
            sessions,
            active,
            previous_edge: None,
            places,
        })
    }

    /// The account on screen, as the application names it.
    pub fn active_account(&self) -> &str {
        &self.sessions[self.active].account
    }

    /// Every account this client holds that is not the one on screen.
    pub fn inactive_accounts(&self) -> BTreeSet<String> {
        self.sessions
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active)
            .map(|(_, session)| session.account.clone())
            .collect()
    }

    pub fn client(&self) -> Client {
        self.sessions[self.active].client.clone()
    }

    /// Put another account on screen.
    ///
    /// The reducer is rebound rather than restarted: the retired selection's
    /// tasks are aborted and its generation retired, so anything the previous
    /// account still delivers is refused before it reaches a Model. The new
    /// selection starts empty, because nothing the previous account showed is
    /// true of this one.
    pub fn switch(&mut self, account: &str) -> Result<(), String> {
        let next = self
            .sessions
            .iter()
            .position(|session| session.account == account)
            .ok_or_else(|| format!("no account named {account}"))?;
        if next == self.active {
            return Ok(());
        }
        self.previous_edge = Some(self.ui.shell_edge());
        let options = self.places.options_for(&self.sessions[next], true);
        self.ui
            .switch_in_place_with_client(self.sessions[next].client.clone(), options);
        self.active = next;
        self.relay = self.sessions[next].relay.clone();
        self.link = self.sessions[next].link.clone();
        self.admin = self.sessions[next].admin.clone();
        Ok(())
    }

    /// Start a fold for every account that is not on screen.
    ///
    /// Called when the app comes to the front and after every switch: these
    /// folds are the foreground subscription the switcher's indicator is read
    /// from, and there is nothing to subscribe to when nobody is looking.
    ///
    /// Each one listens to all of its account's agents, because that is the
    /// only way the count can be real. Nothing runs on this device, so the
    /// policy that keeps a machine's own agents streamed applies to none of
    /// these; an agent nobody subscribed to reports its attention as unknown,
    /// and a badge derived from unknown would be a guess.
    pub fn watch_others(&self, counts: mpsc::UnboundedSender<Waiting>) -> Watchers {
        let tasks = (0..self.sessions.len())
            .filter(|index| *index != self.active)
            .map(|index| {
                let account = self.sessions[index].account.clone();
                let mut ui = Runtime::start_with_client(
                    self.sessions[index].client.clone(),
                    self.places.options_for(&self.sessions[index], false),
                );
                let counts = counts.clone();
                tokio::spawn(async move {
                    let mut listening = HashSet::new();
                    let mut reported: Option<usize> = None;
                    while ui.next_message().await {
                        let unheard: Vec<_> = ui
                            .model()
                            .agents()
                            .map(|card| card.agent.id)
                            .filter(|agent| !listening.contains(agent))
                            .collect();
                        for agent in unheard {
                            listening.insert(agent);
                            ui.note_attached(agent);
                        }
                        let model = ui.model();
                        let waiting = model
                            .agents()
                            .filter(|card| {
                                matches!(
                                    model.effective_attention(card),
                                    Attention::NeedsYou { .. }
                                )
                            })
                            .count();
                        if reported != Some(waiting) {
                            reported = Some(waiting);
                            if counts.send((account.clone(), waiting)).is_err() {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();
        Watchers { tasks }
    }

    /// Put every account's link away, or bring them all back. An account
    /// nobody is looking at holds no connection either.
    pub fn set_active(&self, active: bool) {
        for session in &self.sessions {
            session.link.set_active(active);
        }
    }
}
