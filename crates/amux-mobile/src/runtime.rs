//! The only owner of the embedded account runtimes and the UI reducer.
//!
//! One installation, one profile per account. A profile is a whole device as
//! far as the machines it pairs with are concerned: its own key, its own
//! trust store, its own relay link. Signing in to a second account therefore
//! adds a second device on this phone rather than a second name on the first
//! one, and nothing an account knows is visible to its neighbour.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use amux::{
    Client, CredentialProvider, CredentialSource, EmbeddedRelay, HostId, Installation,
    InstallationOptions, InstallationRoot, InstallationSettings, Listeners, OperationId,
    ProfileAdmin, ProfileId, RelayConnection, RelayEndpoint, RelayRetry, ShutdownReason,
};
use amux_ui::{Attention, Runtime, RuntimeOptions};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConfig {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub device_name: String,
    pub relay: RelayConfig,
    /// Every account this phone is signed in to, in the order the app lists
    /// them. One of them is on screen; the rest are still connected while the
    /// app is in front of somebody, which is what lets the switcher say that
    /// an account nobody is looking at has something waiting.
    pub accounts: Vec<AccountConfig>,
    /// Which account is on screen. Must name one of the accounts above.
    pub active: String,
    pub log_path: PathBuf,
    #[serde(default = "default_frame_interval_ns")]
    pub frame_interval_ns: u64,
}

/// One signed-in account, as the application names it.
///
/// The identifier is the application's: the bridge stores no account service
/// of its own and never parses it. It comes back on every token request so a
/// reply cannot be credited to the wrong account.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub id: String,
    pub token: TokenSource,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub url: String,
    pub tls: RelayTls,
}

#[derive(Clone, Deserialize)]
pub enum RelayTls {
    System,
    PlainLoopback,
}

#[derive(Clone, Deserialize)]
pub enum TokenSource {
    Static(String),
    Callback,
}

fn default_frame_interval_ns() -> u64 {
    16_666_667
}

impl StartConfig {
    fn installation_settings(&self) -> InstallationSettings {
        InstallationSettings {
            repository_roots: Vec::new(),
            claude: Default::default(),
            host_name: self.device_name.clone(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: self.data_dir.join("keymaps"),
            minimum_client_versions: Default::default(),
            // A phone updates through the App Store; nothing here fetches a
            // manifest, and a reachable address would be a background request
            // nobody asked for.
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        }
    }

    fn installation_root(&self) -> PathBuf {
        self.data_dir.join("installation")
    }

    pub fn endpoint(&self) -> Result<RelayEndpoint, String> {
        if self.frame_interval_ns == 0 || self.frame_interval_ns > 1_000_000_000 {
            return Err("frame_interval_ns must be between 1 and 1000000000".into());
        }
        if !self.data_dir.is_absolute()
            || !self.cache_dir.is_absolute()
            || !self.log_path.is_absolute()
            || self.device_name.is_empty()
        {
            return Err("mobile paths must be absolute and device name must be nonempty".into());
        }
        if self.accounts.is_empty() {
            return Err("a phone runs at least one account".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for account in &self.accounts {
            if account.id.is_empty() || !seen.insert(account.id.as_str()) {
                return Err("account identifiers must be nonempty and distinct".into());
            }
        }
        if !seen.contains(self.active.as_str()) {
            return Err("the active account must be one of the accounts".into());
        }
        match self.relay.tls {
            RelayTls::System => RelayEndpoint::system(&self.relay.url).map_err(|e| e.to_string()),
            RelayTls::PlainLoopback => {
                #[cfg(feature = "debug-tools")]
                {
                    let address = self
                        .relay
                        .url
                        .strip_prefix("http://")
                        .ok_or("plaintext relay must use http://")?
                        .parse()
                        .map_err(|_| "plaintext relay must be a literal socket address")?;
                    RelayEndpoint::plain_loopback(address).map_err(|e| e.to_string())
                }
                #[cfg(not(feature = "debug-tools"))]
                Err("plaintext relay requires debug-tools".into())
            }
        }
    }
}

/// One account's place on this phone: its profile, its link, and the way in.
pub struct Seat {
    pub account: String,
    pub profile: ProfileId,
    pub host: HostId,
    pub relay: watch::Receiver<RelayConnection>,
    /// The way to ask this account's relay connection to stop waiting out its
    /// backoff and dial now. Held beside the profile rather than reached
    /// through it because this is the side of the app that owns the link: the
    /// profile is the device, and this is its way to everything else.
    pub retry: Arc<RelayRetry>,
    pub client: Client,
    pub admin: ProfileAdmin,
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

pub struct MobileRuntime {
    pub ui: Runtime,
    /// The active account's link, its way to dial now, and its administration
    /// handle. Copied out of the seat so the run loop can await one and read
    /// another without borrowing the whole runtime.
    pub relay: watch::Receiver<RelayConnection>,
    pub retry: Arc<RelayRetry>,
    pub admin: ProfileAdmin,
    pub seats: Vec<Seat>,
    active: usize,
    /// The shell edge of the account most recently switched away from, held so
    /// a test can produce the one thing a switch cannot recall: a result that
    /// was already in flight for the account being left.
    #[cfg(all(debug_assertions, feature = "debug-tools"))]
    pub previous_edge: Option<amux_ui::ShellEdge>,
    places: Places,
    installation: Option<Installation>,
}

/// Where this application keeps what a runtime writes.
struct Places {
    cache_dir: PathBuf,
    report_dir: PathBuf,
    log_path: PathBuf,
}

impl MobileRuntime {
    /// Open one profile per account and put the named one on screen.
    ///
    /// `credentials` is asked for one provider per account; the provider it
    /// returns supplies that account's routing tokens and nothing else's.
    pub async fn open(
        config: &StartConfig,
        credentials: impl Fn(&AccountConfig) -> Arc<dyn CredentialProvider>,
    ) -> Result<Self, String> {
        config.endpoint()?;
        // Which provider belongs to which profile is settled after the profile
        // exists, so the installation reads it out of this map rather than
        // being handed a provider it would have to guess an owner for.
        let providers: Arc<Mutex<HashMap<ProfileId, Arc<dyn CredentialProvider>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let looked_up = providers.clone();
        let installation = Installation::open(InstallationOptions {
            root: InstallationRoot::OnDisk(config.installation_root()),
            settings: config.installation_settings(),
            listeners: Listeners::InProcessOnly,
            credentials: CredentialSource::HostProvided(Arc::new(move |id| {
                looked_up
                    .lock()
                    .expect("credential providers poisoned")
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(NoCredentials))
            })),
            identity_http: reqwest::Client::new(),
        })
        .await
        .map_err(|error| error.to_string())?;

        // A phone that has run before already has its profiles; the account
        // list names them, so an account is matched to the profile that was
        // labelled with it and only an account with none gets a new one.
        let mut existing: HashMap<String, ProfileId> = installation
            .profiles()
            .into_iter()
            .filter_map(|profile| {
                Some((
                    profile.record.label.override_name.clone()?,
                    profile.record.id,
                ))
            })
            .collect();
        let mut seats = Vec::with_capacity(config.accounts.len());
        for account in &config.accounts {
            let provider = credentials(account);
            let id = match existing.remove(&account.id) {
                Some(id) => id,
                None => {
                    let profile = installation
                        .create(OperationId::new(), Some(account.id.clone()))
                        .await
                        .map_err(|error| format!("create {}: {error}", account.id))?;
                    if !profile.available {
                        return Err(format!(
                            "create {}: {}",
                            account.id,
                            profile
                                .startup_error
                                .unwrap_or_else(|| "unavailable".into())
                        ));
                    }
                    profile.record.id
                }
            };
            providers
                .lock()
                .expect("credential providers poisoned")
                .insert(id, provider.clone());
            let (connection, relay) = watch::channel(RelayConnection::Connecting);
            let retry = Arc::new(RelayRetry::default());
            installation
                .use_embedded_relay(
                    id,
                    EmbeddedRelay {
                        endpoint: config.endpoint()?,
                        credentials: provider,
                        connection,
                        retry: retry.clone(),
                    },
                )
                .await
                .map_err(|error| format!("relay for {}: {error}", account.id))?;
            let status = installation
                .profiles()
                .into_iter()
                .find(|profile| profile.record.id == id)
                .ok_or("profile vanished after it was created")?;
            seats.push(Seat {
                account: account.id.clone(),
                profile: id,
                host: status.host_id,
                relay,
                retry,
                client: installation
                    .client(id)
                    .map_err(|error| format!("client for {}: {error}", account.id))?,
                admin: installation
                    .admin(id)
                    .await
                    .map_err(|error| format!("admin for {}: {error}", account.id))?,
            });
        }
        let active = seats
            .iter()
            .position(|seat| seat.account == config.active)
            .ok_or("the active account has no profile")?;
        let places = Places {
            cache_dir: config.cache_dir.clone(),
            report_dir: config.data_dir.join("reports"),
            log_path: config.log_path.clone(),
        };
        Ok(Self {
            ui: Runtime::start_with_client(
                seats[active].client.clone(),
                places.options_for(&seats[active], true),
            ),
            relay: seats[active].relay.clone(),
            retry: seats[active].retry.clone(),
            admin: seats[active].admin.clone(),
            seats,
            active,
            #[cfg(all(debug_assertions, feature = "debug-tools"))]
            previous_edge: None,
            places,
            installation: Some(installation),
        })
    }

    /// Every account this phone holds that is not the one on screen.
    pub fn inactive_accounts(&self) -> std::collections::BTreeSet<String> {
        self.seats
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active)
            .map(|(_, seat)| seat.account.clone())
            .collect()
    }

    pub fn client(&self) -> Client {
        self.seats[self.active].client.clone()
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
            .seats
            .iter()
            .position(|seat| seat.account == account)
            .ok_or_else(|| format!("no account named {account}"))?;
        if next == self.active {
            return Ok(());
        }
        #[cfg(all(debug_assertions, feature = "debug-tools"))]
        {
            self.previous_edge = Some(self.ui.shell_edge());
        }
        let options = self.places.options_for(&self.seats[next], true);
        self.ui
            .switch_in_place_with_client(self.seats[next].client.clone(), options);
        self.active = next;
        self.relay = self.seats[next].relay.clone();
        self.retry = self.seats[next].retry.clone();
        self.admin = self.seats[next].admin.clone();
        Ok(())
    }

    /// Start a fold for every account that is not on screen.
    ///
    /// Called when the app comes to the front and after every switch: these
    /// folds are the foreground subscription the switcher's indicator is read
    /// from, and there is nothing to subscribe to when nobody is looking.
    ///
    /// Each one listens to all of its account's agents, because that is the
    /// only way the count can be real. Nothing runs on a phone, so the policy
    /// that keeps a machine's own agents streamed applies to none of these;
    /// an agent nobody subscribed to reports its attention as unknown, and a
    /// badge derived from unknown would be a guess.
    pub fn watch_others(&self, counts: mpsc::UnboundedSender<Waiting>) -> Watchers {
        let tasks = (0..self.seats.len())
            .filter(|index| *index != self.active)
            .map(|index| {
                let account = self.seats[index].account.clone();
                let mut ui = Runtime::start_with_client(
                    self.seats[index].client.clone(),
                    self.places.options_for(&self.seats[index], false),
                );
                let counts = counts.clone();
                tokio::spawn(async move {
                    let mut listening = std::collections::HashSet::new();
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
        for seat in &self.seats {
            seat.retry.set_active(active);
        }
    }

    pub async fn shutdown(&mut self) {
        if let Some(installation) = self.installation.take() {
            installation.shutdown(ShutdownReason::UserRequested).await;
        }
    }
}

impl Places {
    /// How a profile's runtime is configured.
    ///
    /// Artifacts are the account's, so each profile caches its own; reports
    /// are this application's diagnostics rather than any account's, so the
    /// screen's runtime writes them where a report is looked for. An account
    /// nobody is reading writes neither: it exists to be counted.
    fn options_for(&self, seat: &Seat, on_screen: bool) -> RuntimeOptions {
        RuntimeOptions {
            host_inventory: Some(seat.admin.clone()),
            // Which host is this device. Nothing infers it for an embedded
            // runtime, and without it this phone cannot tell itself from the
            // machines it is paired with.
            local_host_id: Some(seat.host),
            report_dir: on_screen.then(|| self.report_dir.clone()),
            log_path: on_screen.then(|| self.log_path.clone()),
            artifact_cache: on_screen.then(|| {
                self.cache_dir
                    .join("artifacts")
                    .join(seat.profile.to_string())
            }),
            ..Default::default()
        }
    }
}

/// An account whose provider has not been recorded yet cannot connect, and
/// says so rather than reaching the relay with nothing.
struct NoCredentials;

#[async_trait::async_trait]
impl CredentialProvider for NoCredentials {
    async fn access_token(&self) -> Result<amux::AccessToken, amux::AuthError> {
        Err(amux::AuthError::Unauthenticated)
    }
    fn invalidate(&self, _: &amux::AccessToken) {}
}
