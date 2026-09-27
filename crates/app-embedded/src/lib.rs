//! The daemon's profile runtime, hosted in the phone's own process.
//!
//! The phone runs no agents, but it is a host like any other to the
//! machines it pairs with: it has its own identity and trust, its own store
//! of replica rows, direct links on the local network and a relay link once
//! an account is bound. Its chats read that store through the same client
//! service the terminal dials over a socket, called in process here; no
//! client opens the store.
//!
//! Which replica agents keep a source open is switchable: every listed
//! agent while the app is in front of somebody, and only the ones a chat
//! asks for when a push wakes it in the background.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use app_runtime::values::{
    AccountBinding, AccountView, Bearer, Found, Identity, PairedPeer, PendingPair, RelayLink,
    Roster,
};
pub use app_runtime::values::{PairRequest, StartConfig};
use client::{Client, Clock, InProcess, SystemClock};
use node::{
    ClientApi, CloudOptions, Daemon, DiscoveryFactory, EdgeOptions, FrontDoor, LanOptions,
    ProfileId, ProfileRuntime, SourcePolicy, StartError, StartOptions,
};
use sha2::{Digest, Sha256};
use tonic::Request;
use wire::profile_service_server::ProfileService as _;
use wire::{
    BeginPairRequest, BindProfileRequest, HostVia, Intent, ListProfilesRequest, Observed,
    PeerEntry, PeerRef, PeerVia, PendingPairRequest, ProfileBeginPairRequest, ProfileInfo,
    ProfileOperation, ProfilePendingPairRequest, ProfileRequest, ProfileUnpairRequest, Tier,
    begin_pair_request, peer_ref,
};

/// What a test or a driving build changes about the network edge.
#[derive(Clone)]
pub struct EdgeOverrides {
    /// Where the relay link dials instead of where the cloud names.
    pub cloud: CloudOptions,
    /// Local-network discovery; the phone's system browses and hands its
    /// results over, so there is none by default.
    pub discovery: Option<DiscoveryFactory>,
    /// Where direct links listen: every interface by default.
    pub lan_bind: SocketAddr,
}

impl Default for EdgeOverrides {
    fn default() -> Self {
        EdgeOverrides {
            cloud: CloudOptions::default(),
            discovery: None,
            lan_bind: SocketAddr::from(([0, 0, 0, 0], 0)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedError {
    #[error(transparent)]
    Start(#[from] StartError),
    #[error("the installation hosts no profile")]
    NoProfile,
    #[error("{}", .0.message())]
    Refused(#[from] tonic::Status),
    #[error(transparent)]
    Link(#[from] node::QrPairingError),
    #[error("{0}")]
    Account(#[from] node::AuthError),
    #[error("this device's profile is not listed")]
    NoInfo,
}

/// One installation with its one profile, in process.
pub struct EmbeddedRuntime {
    // Held for its shutdown; it is not shared between threads while held.
    daemon: Mutex<Option<Daemon>>,
    door: FrontDoor,
    data_dir: PathBuf,
    profile: ProfileId,
    runtime: Arc<ProfileRuntime>,
    clock: Arc<dyn Clock>,
}

impl EmbeddedRuntime {
    /// Starts the installation under `config.data_dir`, creating its
    /// profile on first start.
    pub async fn start(config: &StartConfig) -> Result<EmbeddedRuntime, EmbeddedError> {
        Self::start_with(config, EdgeOverrides::default(), Arc::new(SystemClock)).await
    }

    pub async fn start_with(
        config: &StartConfig,
        overrides: EdgeOverrides,
        clock: Arc<dyn Clock>,
    ) -> Result<EmbeddedRuntime, EmbeddedError> {
        let edge = EdgeOptions {
            host_name: config.device_name.clone(),
            // The phone runs no agents.
            kinds: Vec::new(),
            lan: config.lan.then(|| LanOptions {
                bind: overrides.lan_bind,
                socket: None,
            }),
            discovery: overrides.discovery,
            discovery_scope: config.discovery_scope.clone(),
            dial: config.lan,
            link_socket: false,
            cloud: overrides.cloud,
        };
        let options = StartOptions {
            data_dir: config.data_dir.clone(),
            // The phone has no boot id to read; each start is its own boot.
            boot_id: Some(uuid::Uuid::new_v4().to_string()),
            launch: node::Launch::default(),
            clock: clock.clone(),
            push: Arc::new(node::NoopSender),
            daemon_log: config.log_path.clone(),
            front_door: None,
            edge,
        };
        let daemon = node::start(options, None).await?;
        let runtime = daemon
            .profiles()
            .into_iter()
            .next()
            .ok_or(EmbeddedError::NoProfile)?;
        let profile = runtime.profile();
        Ok(EmbeddedRuntime {
            door: daemon.front_door(),
            data_dir: daemon.data_dir().to_owned(),
            daemon: Mutex::new(Some(daemon)),
            profile,
            runtime,
            clock,
        })
    }

    /// The client service in process, as the chats and the fleet call it.
    pub fn client(&self) -> Arc<dyn Client> {
        Arc::new(InProcess::new(ClientApi::new(&self.runtime, None)))
    }

    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    pub fn runtime(&self) -> &Arc<ProfileRuntime> {
        &self.runtime
    }

    /// This device's host id, as its peers pin it.
    pub fn host_id(&self) -> Vec<u8> {
        self.runtime.host().as_bytes().to_vec()
    }

    /// `Listed` in the foreground: every listed agent keeps a source.
    /// `OnDemand` when a push wakes the app in the background: only the
    /// agents a chat asks for.
    pub fn set_source_policy(&self, policy: SourcePolicy) {
        self.runtime.set_source_policy(policy);
    }

    pub fn source_policy(&self) -> SourcePolicy {
        self.runtime.source_policy()
    }

    fn door(&self) -> &FrontDoor {
        &self.door
    }

    fn profile_id(&self) -> String {
        self.profile.to_string()
    }

    /// Reaches a machine by the PIN its person reads out or the link its QR
    /// code carries, and authenticates it; nothing is trusted until the
    /// attempt is confirmed.
    pub async fn begin_pair(&self, request: &PairRequest) -> Result<PendingPair, EmbeddedError> {
        let pairing = match request {
            PairRequest::Pin {
                host_id,
                pin,
                addrs,
            } => BeginPairRequest {
                host_id: host_id.clone(),
                secret: Some(begin_pair_request::Secret::Pin(
                    pin.chars().filter(char::is_ascii_digit).collect(),
                )),
                addrs: addrs.clone(),
            },
            PairRequest::Link(link) => {
                let invitation = node::parse_pair_link(link)?;
                BeginPairRequest {
                    host_id: invitation.host_id.as_bytes().to_vec(),
                    secret: Some(begin_pair_request::Secret::QrSecret(invitation.secret)),
                    addrs: invitation.addrs.iter().map(ToString::to_string).collect(),
                }
            }
        };
        let pending = self
            .door()
            .begin_pair(Request::new(ProfileBeginPairRequest {
                operation_id: operation(),
                profile_id: self.profile_id(),
                pairing: Some(pairing),
            }))
            .await?
            .into_inner();
        let via = match pending.via() {
            PeerVia::Direct => HostVia::Direct,
            PeerVia::Relay => HostVia::Relay,
            PeerVia::Ssh => HostVia::Ssh,
            PeerVia::Unspecified => HostVia::Unspecified,
        };
        let peer = pending.peer.unwrap_or_default();
        Ok(PendingPair {
            token: pending.token,
            host_id: peer.host_id,
            name: peer.name,
            fingerprint: fingerprint(&peer.pubkey),
            expires_at_ms: peer.expires_at_unix_ms,
            via,
        })
    }

    /// Trusts the machine an attempt reached.
    pub async fn confirm_pair(&self, token: &[u8]) -> Result<PeerEntry, EmbeddedError> {
        Ok(self
            .door()
            .confirm_pair(Request::new(ProfilePendingPairRequest {
                operation_id: operation(),
                profile_id: self.profile_id(),
                pairing: Some(PendingPairRequest {
                    token: token.to_vec(),
                }),
            }))
            .await?
            .into_inner()
            .peer
            .unwrap_or_default())
    }

    /// Turns away the machine an attempt reached, telling it so.
    pub async fn abandon_pair(&self, token: &[u8]) -> Result<(), EmbeddedError> {
        self.door()
            .abandon_pair(Request::new(ProfilePendingPairRequest {
                operation_id: operation(),
                profile_id: self.profile_id(),
                pairing: Some(PendingPairRequest {
                    token: token.to_vec(),
                }),
            }))
            .await?;
        Ok(())
    }

    /// Pairs in one step, trusting whoever the secret reaches.
    pub async fn pair(&self, request: &PairRequest) -> Result<PeerEntry, EmbeddedError> {
        let pending = self.begin_pair(request).await?;
        self.confirm_pair(&pending.token).await
    }

    /// This device and the machines it trusts, sorted by name.
    pub async fn roster(&self) -> Result<Roster, EmbeddedError> {
        let request = || {
            Request::new(ProfileRequest {
                profile_id: self.profile_id(),
            })
        };
        let identity = self
            .door()
            .get_device_identity(request())
            .await?
            .into_inner();
        let mut peers: Vec<PairedPeer> = self
            .door()
            .list_peers(request())
            .await?
            .into_inner()
            .peers
            .into_iter()
            .map(|peer| PairedPeer {
                fingerprint: fingerprint(&peer.pubkey),
                host_id: peer.host_id,
                name: peer.name,
                paired_at_ms: peer.paired_at_unix_ms,
            })
            .collect();
        peers.sort_by_key(|peer| peer.name.to_lowercase());
        Ok(Roster {
            identity: Identity {
                fingerprint: fingerprint(&identity.pubkey),
                host_id: identity.host_id,
                name: identity.name,
            },
            peers,
        })
    }

    /// Hands over the whole set the phone's own browser found on the local
    /// network; only the system may browse there.
    pub fn discovered(&self, found: Vec<Found>) {
        let Some(edge) = self.runtime.edge() else {
            return;
        };
        let found = found
            .into_iter()
            .filter_map(|found| {
                Some(node::harness::Advertisement {
                    host_id: node::HostId::from_slice(&found.host_id).ok()?,
                    name: found.name,
                    version: found.version,
                    addrs: found
                        .addrs
                        .iter()
                        .filter_map(|addr| addr.parse().ok())
                        .collect(),
                    scope: found.scope,
                })
            })
            .collect();
        edge.hand_over_discovered(found);
    }

    /// The account the profile is bound to, and its relay link.
    pub async fn account(&self) -> Result<AccountView, EmbeddedError> {
        let info = self
            .door()
            .list_profiles(Request::new(ListProfilesRequest {}))
            .await?
            .into_inner()
            .profiles
            .into_iter()
            .find(|info| info.id == self.profile_id())
            .ok_or(EmbeddedError::NoInfo)?;
        Ok(account_view(&info))
    }

    /// A bearer for the account service, refreshed by the profile when it
    /// is about to expire.
    pub async fn access_token(&self) -> Result<Bearer, EmbeddedError> {
        let edge = self.runtime.edge().ok_or(EmbeddedError::NoProfile)?;
        let token = edge.access_token().await?;
        Ok(Bearer {
            bearer: token.bearer,
            expires_at_ms: token.expires_at.and_then(|at| {
                at.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_millis() as i64)
            }),
        })
    }

    /// Stops trusting a paired machine, telling it so where it can be
    /// reached.
    pub async fn unpair(&self, host_id: &[u8]) -> Result<(), EmbeddedError> {
        self.door()
            .unpair(Request::new(ProfileUnpairRequest {
                operation_id: operation(),
                profile_id: self.profile_id(),
                peer: Some(PeerRef {
                    identifier: Some(peer_ref::Identifier::HostId(host_id.to_vec())),
                }),
                reason: String::new(),
            }))
            .await?;
        Ok(())
    }

    /// Binds the profile to an account with the refresh token the app's
    /// sign-in obtained; the relay link comes up from there.
    /// `client_id` is the OAuth client the token was issued to, which is
    /// the only one it refreshes under.
    pub async fn sign_in(
        &self,
        cloud_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<ProfileInfo, EmbeddedError> {
        Ok(self
            .door()
            .bind_profile(Request::new(BindProfileRequest {
                operation_id: operation(),
                profile_id: Some(self.profile_id()),
                cloud_url: cloud_url.to_owned(),
                staged_refresh_token: refresh_token.to_owned(),
                adopt_non_pristine: true,
                client_id: client_id.to_owned(),
            }))
            .await?
            .into_inner())
    }

    pub async fn sign_out(&self) -> Result<ProfileInfo, EmbeddedError> {
        Ok(self
            .door()
            .logout_profile(Request::new(ProfileOperation {
                operation_id: operation(),
                profile_id: self.profile_id(),
            }))
            .await?
            .into_inner())
    }

    /// Where reports are written.
    pub fn reports(&self) -> PathBuf {
        self.data_dir.join(node::REPORTS)
    }

    /// Stops serving and flushes the store; the installation is marked
    /// clean.
    pub async fn shutdown(self) -> io::Result<()> {
        drop(self.runtime);
        let daemon = self
            .daemon
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        match daemon {
            Some(daemon) => daemon.shutdown().await,
            None => Ok(()),
        }
    }
}

/// A key as a person compares it: hex of its SHA-256.
fn fingerprint(pubkey: &[u8]) -> String {
    Sha256::digest(pubkey)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn account_view(info: &ProfileInfo) -> AccountView {
    let binding = match info.intent() {
        Intent::Bound => AccountBinding::SignedIn,
        Intent::LoggedOut => AccountBinding::SignedOut,
        Intent::Paused => AccountBinding::Paused,
        Intent::Unbound | Intent::Unspecified => AccountBinding::Unbound,
    };
    let relay = match info.observed() {
        Observed::Unspecified | Observed::Local => RelayLink::Off,
        Observed::Connecting => RelayLink::Connecting,
        Observed::Connected => RelayLink::Connected,
        Observed::Retrying => RelayLink::Retrying,
        Observed::AuthenticationRequired => RelayLink::SignInAgain,
        Observed::VersionMismatch => RelayLink::UpdateRequired,
        Observed::StartupFailed => RelayLink::Failed,
    };
    let pro = match info.tier() {
        Tier::Pro => Some(true),
        Tier::Free => Some(false),
        Tier::Unspecified => None,
    };
    AccountView {
        binding,
        email: info.email.clone(),
        name: info.account_name.clone(),
        pro,
        relay,
    }
}

fn operation() -> String {
    uuid::Uuid::new_v4().to_string()
}
