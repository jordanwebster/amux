//! The only owner of an embedded node installation and its relay link.
//!
//! One installation, one profile per account. A profile is a whole device as
//! far as the machines it pairs with are concerned: its own key, its own
//! trust store, its own relay link. Signing in to a second account therefore
//! adds a second device rather than a second name on the first one, and
//! nothing an account knows is visible to its neighbour.
//!
//! Nothing here talks to an identity service. The application's own account
//! client obtains connect tokens and answers the requests this crate raises;
//! this crate connects to the relay it is told to use. It stops only the
//! installation it created.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use app_runtime::{
    AccountAdmin, CloudState, FoundHost, HostEventStreamFuture, HostInventory, Link, Places,
    Refusal, Session, Sessions, Tier, Token, TokenError, TokenRequest,
};
use client::{Client, DeviceIdentity, PeerEntry, PendingPeer};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use node::{
    AccessToken, AuthError, CredentialProvider, CredentialSource, EmbeddedRelay, HostId,
    Installation, InstallationOptions, InstallationRoot, InstallationSettings, Listeners, Observed,
    OperationId, ProfileAdmin, ProfileEvent, ProfileId, RelayConnection, RelayEndpoint, RelayRetry,
    RelocationPolicy, ShutdownReason,
};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};

/// What the application says when it starts the embedded node.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConfig {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub device_name: String,
    /// The relay the application resolved, where there is one. A device with
    /// nobody signed in has none: it reaches the machines on its own network
    /// directly, and a relay is what an account adds.
    #[serde(default)]
    pub relay: Option<RelayConfig>,
    /// Every account this device is signed in to, in the order the app lists
    /// them. One of them is on screen; the rest are still connected while the
    /// app is in front of somebody, which is what lets the switcher say that
    /// an account nobody is looking at has something waiting. Empty is a
    /// device nobody has signed in on, which is a device that works.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    /// Which account is on screen. Must name one of the accounts above where
    /// there are any. With none, it names the account this device was last
    /// signed in as, so signing out leaves that account's machines on screen
    /// rather than emptying the app.
    #[serde(default)]
    pub active: Option<String>,
    pub log_path: PathBuf,
    #[serde(default = "default_frame_interval_ns")]
    pub frame_interval_ns: u64,
}

/// One signed-in account, as the application names it.
///
/// The identifier is the application's: this crate stores no account names of
/// its own and never parses it. It comes back on every token request so a
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
    /// A cleartext relay on this machine. Accepted only by a build with the
    /// driving tools compiled in; every other build refuses it, whatever the
    /// compilation profile.
    PlainLoopback,
}

#[derive(Clone, Deserialize)]
pub enum TokenSource {
    /// A bearer the application already holds, and what it says the account
    /// buys. A driving fixture: a real token expires and is asked for again.
    Static {
        bearer: String,
        #[serde(default)]
        tier: Option<Tier>,
    },
    Callback,
}

fn default_frame_interval_ns() -> u64 {
    16_666_667
}

impl StartConfig {
    pub fn frame_interval(&self) -> Duration {
        Duration::from_nanos(self.frame_interval_ns)
    }

    pub fn places(&self) -> Places {
        Places {
            cache_dir: self.cache_dir.clone(),
            report_dir: self.data_dir.join("reports"),
            log_path: self.log_path.clone(),
        }
    }

    fn installation_settings(&self) -> InstallationSettings {
        InstallationSettings {
            repository_roots: Vec::new(),
            host_name: self.device_name.clone(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: self.data_dir.join("keymaps"),
            minimum_client_versions: Default::default(),
            // A rich client updates through its own store; nothing here
            // fetches a manifest, and a reachable address would be a
            // background request nobody asked for.
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        }
    }

    fn installation_root(&self) -> PathBuf {
        self.data_dir.join("installation")
    }

    /// The relay this configuration names, nothing where it names none, or
    /// why what it named is not one.
    pub fn endpoint(&self) -> Result<Option<RelayEndpoint>, String> {
        if self.frame_interval_ns == 0 || self.frame_interval_ns > 1_000_000_000 {
            return Err("frame_interval_ns must be between 1 and 1000000000".into());
        }
        if !self.data_dir.is_absolute()
            || !self.cache_dir.is_absolute()
            || !self.log_path.is_absolute()
            || self.device_name.is_empty()
        {
            return Err("paths must be absolute and device name must be nonempty".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for account in &self.accounts {
            if account.id.is_empty() || !seen.insert(account.id.as_str()) {
                return Err("account identifiers must be nonempty and distinct".into());
            }
        }
        // Signed in, the account on screen has to be one of them. Signed out,
        // the name is a memory of the account this device last read, and the
        // profile it points at may well still be here.
        if !self.accounts.is_empty()
            && !self
                .active
                .as_deref()
                .is_some_and(|active| seen.contains(active))
        {
            return Err("the active account must be one of the accounts".into());
        }
        // Nobody signed in means no relay to dial, and a configuration that
        // named one anyway would be a route for an account that does not
        // exist.
        if self.accounts.is_empty() && self.relay.is_some() {
            return Err("a relay belongs to an account".into());
        }
        if !self.accounts.is_empty() && self.relay.is_none() {
            return Err("a signed-in client needs a relay".into());
        }
        let Some(relay) = &self.relay else {
            return Ok(None);
        };
        match relay.tls {
            RelayTls::System => RelayEndpoint::system(&relay.url)
                .map(Some)
                .map_err(|e| e.to_string()),
            RelayTls::PlainLoopback => {
                #[cfg(feature = "debug-tools")]
                {
                    let address = relay
                        .url
                        .strip_prefix("http://")
                        .ok_or("plaintext relay must use http://")?
                        .parse()
                        .map_err(|_| "plaintext relay must be a literal socket address")?;
                    RelayEndpoint::plain_loopback(address)
                        .map(Some)
                        .map_err(|e| e.to_string())
                }
                #[cfg(not(feature = "debug-tools"))]
                Err("plaintext relay requires debug-tools".into())
            }
        }
    }
}

/// The build of this library: the version alone, or the version with
/// `+debug-tools` when the driving tools are compiled in. The suffix is a
/// literal only the debug-tools build contains, so an application binary can
/// be inspected for it to prove which of the two libraries it linked.
pub fn build() -> &'static str {
    #[cfg(feature = "debug-tools")]
    {
        concat!(env!("CARGO_PKG_VERSION"), "+debug-tools")
    }
    #[cfg(not(feature = "debug-tools"))]
    {
        env!("CARGO_PKG_VERSION")
    }
}

/// One account's credentials, asked of the application each time.
///
/// Rust never talks to the identity service: a request goes out with the
/// account it is for, and the application's own account client answers it.
struct Credentials {
    account: String,
    source: TokenSource,
    requests: mpsc::Sender<TokenRequest>,
    next_id: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl CredentialProvider for Credentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        match &self.source {
            TokenSource::Static { bearer, tier } => Ok(AccessToken {
                bearer: bearer.clone(),
                expires_at: None,
                tier: *tier,
            }),
            TokenSource::Callback => {
                let (reply, receive) = oneshot::channel();
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                self.requests
                    .send(TokenRequest {
                        id,
                        account: self.account.clone(),
                        reply,
                    })
                    .await
                    .map_err(|_| AuthError::Unauthenticated)?;
                let reply = tokio::time::timeout(Duration::from_secs(30), receive)
                    .await
                    .map_err(|_| AuthError::Provider("token request timed out".into()))?
                    .map_err(|_| AuthError::Unauthenticated)?;
                match reply {
                    Ok(Token {
                        bearer,
                        expires_at,
                        tier,
                    }) => Ok(AccessToken {
                        bearer,
                        expires_at,
                        tier,
                    }),
                    Err(TokenError::Unauthenticated) => Err(AuthError::Unauthenticated),
                    Err(TokenError::Provider(detail)) => Err(AuthError::Provider(detail)),
                }
            }
        }
    }
    fn invalidate(&self, _token: &AccessToken) {}
}

/// An account whose provider has not been recorded yet cannot connect, and
/// says so rather than reaching the relay with nothing.
struct NoCredentials;

#[async_trait::async_trait]
impl CredentialProvider for NoCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        Err(AuthError::Unauthenticated)
    }
    fn invalidate(&self, _: &AccessToken) {}
}

/// The relay link's retry handle, as the app layer is allowed to touch it.
pub struct RelayLink(Arc<RelayRetry>);

impl RelayLink {
    pub fn new(retry: Arc<RelayRetry>) -> Self {
        Self(retry)
    }
}

impl Link for RelayLink {
    fn retry_now(&self) {
        self.0.now();
    }
    fn set_active(&self, active: bool) {
        self.0.set_active(active);
    }
    fn attempts(&self) -> u64 {
        self.0.attempts()
    }
    fn shortened(&self) -> u64 {
        self.0.shortened()
    }
}

/// The link of a device nobody has signed in on: there is none.
///
/// A screen can ask any session to dial now or to put itself away, and the
/// honest answer here is that nothing happens, because nothing is connected.
/// Not an error and not a silent no-op elsewhere: the absence of a relay is a
/// state this device runs in, not a fault in it.
///
/// It holds the connection state nobody publishes so that state stays
/// readable. A screen watching a closed channel would be told the connection
/// monitor had failed, which is a different thing from a device that is
/// simply not on a relay.
struct NoLink(#[allow(dead_code)] watch::Sender<RelayConnection>);

impl Link for NoLink {
    fn retry_now(&self) {}
    fn set_active(&self, _active: bool) {}
    fn attempts(&self) -> u64 {
        0
    }
    fn shortened(&self) -> u64 {
        0
    }
}

/// One profile's administration, as the app layer is allowed to ask it.
///
/// The node-backed answer to the app layer's traits, for any process that
/// holds a profile's admin handle: the embedded installation this crate
/// opens, or a daemon running in the same process.
pub struct AdminSeat {
    admin: ProfileAdmin,
    /// The installation this profile belongs to, where this process owns one.
    /// What an account buys is not a question about a profile's trust store:
    /// it goes back out through the credentials the relay route was given, and
    /// only the installation that was handed those can ask.
    ///
    /// Held weakly. This seat outlives the installation — a screen holds it
    /// until the app puts the screen down — and an installation that is still
    /// referenced is an installation still holding its root, which the next
    /// one to open it would be refused for.
    entitlement: Option<(std::sync::Weak<Installation>, ProfileId)>,
}

impl AdminSeat {
    /// Administration for a profile of a daemon this process attached to. The
    /// daemon owns its own account service; nothing here can ask on its behalf.
    pub fn new(admin: ProfileAdmin) -> Self {
        Self {
            admin,
            entitlement: None,
        }
    }

    /// Administration for a profile of the installation this process started.
    pub fn embedded(
        admin: ProfileAdmin,
        installation: &Arc<Installation>,
        profile: ProfileId,
    ) -> Self {
        Self {
            admin,
            entitlement: Some((Arc::downgrade(installation), profile)),
        }
    }
}

impl HostInventory for AdminSeat {
    fn subscribe_hosts(&self) -> HostEventStreamFuture<'_> {
        Box::pin(async move {
            self.admin
                .subscribe_hosts()
                .await
                .map(|stream| stream.boxed() as app_runtime::HostEventStream)
        })
    }
}

impl AccountAdmin for AdminSeat {
    fn device_identity(&self) -> BoxFuture<'_, Result<DeviceIdentity, String>> {
        Box::pin(async move {
            self.admin
                .device_identity()
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn list_peers(&self) -> BoxFuture<'_, Result<Vec<PeerEntry>, String>> {
        Box::pin(async move { self.admin.list_peers().await.map_err(|e| e.to_string()) })
    }
    fn unpair(&self, host: HostId, reason: String) -> BoxFuture<'_, Result<PeerEntry, String>> {
        Box::pin(async move {
            self.admin
                .unpair(host, reason)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn begin_pair_pin(
        &self,
        host: HostId,
        pin: String,
        addrs: Vec<std::net::SocketAddr>,
    ) -> BoxFuture<'_, Result<PendingPeer, Refusal>> {
        Box::pin(async move {
            self.admin
                .begin_pair_pin(host, &pin, &addrs)
                .await
                .map_err(refusal)
        })
    }
    fn begin_pair_link(&self, payload: String) -> BoxFuture<'_, Result<PendingPeer, Refusal>> {
        Box::pin(async move {
            // A link that will not parse is refused in the same words a wrong
            // code is: what an unreadable link proves about the machine that
            // issued it is nothing.
            let payload = node::parse_qr_pairing_payload(&payload).map_err(|_| Refusal::Refused)?;
            self.admin.begin_pair_qr(&payload).await.map_err(refusal)
        })
    }
    fn pairing_candidates(&self) -> BoxFuture<'_, Result<Vec<client::PairingCandidate>, String>> {
        Box::pin(async move {
            self.admin
                .list_pairing_hosts()
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn set_foreground(&self, active: bool) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // The relay link is the session's own business and is put away
            // separately; what this reaches is the links to the machines on
            // this network, which only an installation this process owns has.
            let Some((installation, _)) = &self.entitlement else {
                return;
            };
            let Some(installation) = installation.upgrade() else {
                return;
            };
            match active {
                true => installation.host_resume().await,
                false => installation.host_suspend().await,
            }
        })
    }
    fn hand_over_discovered(&self, found: Vec<FoundHost>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Only a device this process browses for has a browser to hand a
            // set to. A profile of a daemon this process attached to browses
            // the network itself, and a set found here would be a second
            // opinion it never asked for.
            let Some((installation, _)) = &self.entitlement else {
                return;
            };
            let Some(installation) = installation.upgrade() else {
                return;
            };
            installation
                .hand_over_discovered(
                    found
                        .into_iter()
                        .map(|host| node::discovery::Advertisement {
                            host_id: host.host,
                            name: host.name,
                            version: host.version,
                            addrs: host.addrs,
                        })
                        .collect(),
                )
                .await;
        })
    }
    fn confirm_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<PeerEntry, String>> {
        Box::pin(async move {
            self.admin
                .confirm_pair(pending)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn abandon_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.admin
                .abandon_pair(pending)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn refresh_entitlement(&self) -> BoxFuture<'_, Result<Tier, String>> {
        Box::pin(async move {
            let (installation, profile) = self
                .entitlement
                .as_ref()
                .ok_or("this client holds no account service")?;
            let installation = installation.upgrade().ok_or("the device has stopped")?;
            installation
                .refresh_entitlement(*profile)
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn pair_link_now(&self, payload: String) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            let payload = node::parse_qr_pairing_payload(&payload).map_err(|e| e.to_string())?;
            // Authenticate the link, then commit trust in the same breath.
            // Pairing has only the two-phase form, so standing in for the
            // person means answering the pending peer immediately rather
            // than calling a one-shot the protocol no longer offers.
            let pending = self
                .admin
                .begin_pair_qr(&payload)
                .await
                .map_err(|e| e.to_string())?;
            self.admin
                .confirm_pair(pending)
                .await
                .map(|peer| peer.name)
                .map_err(|e| e.to_string())
        })
    }
}

/// What a pairing failure is worth saying to a screen.
///
/// A machine only a relay could reach, on an account that has not paid for
/// one, is the single failure a person can do something about; every other
/// way an attempt can end tells somebody guessing codes nothing.
fn refusal(error: client::PairingError) -> Refusal {
    match error {
        client::PairingError::PaymentRequired => Refusal::SubscriptionRequired,
        _ => Refusal::Refused,
    }
}

/// The installation this process created, and the sessions it opened on it.
/// Writes what this runtime decides to the log a report reads.
///
/// A device's runtime had no tracing sink of any kind, so the only account of
/// what it did — which address it dialled, why a link never came up — was
/// discarded as it was written, and every report's log tail was empty. Only
/// the build with the driving tools does this: on a device nobody is
/// debugging, a file that grows for the life of an installation buys nothing,
/// and the report is written from a recording rather than from prose.
///
/// Once per process, and the file starts empty: a driver reads this run.
#[cfg(feature = "debug-tools")]
fn write_tracing_to(log_path: &std::path::Path) {
    use tracing_subscriber::EnvFilter;

    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let mut installed = false;
    INSTALLED.get_or_init(|| installed = true);
    if !installed {
        return;
    }
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::File::create(log_path) else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("warn,node=debug,app_runtime=debug,app_embedded=debug")
        }))
        .with_writer(move || file.try_clone().expect("clone the runtime log handle"))
        .try_init();
}

#[cfg(not(feature = "debug-tools"))]
fn write_tracing_to(_log_path: &std::path::Path) {}

pub struct Embedded {
    pub sessions: Sessions,
    installation: Option<Arc<Installation>>,
    /// Turns each profile's own account of its cloud link into the states a
    /// screen words. Stopped with this crate's installation.
    statuses: Option<tokio::task::JoinHandle<()>>,
}

/// One profile this process opened, before it became a session.
struct Opened {
    account: Option<String>,
    id: ProfileId,
    relay: Option<(watch::Receiver<RelayConnection>, Arc<RelayRetry>)>,
}

/// What a screen says about a profile's link, from what the profile observed.
///
/// Nobody signed in is signed out whatever the link is doing, because there is
/// no link: a device with no account reaches the machines on its own network
/// and nothing else.
fn cloud_state(observed: &Observed, signed_in: bool) -> CloudState {
    if !signed_in {
        return CloudState::SignedOut;
    }
    match observed {
        Observed::Local | Observed::Connecting => CloudState::Connecting,
        Observed::Connected { tier, carrier } => CloudState::Connected {
            tier: *tier,
            carrier: *carrier,
        },
        Observed::Retrying | Observed::UpdateRequired { .. } | Observed::StartupFailed => {
            CloudState::Retrying
        }
        Observed::AuthenticationRequired => CloudState::AuthRequired,
    }
}

impl Embedded {
    /// Open one profile per account and put the named one on screen, or open
    /// the one profile a device with nobody signed in runs.
    ///
    /// Token requests for every account go out on `requests`, each naming
    /// the account it is for.
    pub async fn open(
        config: &StartConfig,
        requests: mpsc::Sender<TokenRequest>,
    ) -> Result<Self, String> {
        let endpoint = config.endpoint()?;
        write_tracing_to(&config.log_path);
        let next_id = Arc::new(AtomicU64::new(1));
        // What this device has found is whatever the application last handed
        // over, so the browser is the app's and every profile reads it.
        let discovery = Arc::new(node::discovery::ScriptedDiscovery::new());
        // Which provider belongs to which profile is settled after the profile
        // exists, so the installation reads it out of this map rather than
        // being handed a provider it would have to guess an owner for.
        let providers: Arc<std::sync::Mutex<HashMap<ProfileId, Arc<dyn CredentialProvider>>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let looked_up = providers.clone();
        let installation = Installation::open(InstallationOptions {
            relocation: RelocationPolicy::Rebase,
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
            host_factory: None,
            // The application browses this device's network; nothing here
            // asks the system for it, because on a phone only the system may.
            discovery: Some(discovery.clone()),
        })
        .await
        .map_err(|error| error.to_string())?;
        // Shared with each profile's administration, which has to be able to
        // ask the account service a question the profile itself cannot.
        let installation = Arc::new(installation);

        let opened = match config.accounts.is_empty() {
            true => vec![signed_out_profile(&installation, config.active.as_deref()).await?],
            false => {
                let mut opened = Vec::with_capacity(config.accounts.len());
                for account in &config.accounts {
                    opened.push(
                        signed_in_profile(
                            &installation,
                            account,
                            endpoint.as_ref().expect("a signed-in client has a relay"),
                            &providers,
                            &requests,
                            &next_id,
                        )
                        .await?,
                    );
                }
                opened
            }
        };

        // Each profile's link state, as the screen words it, before any of
        // them has been reported: a session that has not been told anything
        // yet says what is true of it rather than nothing at all.
        let mut clouds = HashMap::new();
        let mut sessions = Vec::with_capacity(opened.len());
        for profile in &opened {
            let (state, cloud) =
                watch::channel(cloud_state(&Observed::Local, profile.account.is_some()));
            clouds.insert(profile.id, (state, profile.account.is_some()));
            let status = installation
                .profiles()
                .into_iter()
                .find(|status| status.record.id == profile.id)
                .ok_or("profile vanished after it was opened")?;
            let admin = Arc::new(AdminSeat::embedded(
                installation
                    .admin(profile.id)
                    .await
                    .map_err(|error| format!("admin for {}: {error}", profile.id))?,
                &installation,
                profile.id,
            ));
            let (relay, link): (watch::Receiver<RelayConnection>, Arc<dyn Link>) =
                match &profile.relay {
                    Some((relay, retry)) => (relay.clone(), Arc::new(RelayLink(retry.clone()))),
                    // Nobody signed in has no link to report on and nothing to
                    // ask of one. Saying so is the honest answer to a screen
                    // that draws a connection: this device is not disconnected
                    // from a relay, it is not on one.
                    None => {
                        let (state, relay) = watch::channel(RelayConnection::Disconnected {
                            reason: node::DisconnectReason::Stopped,
                        });
                        (relay, Arc::new(NoLink(state)))
                    }
                };
            sessions.push(Session {
                account: profile.account.clone(),
                profile: profile.id,
                host: status.host_id,
                relay,
                cloud,
                link,
                client: installation
                    .client(profile.id)
                    .map_err(|error| format!("client for {}: {error}", profile.id))?,
                inventory: admin.clone(),
                admin,
            });
        }
        let statuses = watch_profiles(&installation, clouds);
        let sessions = Sessions::open(
            sessions,
            config
                .active
                .as_deref()
                .filter(|_| !config.accounts.is_empty()),
            config.places(),
        )?;
        // Where each account's remembered fleet is, for the launch after this
        // one: a launch has rows to draw before it has started anything, and
        // profile identifiers are the installation's to make.
        let mut directory: std::collections::BTreeMap<String, ProfileId> = sessions
            .sessions
            .iter()
            .filter_map(|session| Some((session.account.clone()?, session.profile)))
            .collect();
        directory.insert(String::new(), sessions.active_profile());
        let _ = app_runtime::cache::remember_profiles(&config.cache_dir, &directory);
        Ok(Self {
            sessions,
            installation: Some(installation),
            statuses: Some(statuses),
        })
    }

    /// The client of the account on screen.
    pub fn client(&self) -> Client {
        self.sessions.client()
    }

    /// Stop the installation this process created. Nothing else is stopped:
    /// every view and subscription is the app layer's to close.
    pub async fn shutdown(&mut self) {
        if let Some(statuses) = self.statuses.take() {
            statuses.abort();
        }
        if let Some(installation) = self.installation.take() {
            installation.shutdown(ShutdownReason::UserRequested).await;
        }
    }
}

/// The profile a device with nobody signed in runs on.
///
/// Preferably the one the account last on screen was labelled with, so signing
/// out leaves that account's machines where they were rather than emptying the
/// app. Otherwise the unbound profile — the one carrying no account's label —
/// which is the profile a phone that has never been signed in pairs on, and
/// the one the first account will adopt.
async fn signed_out_profile(
    installation: &Installation,
    last: Option<&str>,
) -> Result<Opened, String> {
    let profiles = installation.profiles();
    let remembered = last.and_then(|last| {
        profiles
            .iter()
            .find(|profile| profile.record.label.override_name.as_deref() == Some(last))
    });
    let unbound = profiles
        .iter()
        .find(|profile| profile.record.label.override_name.is_none());
    let id = match remembered.or(unbound) {
        Some(profile) => profile.record.id,
        None => {
            let created = installation
                .create(OperationId::new(), None)
                .await
                .map_err(|error| format!("create the first profile: {error}"))?;
            available(&created)?;
            created.record.id
        }
    };
    Ok(Opened {
        account: None,
        id,
        relay: None,
    })
}

/// The profile one account runs on, and its relay link.
///
/// An account that has been on this device before has a profile labelled with
/// it. An account signing in for the first time adopts the unbound profile if
/// there is one — that is what keeps the machines a phone paired with before
/// anybody signed in — and otherwise gets a profile of its own. A profile
/// already labelled with somebody is never relabelled: a second account is a
/// second device as far as the machines either of them knows are concerned.
async fn signed_in_profile(
    installation: &Installation,
    account: &AccountConfig,
    endpoint: &RelayEndpoint,
    providers: &Arc<std::sync::Mutex<HashMap<ProfileId, Arc<dyn CredentialProvider>>>>,
    requests: &mpsc::Sender<TokenRequest>,
    next_id: &Arc<AtomicU64>,
) -> Result<Opened, String> {
    let profiles = installation.profiles();
    let labelled = profiles
        .iter()
        .find(|profile| profile.record.label.override_name.as_deref() == Some(&account.id));
    let id = match labelled {
        Some(profile) => profile.record.id,
        None => match profiles
            .iter()
            .find(|profile| profile.record.label.override_name.is_none())
        {
            // Adoption: the same profile, now under a name. Its key, its
            // trust store and the fleet it remembers are the ones this device
            // already had, because it is the same device.
            Some(profile) => {
                let adopted = installation
                    .rename(
                        OperationId::new(),
                        profile.record.id,
                        profile.record.revision,
                        Some(account.id.clone()),
                    )
                    .await
                    .map_err(|error| format!("adopt for {}: {error}", account.id))?;
                available(&adopted)?;
                adopted.record.id
            }
            None => {
                let created = installation
                    .create(OperationId::new(), Some(account.id.clone()))
                    .await
                    .map_err(|error| format!("create {}: {error}", account.id))?;
                available(&created)?;
                created.record.id
            }
        },
    };
    let provider: Arc<dyn CredentialProvider> = Arc::new(Credentials {
        account: account.id.clone(),
        source: account.token.clone(),
        requests: requests.clone(),
        next_id: next_id.clone(),
    });
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
                endpoint: endpoint.clone(),
                credentials: provider,
                connection,
                retry: retry.clone(),
            },
        )
        .await
        .map_err(|error| format!("relay for {}: {error}", account.id))?;
    Ok(Opened {
        account: Some(account.id.clone()),
        id,
        relay: Some((relay, retry)),
    })
}

fn available(profile: &node::ProfileStatus) -> Result<(), String> {
    match profile.available {
        true => Ok(()),
        false => Err(profile
            .startup_error
            .clone()
            .unwrap_or_else(|| "unavailable".into())),
    }
}

/// Turn every profile's own account of its link into the state its session
/// publishes, for as long as the installation runs.
fn watch_profiles(
    installation: &Installation,
    clouds: HashMap<ProfileId, (watch::Sender<CloudState>, bool)>,
) -> tokio::task::JoinHandle<()> {
    let mut watch = installation.watch();
    tokio::spawn(async move {
        while let Some(event) = watch.recv().await {
            let ProfileEvent::Upserted { profile, .. } = event else {
                continue;
            };
            if let Some((state, signed_in)) = clouds.get(&profile.record.id) {
                state.send_if_modified(|held| {
                    let next = cloud_state(&profile.observed, *signed_in);
                    let changed = *held != next;
                    *held = next;
                    changed
                });
            }
        }
    })
}
