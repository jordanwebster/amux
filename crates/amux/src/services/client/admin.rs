//! In-process pairing and trust administration, available only to the installation owner.

use super::*;
use crate::client::{
    pairing_identity_from_wire, pairing_start_from_wire, peer_entry_from_wire, peer_ref,
    status_to_client_error,
};
use crate::{ClientError, PairingStart, PeerEntry, PeerIdentifier, SshPairingPeer};

/// An in-process administration handle for one profile. It cannot be obtained
/// through a profile socket or a peer tunnel.
#[derive(Clone)]
pub struct ProfileAdmin {
    service: ClientService,
    id: crate::installation::ProfileId,
}

mod method {
    pub(super) const PROFILE_START_PAIRING_NAME: &str = "/amux.v1.ProfileService/StartPairing";
    pub(super) const PROFILE_PAIR_PIN_CLOUD_PEER_NAME: &str =
        "/amux.v1.ProfileService/PairPinCloudPeer";
    pub(super) const PROFILE_PAIR_QR_CLOUD_PEER_NAME: &str =
        "/amux.v1.ProfileService/PairQrCloudPeer";
    pub(super) const PROFILE_LIST_PEERS_NAME: &str = "/amux.v1.ProfileService/ListPeers";
    pub(super) const PROFILE_GET_PEER_NAME: &str = "/amux.v1.ProfileService/GetPeer";
    pub(super) const PROFILE_UNPAIR_NAME: &str = "/amux.v1.ProfileService/Unpair";
}

impl ProfileAdmin {
    pub(crate) fn new(service: ClientService, id: crate::installation::ProfileId) -> Self {
        Self { service, id }
    }

    pub fn profile_id(&self) -> crate::installation::ProfileId {
        self.id
    }

    pub async fn list_pairing_hosts(&self) -> Result<Vec<HostEntry>, ClientError> {
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
        self.start_pairing(wire::start_pairing_request::Mode::Pin, false, None)
            .await
    }

    pub async fn start_lan_pin_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Pin, true, None)
            .await
    }

    pub async fn start_qr_pairing(&self) -> Result<PairingStart, ClientError> {
        self.start_pairing(wire::start_pairing_request::Mode::Qr, false, None)
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
            false,
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
        require_lan_direct: bool,
        demo: Option<wire::DemoPairing>,
    ) -> Result<PairingStart, ClientError> {
        let response = self
            .rpc_start_pairing(tonic::Request::new(wire::StartPairingRequest {
                mode: mode as i32,
                require_lan_direct,
                demo,
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
        let reachability = ssh_target.map(|target| {
            wire::pair_peer_request::Reachability::SshTarget(wire::SshTarget {
                target: target.target,
                profile_id: target.profile.to_string(),
            })
        });
        self.pair_peer(peer, reachability).await
    }

    pub async fn pair_direct_peer(
        &self,
        peer: SshPairingPeer,
        address: SocketAddr,
    ) -> Result<(), ClientError> {
        self.pair_peer(
            peer,
            Some(wire::pair_peer_request::Reachability::DirectTcpAddr(
                address.to_string(),
            )),
        )
        .await
    }

    pub async fn pair_pin_cloud_peer(
        &self,
        host_id: uuid::Uuid,
        pin: String,
    ) -> Result<SshPairingPeer, ClientError> {
        let response = self
            .rpc_pair_pin_cloud_peer(tonic::Request::new(wire::PairPinCloudPeerRequest {
                host_id: host_id.as_bytes().to_vec(),
                pin,
            }))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::PROFILE_PAIR_PIN_CLOUD_PEER_NAME,
            message: "missing PairPinCloudPeerResponse.peer".to_string(),
        })?;
        let (host_id, pubkey, name) =
            pairing_identity_from_wire(method::PROFILE_PAIR_PIN_CLOUD_PEER_NAME, peer)?;
        Ok(SshPairingPeer {
            host_id,
            pubkey,
            name,
        })
    }

    pub async fn pair_qr_cloud_peer(
        &self,
        host_id: uuid::Uuid,
        secret: Vec<u8>,
    ) -> Result<SshPairingPeer, ClientError> {
        let response = self
            .rpc_pair_qr_cloud_peer(tonic::Request::new(wire::PairQrCloudPeerRequest {
                host_id: host_id.as_bytes().to_vec(),
                secret,
            }))
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let peer = response.peer.ok_or_else(|| ClientError::Decode {
            method: method::PROFILE_PAIR_QR_CLOUD_PEER_NAME,
            message: "missing PairQrCloudPeerResponse.peer".to_string(),
        })?;
        let (host_id, pubkey, name) =
            pairing_identity_from_wire(method::PROFILE_PAIR_QR_CLOUD_PEER_NAME, peer)?;
        Ok(SshPairingPeer {
            host_id,
            pubkey,
            name,
        })
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

    async fn pair_peer(
        &self,
        peer: SshPairingPeer,
        reachability: Option<wire::pair_peer_request::Reachability>,
    ) -> Result<(), ClientError> {
        self.rpc_pair_peer(tonic::Request::new(wire::PairPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: peer.host_id.as_bytes().to_vec(),
                pubkey: peer.pubkey,
                name: peer.name,
            }),
            reachability,
        }))
        .await
        .map_err(status_to_client_error)?;
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
        let (name, tcp_port, cloud_url) = {
            let state = self.service.server_state.read().await;
            (
                state.config.host_name.clone(),
                state.config.tcp_port,
                state.config.cloud_url.clone(),
            )
        };
        if name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(tonic::Status::invalid_argument(
                "host_name is too long for pairing",
            ));
        }
        if request.require_lan_direct && tcp_port.is_none() {
            return Err(tonic::Status::failed_precondition(
                "set `tcp_port` in your config, or use cloud / SSH pairing",
            ));
        }
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
            let method = pairing_mode_name(mode);
            let secret =
                start_pairing_secret(&self.service.pair_mode, mode).inspect_err(|error| {
                    audit::pairing_failure(method, error);
                })?;
            (method, PAIR_MODE_TTL, secret)
        };
        audit::pairing_start(method);
        Ok(tonic::Response::new(wire::StartPairingResponse {
            identity: Some(wire::PairingIdentity {
                host_id: self.service.local_agents.host_id().as_bytes().to_vec(),
                pubkey: self.service.pairing_trust.local_pubkey.clone(),
                name,
            }),
            ttl_seconds: ttl.as_secs(),
            tcp_port: tcp_port.map(u32::from),
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

    pub(crate) async fn rpc_pair_peer(
        &self,
        request: tonic::Request<wire::PairPeerRequest>,
    ) -> TonicResult<wire::PairPeerResponse> {
        let trust = &self.service.pairing_trust;
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| tonic::Status::invalid_argument("PairPeerRequest.peer is required"))?;
        let (host_id, pubkey, name) = ssh_pairing_identity_from_wire(peer)?;
        if host_id == self.service.local_agents.host_id() || pubkey == trust.local_pubkey {
            return Err(tonic::Status::invalid_argument("SELF_PAIRING"));
        }
        let reachability = pair_peer_reachability_from_wire(request.reachability)?;
        let link_reachability = reachability.clone();
        let method = pair_peer_audit_method(&link_reachability);

        audit::pairing_start(method);
        commit_peer_trust(
            PeerTrustCommitContext::new(
                trust.trust_store.clone(),
                trust.trust_commit_lock.clone(),
                self.service.remote_agent_connections.clone(),
                trust.data_dir.clone(),
            ),
            PeerTrustUpdate::new(host_id, pubkey, name, reachability),
        )
        .await
        .inspect_err(|error| {
            audit::pairing_failure(method, error);
        })?;
        audit::pairing_success(method, host_id);
        self.service.publish_host_status_update(host_id).await;
        if let Some(reachability) = link_reachability {
            self.service
                .reachability_links
                .spawn_pair_time_link(host_id, reachability);
        }
        Ok(tonic::Response::new(wire::PairPeerResponse {}))
    }

    pub(crate) async fn rpc_pair_pin_cloud_peer(
        &self,
        request: tonic::Request<wire::PairPinCloudPeerRequest>,
    ) -> TonicResult<wire::PairPinCloudPeerResponse> {
        let request = request.into_inner();
        let peer_host_id = uuid_from_bytes("PairPinCloudPeerRequest.host_id", &request.host_id)?;
        let peer = self
            .service
            .pair_cloud_peer_with_secret(peer_host_id, request.pin.as_bytes(), "cloud_pin")
            .await?;
        Ok(tonic::Response::new(wire::PairPinCloudPeerResponse {
            peer: Some(peer),
        }))
    }

    pub(crate) async fn rpc_pair_qr_cloud_peer(
        &self,
        request: tonic::Request<wire::PairQrCloudPeerRequest>,
    ) -> TonicResult<wire::PairQrCloudPeerResponse> {
        let request = request.into_inner();
        let peer_host_id = uuid_from_bytes("PairQrCloudPeerRequest.host_id", &request.host_id)?;
        validate_pairing_qr_secret("PairQrCloudPeerRequest.secret", &request.secret)?;
        let peer = self
            .service
            .pair_cloud_peer_with_secret(peer_host_id, &request.secret, "cloud_qr")
            .await?;
        Ok(tonic::Response::new(wire::PairQrCloudPeerResponse {
            peer: Some(peer),
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
