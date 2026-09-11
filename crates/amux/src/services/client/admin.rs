//! In-process pairing and trust administration, available only to the installation owner.

use super::*;
use crate::client::{
    DeviceIdentity, PairingError, PeerVia, PendingPeer, pairing_identity_from_wire,
    pairing_start_from_wire, peer_entry_from_wire, peer_ref, public_key_fingerprint,
    status_to_client_error, status_to_pairing_error, uuid_from_wire_bytes,
};
use crate::{
    ClientError, PairingCandidate, PairingStart, PeerEntry, PeerIdentifier, SshPairingPeer,
};

/// An in-process administration handle for one profile. It cannot be obtained
/// through a profile socket or a peer tunnel.
#[derive(Clone)]
pub struct ProfileAdmin {
    service: ClientService,
    id: crate::installation::ProfileId,
}

mod method {
    pub(super) const PROFILE_START_PAIRING_NAME: &str = "/amux.v1.ProfileService/StartPairing";
    pub(super) const PROFILE_LIST_PEERS_NAME: &str = "/amux.v1.ProfileService/ListPeers";
    pub(super) const PROFILE_GET_PEER_NAME: &str = "/amux.v1.ProfileService/GetPeer";
    pub(super) const PROFILE_UNPAIR_NAME: &str = "/amux.v1.ProfileService/Unpair";
}

impl ProfileAdmin {
    pub async fn begin_pair_pin(
        &self,
        host: crate::HostId,
        pin: &str,
    ) -> Result<PendingPeer, PairingError> {
        self.begin_pair(wire::BeginPairRequest {
            host_id: host.as_bytes().to_vec(),
            secret: Some(wire::begin_pair_request::Secret::Pin(pin.to_string())),
            addrs: Vec::new(),
        })
        .await
    }

    pub async fn begin_pair_pin_at(
        &self,
        addr: SocketAddr,
        pin: &str,
    ) -> Result<PendingPeer, PairingError> {
        self.begin_pair(wire::BeginPairRequest {
            host_id: Vec::new(),
            secret: Some(wire::begin_pair_request::Secret::Pin(pin.to_string())),
            addrs: vec![addr.to_string()],
        })
        .await
    }

    pub async fn begin_pair_qr(
        &self,
        payload: &crate::QrPairingPayload,
    ) -> Result<PendingPeer, PairingError> {
        self.begin_pair(wire::BeginPairRequest {
            host_id: payload.host_id.as_bytes().to_vec(),
            secret: Some(wire::begin_pair_request::Secret::QrSecret(
                payload.secret.clone(),
            )),
            addrs: payload.addrs.iter().map(ToString::to_string).collect(),
        })
        .await
    }

    async fn begin_pair(
        &self,
        request: wire::BeginPairRequest,
    ) -> Result<PendingPeer, PairingError> {
        let response = self
            .rpc_begin_pair(tonic::Request::new(request))
            .await
            .map_err(status_to_pairing_error)?
            .into_inner();
        let peer = response.peer.ok_or(PairingError::Refused)?;
        let expires_at = chrono::DateTime::from_timestamp_millis(peer.expires_at_unix_ms)
            .ok_or(PairingError::Refused)?;
        let (host_id, pubkey, name) = pairing_identity_from_wire("BeginPair", peer)?;
        let via = peer_via_from_wire(response.via)?;
        Ok(PendingPeer {
            host_id,
            name,
            fingerprint: public_key_fingerprint(&pubkey),
            expires_at,
            via,
            token: response.token,
        })
    }

    pub async fn confirm_pair(&self, pending: PendingPeer) -> Result<PeerEntry, PairingError> {
        let response = self
            .rpc_confirm_pair(tonic::Request::new(wire::PendingPairRequest {
                token: pending.token,
            }))
            .await
            .map_err(status_to_pairing_error)?
            .into_inner();
        Ok(peer_entry_from_wire(
            "ConfirmPair",
            response.peer.ok_or(PairingError::Refused)?,
        )?)
    }

    pub async fn abandon_pair(&self, pending: PendingPeer) -> Result<(), PairingError> {
        self.rpc_abandon_pair(tonic::Request::new(wire::PendingPairRequest {
            token: pending.token,
        }))
        .await
        .map_err(status_to_pairing_error)?;
        Ok(())
    }

    pub async fn device_identity(&self) -> Result<DeviceIdentity, ClientError> {
        let identity = self
            .rpc_get_device_identity(tonic::Request::new(wire::GetDeviceIdentityRequest {}))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let method = "/amux.v1.ProfileService/GetDeviceIdentity";
        let host_id = uuid_from_wire_bytes(method, "DeviceIdentity.host_id", identity.host_id)?;
        if identity.pubkey.len() != PUBKEY_LEN {
            return Err(ClientError::Decode {
                method,
                message: "DeviceIdentity.pubkey must be 32 bytes".to_string(),
            });
        }
        Ok(DeviceIdentity {
            host_id,
            name: identity.name,
            fingerprint: public_key_fingerprint(&identity.pubkey),
        })
    }

    pub(crate) fn new(service: ClientService, id: crate::installation::ProfileId) -> Self {
        Self { service, id }
    }

    pub fn profile_id(&self) -> crate::installation::ProfileId {
        self.id
    }

    pub async fn list_pairing_hosts(&self) -> Result<Vec<PairingCandidate>, ClientError> {
        self.service
            .pairing_trust
            .trust_commit_lock
            .check()
            .map_err(|error| status_to_client_error(protocol_status(error)))?;
        Ok(self.service.list_pairing_candidates().await)
    }

    #[cfg(any(test, testnet))]
    pub(crate) fn for_test(service: ClientService) -> Self {
        let id = crate::installation::ProfileId(service.local_agents.host_id());
        Self::new(service, id)
    }
    pub async fn start_pin_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Pin, None, None)
            .await
    }

    pub async fn start_lan_pin_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Pin, None, None)
            .await
    }

    pub async fn start_pin_pairing_with_ttl(
        &self,
        ttl: Duration,
    ) -> Result<PairingStart, ClientError> {
        self.start_pairing(
            wire::start_pairing_request::Mode::Pin,
            Some(ttl.as_secs()),
            None,
        )
        .await
    }

    pub async fn start_qr_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Qr, None, None)
            .await
    }

    /// Start a reusable fixed-PIN pairing session that outlives this call.
    pub async fn start_demo_pin_pairing(
        &self,
        pin: String,
        ttl: Duration,
    ) -> Result<PairingStart, ClientError> {
        self.start_pairing(
            wire::start_pairing_request::Mode::Pin,
            None,
            Some(wire::DemoPairing {
                pin,
                ttl_seconds: ttl.as_secs(),
            }),
        )
        .await
    }

    async fn start_pairing(
        &self,
        mode: wire::start_pairing_request::Mode,
        ttl_seconds: Option<u64>,
        demo: Option<wire::DemoPairing>,
    ) -> Result<PairingStart, ClientError> {
        let response = self
            .rpc_start_pairing(tonic::Request::new(wire::StartPairingRequest {
                mode: mode as i32,
                demo,
                ttl_seconds,
            }))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        pairing_start_from_wire(method::PROFILE_START_PAIRING_NAME, response)
    }

    pub async fn cancel_pairing(&self) -> Result<(), ClientError> {
        self.rpc_cancel_pairing(tonic::Request::new(wire::CancelPairingRequest {}))
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn pairing_is_active(&self) -> Result<bool, ClientError> {
        let response = self
            .rpc_get_pairing_status(tonic::Request::new(wire::GetPairingStatusRequest {}))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(response.active)
    }

    pub async fn pair_ssh_peer(
        &self,
        peer: SshPairingPeer,
        ssh_target: Option<crate::SshTarget>,
    ) -> Result<(), ClientError> {
        let reachability = ssh_target.map(|target| Reachability::Ssh {
            target: target.target,
            profile: target.profile,
        });
        self.commit_peer(peer, reachability, "ssh").await
    }

    pub(crate) async fn rpc_trust_ssh_peer(
        &self,
        request: tonic::Request<wire::TrustSshPeerRequest>,
    ) -> TonicResult<wire::Empty> {
        let request = request.into_inner();
        let identity = request.peer.ok_or_else(|| {
            tonic::Status::invalid_argument("TrustSshPeerRequest.peer is required")
        })?;
        let (host_id, pubkey, name) = ssh_pairing_identity_from_wire(identity)?;
        let reachability = request
            .ssh_target
            .map(ssh_reachability_from_wire)
            .transpose()?;
        self.commit_peer(
            SshPairingPeer {
                host_id,
                pubkey,
                name,
            },
            reachability,
            "ssh",
        )
        .await
        .map_err(|error| tonic::Status::internal(error.to_string()))?;
        Ok(tonic::Response::new(wire::Empty {}))
    }

    pub async fn list_peers(&self) -> Result<Vec<PeerEntry>, ClientError> {
        let response = self
            .rpc_list_peers(tonic::Request::new(wire::ListPeersRequest {}))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        response
            .peers
            .into_iter()
            .map(|peer| peer_entry_from_wire(method::PROFILE_LIST_PEERS_NAME, peer))
            .collect()
    }

    pub async fn get_peer(
        &self,
        peer: impl Into<PeerIdentifier>,
    ) -> Result<PeerEntry, ClientError> {
        let response = self
            .rpc_get_peer(tonic::Request::new(wire::GetPeerRequest {
                peer: Some(peer_ref(peer.into())),
            }))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::PROFILE_GET_PEER_NAME,
            message: "missing GetPeerResponse.peer".to_string(),
        })?;
        peer_entry_from_wire(method::PROFILE_GET_PEER_NAME, peer)
    }

    pub async fn unpair(
        &self,
        peer: impl Into<PeerIdentifier>,
        reason: impl Into<String>,
    ) -> Result<PeerEntry, ClientError> {
        let response = self
            .rpc_unpair(tonic::Request::new(wire::UnpairRequest {
                peer: Some(peer_ref(peer.into())),
                reason: reason.into(),
            }))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.removed_peer.ok_or_else(|| ClientError::Decode {
            method: method::PROFILE_UNPAIR_NAME,
            message: "missing UnpairResponse.removed_peer".to_string(),
        })?;
        peer_entry_from_wire(method::PROFILE_UNPAIR_NAME, peer)
    }

    async fn commit_peer(
        &self,
        peer: SshPairingPeer,
        reachability: Option<Reachability>,
        method: &'static str,
    ) -> Result<(), ClientError> {
        let trust = &self.service.pairing_trust;
        if peer.host_id == self.service.local_agents.host_id() || peer.pubkey == trust.local_pubkey
        {
            return Err(status_to_client_error(tonic::Status::invalid_argument(
                "SELF_PAIRING",
            )));
        }
        let link_reachability = reachability.clone();
        audit::pairing_start(method);
        commit_peer_trust(
            PeerTrustCommitContext::new(
                trust.trust_store.clone(),
                trust.trust_commit_lock.clone(),
                self.service.remote_agent_connections.clone(),
                trust.data_dir.clone(),
            ),
            PeerTrustUpdate::new(peer.host_id, peer.pubkey, peer.name, reachability),
        )
        .await
        .map_err(status_to_client_error)?;
        audit::pairing_success(method, peer.host_id);
        self.service.publish_host_status_update(peer.host_id).await;
        if let Some(reachability) = link_reachability {
            self.service
                .reachability_links
                .spawn_pair_time_link(peer.host_id, reachability);
        }
        Ok(())
    }

    pub(crate) async fn rpc_start_pairing(
        &self,
        request: tonic::Request<wire::StartPairingRequest>,
    ) -> TonicResult<wire::StartPairingResponse> {
        let _operation = self.service.pairing_trust.trust_commit_lock.lock().await;
        self.service
            .pairing_trust
            .trust_commit_lock
            .check()
            .map_err(protocol_status)?;
        let request = request.into_inner();
        let mode = wire::start_pairing_request::Mode::try_from(request.mode).map_err(|_| {
            tonic::Status::invalid_argument(format!(
                "invalid StartPairingRequest mode: {}",
                request.mode
            ))
        })?;
        if request.demo.is_some() && mode != wire::start_pairing_request::Mode::Pin {
            return Err(tonic::Status::invalid_argument(
                "demo pairing requires PIN mode",
            ));
        }
        let (name, lan_port, cloud_url) = {
            let state = self.service.server_state.read().await;
            (
                state.config.host_name.clone(),
                state.config.lan.listen.then_some(state.config.lan.port),
                state
                    .credentials
                    .is_some()
                    .then(|| state.config.cloud_url.clone()),
            )
        };
        if name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(tonic::Status::invalid_argument(
                "host_name is too long for pairing",
            ));
        }
        self.service.reachability_links.requery();
        let (method, ttl, secret) = if let Some(demo) = request.demo {
            if demo.ttl_seconds == 0 || demo.ttl_seconds > DEMO_PAIR_MODE_MAX_TTL.as_secs() {
                return Err(tonic::Status::invalid_argument(format!(
                    "demo pairing ttl must be between 1 second and {} days",
                    DEMO_PAIR_MODE_MAX_TTL.as_secs() / 86_400
                )));
            }
            let ttl = std::time::Duration::from_secs(demo.ttl_seconds);
            self.service
                .pair_mode
                .start_demo_pin(demo.pin.clone(), ttl)
                .map_err(|error| match error {
                    PairModeError::InvalidPinFormat => {
                        tonic::Status::invalid_argument("PIN must be six decimal digits")
                    }
                    other => pair_mode_admin_status(other),
                })
                .inspect_err(|error| audit::pairing_failure("demo", error))?;
            tracing::warn!(
                ttl_seconds = demo.ttl_seconds,
                "demo pairing active: a reusable fixed PIN pairs any device that presents it"
            );
            (
                "demo",
                ttl,
                wire::start_pairing_response::Secret::Pin(demo.pin),
            )
        } else {
            let ttl = request
                .ttl_seconds
                .map(Duration::from_secs)
                .unwrap_or(PAIR_MODE_TTL);
            if ttl.is_zero() || ttl > DEMO_PAIR_MODE_MAX_TTL {
                return Err(tonic::Status::invalid_argument(format!(
                    "pairing ttl must be between 1 second and {} days",
                    DEMO_PAIR_MODE_MAX_TTL.as_secs() / 86_400
                )));
            }
            let method = pairing_mode_name(mode);
            let secret = start_pairing_secret_for_duration(&self.service.pair_mode, mode, ttl)
                .inspect_err(|error| audit::pairing_failure(method, error))?;
            (method, ttl, secret)
        };
        audit::pairing_start(method);
        Ok(tonic::Response::new(wire::StartPairingResponse {
            identity: Some(wire::PairingIdentity {
                expires_at_unix_ms: 0,
                host_id: self.service.local_agents.host_id().as_bytes().to_vec(),
                pubkey: self.service.pairing_trust.local_pubkey.clone(),
                name,
            }),
            ttl_seconds: ttl.as_secs(),
            addrs: lan_port
                .map(local_pairing_addrs)
                .unwrap_or_default()
                .into_iter()
                .map(|addr| addr.to_string())
                .collect(),
            cloud_url,
            secret: Some(secret),
        }))
    }

    pub(crate) async fn rpc_get_pairing_status(
        &self,
        _request: tonic::Request<wire::GetPairingStatusRequest>,
    ) -> TonicResult<wire::GetPairingStatusResponse> {
        Ok(tonic::Response::new(wire::GetPairingStatusResponse {
            active: self.service.pair_mode.is_active(),
        }))
    }

    pub(crate) async fn rpc_cancel_pairing(
        &self,
        _request: tonic::Request<wire::CancelPairingRequest>,
    ) -> TonicResult<wire::CancelPairingResponse> {
        let _operation = self.service.pairing_trust.trust_commit_lock.lock().await;
        self.service
            .pairing_trust
            .trust_commit_lock
            .check()
            .map_err(protocol_status)?;
        if self.service.pair_mode.cancel() {
            audit::pairing_cancel("admin");
        }
        Ok(tonic::Response::new(wire::CancelPairingResponse {}))
    }

    pub(crate) async fn rpc_begin_pair(
        &self,
        request: tonic::Request<wire::BeginPairRequest>,
    ) -> TonicResult<wire::PendingPairResponse> {
        self.service
            .pairing_trust
            .trust_commit_lock
            .check()
            .map_err(protocol_status)?;
        let request = request.into_inner();
        let invalid = || tonic::Status::permission_denied("INVALID_PIN");
        let requested_host = if request.host_id.is_empty() {
            None
        } else {
            Some(
                uuid_from_bytes("BeginPairRequest.host_id", &request.host_id)
                    .map_err(|_| invalid())?,
            )
        };
        if requested_host == Some(self.service.local_agents.host_id()) {
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }
        let secret = match request.secret {
            Some(wire::begin_pair_request::Secret::Pin(pin))
                if pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                pin.into_bytes()
            }
            Some(wire::begin_pair_request::Secret::QrSecret(secret))
                if secret.len() == QR_SECRET_LEN =>
            {
                secret
            }
            _ => return Err(invalid()),
        };
        let requested_addrs = request
            .addrs
            .into_iter()
            .map(|addr| addr.parse::<SocketAddr>().map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?;
        let found_addrs = requested_host
            .map(|host| self.service.reachability_links.found_addrs(host))
            .unwrap_or_default();
        let mut direct_addrs = found_addrs.clone();
        for addr in requested_addrs {
            if !direct_addrs.contains(&addr) {
                direct_addrs.push(addr);
            }
        }
        let local_name = self
            .service
            .server_state
            .read()
            .await
            .host_name()
            .to_string();
        let identity = LocalPairingIdentity::new(
            self.service.local_agents.host_id(),
            self.service.pairing_trust.local_pubkey.clone(),
        );
        let mut last_unreachable = None;
        let mut selected = None;
        for addr in direct_addrs {
            let channel = match crate::transport::pairing_channel(addr) {
                Ok(channel) => channel,
                Err(error) => {
                    last_unreachable = Some(error.to_string());
                    continue;
                }
            };
            let result = begin_pair_initiator(
                &mut wire::pairing_service_client::PairingServiceClient::new(channel),
                &identity,
                &local_name,
                &secret,
            )
            .await;
            match result {
                Ok(pending) => {
                    selected = Some((
                        pending,
                        Reachability::Direct { addrs: vec![addr] },
                        wire::PeerVia::Direct,
                        found_addrs.contains(&addr),
                    ));
                    break;
                }
                Err(error)
                    if matches!(
                        error.code(),
                        tonic::Code::Unavailable | tonic::Code::Internal
                    ) =>
                {
                    last_unreachable = Some(error.to_string());
                }
                Err(error) => return Err(opaque_pairing_status(error)),
            }
        }
        if selected.is_none()
            && let Some(host) = requested_host
            && self
                .service
                .remote_agent_connections
                .has_cloud_route(host)
                .await
        {
            let channel = self
                .service
                .remote_agent_connections
                .cloud_pairing_channel_to(host)
                .await
                .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
            let pending = begin_pair_initiator(
                &mut wire::pairing_service_client::PairingServiceClient::new(channel),
                &identity,
                &local_name,
                &secret,
            )
            .await
            .map_err(opaque_pairing_status)?;
            selected = Some((pending, Reachability::Cloud, wire::PeerVia::Relay, false));
        }
        let (pending, reachability, via, from_discovery) = selected.ok_or_else(|| {
            tonic::Status::unavailable(
                last_unreachable.unwrap_or_else(|| "pairing target is not reachable".to_string()),
            )
        })?;
        let peer_host = uuid_from_bytes("PairingIdentity.host_id", &pending.peer.host_id)
            .map_err(|_| invalid())?;
        if peer_host == self.service.local_agents.host_id()
            || pending.peer.pubkey == self.service.pairing_trust.local_pubkey
        {
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }
        if requested_host.is_some_and(|host| host != peer_host) {
            return Err(invalid());
        }
        if from_discovery
            && let Ok(store) = self.service.pairing_trust.trust_store.read()
            && let Some(pinned) = store.pubkey_for_host(peer_host)
            && pinned != pending.peer.pubkey.as_slice()
        {
            return Err(invalid());
        }
        let remaining_ms = pending
            .peer
            .expires_at_unix_ms
            .checked_sub(Utc::now().timestamp_millis())
            .filter(|remaining| *remaining > 0)
            .ok_or_else(invalid)?;
        let token = Uuid::new_v4();
        let response = wire::PendingPairResponse {
            token: token.as_bytes().to_vec(),
            peer: Some(pending.peer.clone()),
            via: via as i32,
        };
        let mut state = self.service.state.write().await;
        if state.pending_pairs.len() >= 32 {
            return Err(tonic::Status::resource_exhausted(
                "too many pending pairings",
            ));
        }
        state.pending_pairs.insert(
            token,
            PendingAdminPair {
                pairing: pending,
                reachability,
                via,
            },
        );
        drop(state);
        let state = Arc::downgrade(&self.service.state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(remaining_ms as u64)).await;
            if let Some(state) = state.upgrade() {
                state.write().await.pending_pairs.remove(&token);
            }
        });
        Ok(tonic::Response::new(response))
    }

    pub(crate) async fn rpc_confirm_pair(
        &self,
        request: tonic::Request<wire::PendingPairRequest>,
    ) -> TonicResult<wire::GetPeerResponse> {
        let pending = self.service.take_pending_pair(request.into_inner()).await?;
        let peer = tokio::time::timeout(PAIR_INITIATOR_TIMEOUT, pending.pairing.confirm())
            .await
            .map_err(|_| tonic::Status::permission_denied("INVALID_PIN"))?
            .map_err(opaque_pairing_status)?;
        let host_id = peer.host_id;
        self.commit_peer(
            peer,
            Some(pending.reachability),
            match pending.via {
                wire::PeerVia::Direct => "direct_pin",
                wire::PeerVia::Relay => "cloud",
                wire::PeerVia::Ssh => "ssh",
                wire::PeerVia::Unspecified => "pairing",
            },
        )
        .await
        .map_err(|error| tonic::Status::internal(error.to_string()))?;
        let entry = self
            .service
            .peer_entries()?
            .into_iter()
            .find(|(host, _)| *host == host_id)
            .ok_or_else(|| tonic::Status::internal("paired peer missing"))?;
        Ok(tonic::Response::new(wire::GetPeerResponse {
            peer: Some(peer_entry_to_wire(entry.0, &entry.1)),
        }))
    }

    pub(crate) async fn rpc_abandon_pair(
        &self,
        request: tonic::Request<wire::PendingPairRequest>,
    ) -> TonicResult<wire::PairingAbandoned> {
        let pending = self.service.take_pending_pair(request.into_inner()).await?;
        tokio::time::timeout(PAIR_INITIATOR_TIMEOUT, pending.pairing.abandon())
            .await
            .map_err(|_| tonic::Status::permission_denied("INVALID_PIN"))?
            .map_err(opaque_pairing_status)?;
        Ok(tonic::Response::new(wire::PairingAbandoned {}))
    }

    pub(crate) async fn rpc_get_device_identity(
        &self,
        _request: tonic::Request<wire::GetDeviceIdentityRequest>,
    ) -> TonicResult<wire::DeviceIdentity> {
        Ok(tonic::Response::new(wire::DeviceIdentity {
            host_id: self.service.local_agents.host_id().as_bytes().to_vec(),
            name: self
                .service
                .server_state
                .read()
                .await
                .config
                .host_name
                .clone(),
            pubkey: self.service.pairing_trust.local_pubkey.clone(),
        }))
    }

    pub(crate) async fn rpc_list_peers(
        &self,
        _request: tonic::Request<wire::ListPeersRequest>,
    ) -> TonicResult<wire::ListPeersResponse> {
        let peers = self
            .service
            .peer_entries()?
            .into_iter()
            .map(|(host_id, entry)| peer_entry_to_wire(host_id, &entry))
            .collect();
        Ok(tonic::Response::new(wire::ListPeersResponse { peers }))
    }

    pub(crate) async fn rpc_get_peer(
        &self,
        request: tonic::Request<wire::GetPeerRequest>,
    ) -> TonicResult<wire::GetPeerResponse> {
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("GetPeerRequest.peer is required"))?;
        let (host_id, entry) = self.service.peer_entry(peer)?;
        Ok(tonic::Response::new(wire::GetPeerResponse {
            peer: Some(peer_entry_to_wire(host_id, &entry)),
        }))
    }

    pub(crate) async fn rpc_unpair(
        &self,
        request: tonic::Request<wire::UnpairRequest>,
    ) -> TonicResult<wire::UnpairResponse> {
        audit::client_service_disruptive_call("ProfileService.Unpair", "local", None);
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("UnpairRequest.peer is required"))?;
        let (host_id, entry) = self.service.unpair_peer(peer, request.reason).await?;
        Ok(tonic::Response::new(wire::UnpairResponse {
            removed_peer: Some(peer_entry_to_wire(host_id, &entry)),
        }))
    }
}

#[cfg(not(target_os = "ios"))]
fn local_pairing_addrs(port: u16) -> Vec<SocketAddr> {
    let mut addrs = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| SocketAddr::new(interface.ip(), port))
        .filter(|addr| !addr.ip().is_unspecified())
        .collect::<Vec<_>>();
    addrs.sort_unstable();
    addrs.dedup();
    addrs
}

#[cfg(target_os = "ios")]
fn local_pairing_addrs(_port: u16) -> Vec<SocketAddr> {
    Vec::new()
}

fn peer_via_from_wire(via: i32) -> Result<PeerVia, PairingError> {
    match wire::PeerVia::try_from(via) {
        Ok(wire::PeerVia::Direct) => Ok(PeerVia::Direct),
        Ok(wire::PeerVia::Relay) => Ok(PeerVia::Relay),
        Ok(wire::PeerVia::Ssh) => Ok(PeerVia::Ssh),
        _ => Err(PairingError::Internal("missing pairing route".to_string())),
    }
}

fn ssh_pairing_identity_from_wire(
    identity: wire::PairingIdentity,
) -> Result<(crate::HostId, Vec<u8>, String), tonic::Status> {
    let host_id = uuid_from_bytes("PairingIdentity.host_id", &identity.host_id)?;
    if identity.pubkey.len() != PUBKEY_LEN {
        return Err(tonic::Status::invalid_argument(
            "PairingIdentity.pubkey must be 32 bytes",
        ));
    }
    if identity.name.len() > MAX_PAIRING_NAME_BYTES {
        return Err(tonic::Status::invalid_argument(
            "PairingIdentity.name is too long",
        ));
    }
    Ok((host_id, identity.pubkey, identity.name))
}

fn ssh_reachability_from_wire(target: wire::SshTarget) -> Result<Reachability, tonic::Status> {
    if target.target.trim().is_empty() || target.target.starts_with('-') {
        return Err(tonic::Status::invalid_argument(
            "TrustSshPeerRequest.ssh_target is invalid",
        ));
    }
    let profile = crate::installation::ProfileId(target.profile_id.parse().map_err(|_| {
        tonic::Status::invalid_argument("TrustSshPeerRequest.ssh_target.profile_id must be a UUID")
    })?);
    Ok(Reachability::Ssh {
        target: target.target,
        profile,
    })
}
