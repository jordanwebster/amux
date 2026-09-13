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
    AccountAdmin, HostEventStreamFuture, HostInventory, Link, Places, Session, Sessions, Token,
    TokenError, TokenRequest,
};
use client::{Client, DeviceIdentity, PeerEntry, PendingPeer};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use node::{
    AccessToken, AuthError, CredentialProvider, CredentialSource, EmbeddedRelay, HostId,
    Installation, InstallationOptions, InstallationRoot, InstallationSettings, Listeners,
    OperationId, ProfileAdmin, ProfileId, RelayConnection, RelayEndpoint, RelayRetry,
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
    pub relay: RelayConfig,
    /// Every account this device is signed in to, in the order the app lists
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
    Static(String),
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

    /// The relay this configuration names, or why it is not one.
    pub fn endpoint(&self) -> Result<RelayEndpoint, String> {
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
        if self.accounts.is_empty() {
            return Err("a client runs at least one account".into());
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
            TokenSource::Static(bearer) => Ok(AccessToken {
                bearer: bearer.clone(),
                expires_at: None,
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
                    Ok(Token { bearer, expires_at }) => Ok(AccessToken { bearer, expires_at }),
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

/// One profile's administration, as the app layer is allowed to ask it.
///
/// The node-backed answer to the app layer's traits, for any process that
/// holds a profile's admin handle: the embedded installation this crate
/// opens, or a daemon running in the same process.
pub struct AdminSeat(ProfileAdmin);

impl AdminSeat {
    pub fn new(admin: ProfileAdmin) -> Self {
        Self(admin)
    }
}

impl HostInventory for AdminSeat {
    fn subscribe_hosts(&self) -> HostEventStreamFuture<'_> {
        Box::pin(async move {
            self.0
                .subscribe_hosts()
                .await
                .map(|stream| stream.boxed() as app_runtime::HostEventStream)
        })
    }
}

impl AccountAdmin for AdminSeat {
    fn device_identity(&self) -> BoxFuture<'_, Result<DeviceIdentity, String>> {
        Box::pin(async move { self.0.device_identity().await.map_err(|e| e.to_string()) })
    }
    fn list_peers(&self) -> BoxFuture<'_, Result<Vec<PeerEntry>, String>> {
        Box::pin(async move { self.0.list_peers().await.map_err(|e| e.to_string()) })
    }
    fn unpair(&self, host: HostId, reason: String) -> BoxFuture<'_, Result<PeerEntry, String>> {
        Box::pin(async move { self.0.unpair(host, reason).await.map_err(|e| e.to_string()) })
    }
    fn begin_pair_pin(
        &self,
        host: HostId,
        pin: String,
    ) -> BoxFuture<'_, Result<PendingPeer, String>> {
        Box::pin(async move {
            self.0
                .begin_pair_pin(host, &pin)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn begin_pair_link(&self, payload: String) -> BoxFuture<'_, Result<PendingPeer, String>> {
        Box::pin(async move {
            // A link that will not parse is refused in the same words a wrong
            // code is: what an unreadable link proves about the machine that
            // issued it is nothing.
            let payload = node::parse_qr_pairing_payload(&payload).map_err(|e| e.to_string())?;
            self.0
                .begin_pair_qr(&payload)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn confirm_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<PeerEntry, String>> {
        Box::pin(async move {
            self.0
                .confirm_pair(pending)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn abandon_pair(&self, pending: PendingPeer) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0
                .abandon_pair(pending)
                .await
                .map_err(|e| e.to_string())
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
                .0
                .begin_pair_qr(&payload)
                .await
                .map_err(|e| e.to_string())?;
            self.0
                .confirm_pair(pending)
                .await
                .map(|peer| peer.name)
                .map_err(|e| e.to_string())
        })
    }
}

/// The installation this process created, and the sessions it opened on it.
pub struct Embedded {
    pub sessions: Sessions,
    installation: Option<Installation>,
}

impl Embedded {
    /// Open one profile per account and put the named one on screen.
    ///
    /// Token requests for every account go out on `requests`, each naming
    /// the account it is for.
    pub async fn open(
        config: &StartConfig,
        requests: mpsc::Sender<TokenRequest>,
    ) -> Result<Self, String> {
        config.endpoint()?;
        let next_id = Arc::new(AtomicU64::new(1));
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
        })
        .await
        .map_err(|error| error.to_string())?;

        // A device that has run before already has its profiles; the account
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
        let mut sessions = Vec::with_capacity(config.accounts.len());
        for account in &config.accounts {
            let provider: Arc<dyn CredentialProvider> = Arc::new(Credentials {
                account: account.id.clone(),
                source: account.token.clone(),
                requests: requests.clone(),
                next_id: next_id.clone(),
            });
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
            let admin = Arc::new(AdminSeat(
                installation
                    .admin(id)
                    .await
                    .map_err(|error| format!("admin for {}: {error}", account.id))?,
            ));
            sessions.push(Session {
                account: account.id.clone(),
                profile: id,
                host: status.host_id,
                relay,
                link: Arc::new(RelayLink(retry)),
                client: installation
                    .client(id)
                    .map_err(|error| format!("client for {}: {error}", account.id))?,
                inventory: admin.clone(),
                admin,
            });
        }
        let sessions = Sessions::open(sessions, &config.active, config.places())?;
        Ok(Self {
            sessions,
            installation: Some(installation),
        })
    }

    /// The client of the account on screen.
    pub fn client(&self) -> Client {
        self.sessions.client()
    }

    /// Stop the installation this process created. Nothing else is stopped:
    /// every view and subscription is the app layer's to close.
    pub async fn shutdown(&mut self) {
        if let Some(installation) = self.installation.take() {
            installation.shutdown(ShutdownReason::UserRequested).await;
        }
    }
}
