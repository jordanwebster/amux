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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use client::{Client, DeviceIdentity, PairingCandidate, PeerEntry, PendingPeer};
use futures_util::future::BoxFuture;
use model::{HostId, ProfileId, RelayConnection, Tier};
use tokio::sync::{mpsc, watch};
use ui_runtime::{HostInventory, Runtime, RuntimeOptions, ShellEdge};
use ui_state::{Attention, CloudState};

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

/// Why a pairing attempt was turned down, in the only distinction a screen
/// draws differently.
///
/// Every way a secret can be wrong is one refusal: whether the code was
/// mistyped, already used, expired or never issued is exactly what somebody
/// guessing codes would want to learn. A machine that is only reachable
/// through a relay this account has not paid for is a different sentence
/// altogether — nothing is wrong with the code, and the person can act on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Refused,
    SubscriptionRequired,
}

/// One machine the platform's browser resolved on this network.
///
/// A dial hint and nothing more: a name, the identity it claims and where to
/// try it. Whether the machine is who it says is settled by the handshake
/// when something dials it, never by the advertisement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoundHost {
    pub host: HostId,
    pub name: String,
    pub version: u32,
    pub addrs: Vec<SocketAddr>,
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
    /// writing no trust yet. The addresses are the ones the candidate the
    /// person tapped carried, tried before the relay is consulted.
    fn begin_pair_pin(
        &self,
        host: HostId,
        pin: String,
        addrs: Vec<SocketAddr>,
    ) -> BoxFuture<'_, Result<PendingPeer, Refusal>>;
    /// Authenticate the payload an `amux://pair` link carries, writing no
    /// trust yet. A payload that does not parse is a refusal like any other.
    /// The addresses the payload itself carries are the ones dialled.
    fn begin_pair_link(&self, payload: String) -> BoxFuture<'_, Result<PendingPeer, Refusal>>;
    /// The machines this profile could pair with: the ones an outside browser
    /// handed over and the ones its relay can see, each with the route an
    /// attempt would take and the addresses it would try.
    fn pairing_candidates(&self) -> BoxFuture<'_, Result<Vec<PairingCandidate>, String>>;
    /// Hand this device the machines the platform's own browser resolved.
    ///
    /// A phone may not browse the local network itself: the system browses
    /// and the app is told, so what this device has found is whatever was
    /// last handed to it. A client whose device browses for itself ignores
    /// this.
    fn hand_over_discovered(&self, found: Vec<FoundHost>) -> BoxFuture<'_, ()>;
    /// Whether the app is in front of somebody, as this device's own links
    /// care about it. Going away closes the links to the machines on this
    /// network — a phone in a pocket must not hold a socket the system will
    /// freeze — and coming back dials them again, found addresses first.
    fn set_foreground(&self, active: bool) -> BoxFuture<'_, ()>;
    fn confirm_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<PeerEntry, String>>;
    fn abandon_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<(), String>>;
    /// Pair with the machine a link payload names in one step, trusting it
    /// without the confirmation a person would give. A harness affordance:
    /// a driver proving what a paired device shows needs the trust before
    /// the screens that confirm it exist.
    fn pair_link_now(&self, payload: String) -> BoxFuture<'_, Result<String, String>>;
    /// Ask the account service what this account buys, now rather than at the
    /// next reconnection. What a purchase that has just gone through is
    /// waiting on: nothing local knows it happened.
    fn refresh_entitlement(&self) -> BoxFuture<'_, Result<Tier, String>>;
}

/// One signed-in account: its identity on the network, its link, and the
/// ways in.
pub struct Session {
    /// The identifier the application gave this account, or nothing where
    /// nobody is signed in. Never parsed here; it comes back on every event
    /// that concerns the account so a reply cannot be credited to the wrong
    /// one.
    ///
    /// A device runs perfectly well with this absent: the machines on its own
    /// network are paired with directly, and an account is what adds reaching
    /// them from somewhere else.
    pub account: Option<String>,
    /// Which profile of the installation this session is. The identity that
    /// outlives an account: the profile a device paired on while signed out
    /// is the one the first account adopts, so its machines and its cache
    /// carry over.
    pub profile: ProfileId,
    /// What this profile's relay link is doing, as a screen words it. Fed to
    /// the runtime so the shared model can answer what a host route and a
    /// subscription prompt depend on.
    pub cloud: watch::Receiver<CloudState>,
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
        RuntimeOptions {
            host_inventory: Some(session.inventory.clone()),
            // Which host is this device. Nothing infers it for an embedded
            // runtime, and without it a device cannot tell itself from the
            // machines it is paired with.
            local_host_id: Some(session.host),
            report_dir: on_screen.then(|| self.report_dir.clone()),
            log_path: on_screen.then(|| self.log_path.clone()),
            artifact_cache: on_screen.then(|| {
                self.cache_dir
                    .join("artifacts")
                    .join(session.profile.to_string())
            }),
            // Only the account on screen reports its cloud state: what the
            // shared model answers about routes and prompts is about what
            // somebody is looking at.
            cloud_status: on_screen.then(|| cloud_states(session.cloud.clone())),
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

/// One profile's cloud states as the runtime consumes them: the state it is
/// in now, and every change after it.
fn cloud_states(
    cloud: watch::Receiver<CloudState>,
) -> futures_util::stream::BoxStream<'static, CloudState> {
    use futures_util::StreamExt;
    futures_util::stream::unfold((cloud, true), |(mut cloud, first)| async move {
        if first {
            let state = cloud.borrow_and_update().clone();
            return Some((state, (cloud, false)));
        }
        cloud.changed().await.ok()?;
        let state = cloud.borrow_and_update().clone();
        Some((state, (cloud, false)))
    })
    .boxed()
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
    /// Put the named account on screen and start its runtime. With nobody
    /// signed in there is one session and no name to give, so the first one
    /// is the one on screen.
    pub fn open(
        sessions: Vec<Session>,
        active: Option<&str>,
        places: Places,
    ) -> Result<Self, String> {
        if sessions.is_empty() {
            return Err("a client runs at least one profile".into());
        }
        let active = match active {
            Some(active) => sessions
                .iter()
                .position(|session| session.account.as_deref() == Some(active))
                .ok_or("the active account has no session")?,
            None => 0,
        };
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

    /// The account on screen, as the application names it, or nothing where
    /// nobody is signed in.
    pub fn active_account(&self) -> Option<&str> {
        self.sessions[self.active].account.as_deref()
    }

    /// Which profile is on screen. What the fleet it remembers is filed under.
    pub fn active_profile(&self) -> ProfileId {
        self.sessions[self.active].profile
    }

    /// Every account this client holds that is not the one on screen.
    pub fn inactive_accounts(&self) -> BTreeSet<String> {
        self.sessions
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active)
            .filter_map(|(_, session)| session.account.clone())
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
            .position(|session| session.account.as_deref() == Some(account))
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
            .filter(|index| self.sessions[*index].account.is_some())
            .map(|index| {
                let account = self.sessions[index]
                    .account
                    .clone()
                    .expect("an account nobody is looking at has a name");
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
