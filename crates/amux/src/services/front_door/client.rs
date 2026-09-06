//! Profile administration over the installation's front door.

use std::net::SocketAddr;

use chrono::DateTime;

use crate::client::{
    DeviceIdentity, PairingError, PendingPeer, public_key_fingerprint, status_to_pairing_error,
    uuid_from_wire_bytes,
};
const PAIRING_PUBKEY_LEN: usize = 32;
use std::time::Duration;

use tonic::transport::Channel;
use uuid::Uuid;

use crate::client::{
    debug_format_to_wire, host_entry_from_wire, pairing_identity_from_wire,
    pairing_start_from_wire, peer_entry_from_wire, peer_ref, status_to_client_error,
};
use crate::installation::ProfileId;
use crate::protocol::wire;
use crate::{
    ClientError, DebugFormat, HostEntry, PairingStart, PeerEntry, PeerIdentifier, SshPairingPeer,
};

/// A local administration connection pinned to one immutable profile UUID.
/// It never connects to the profile's ClientService socket.
#[derive(Clone)]
pub struct ProfileAdminClient {
    id: ProfileId,
    inner: wire::profile_service_client::ProfileServiceClient<Channel>,
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

impl ProfileAdminClient {
    /// Authenticate a PIN through the relay without writing either trust store.
    pub async fn begin_pair_pin(
        &self,
        host: crate::HostId,
        pin: &str,
    ) -> Result<PendingPeer, PairingError> {
        self.begin_pair(wire::BeginPairRequest {
            host_id: host.as_bytes().to_vec(),
            secret: Some(wire::begin_pair_request::Secret::Pin(pin.to_string())),
            cloud_url: None,
        })
        .await
    }

    /// Authenticate the scanned secret and return the sealed host identity for review.
    pub async fn begin_pair_qr(
        &self,
        payload: &crate::QrPairingPayload,
    ) -> Result<PendingPeer, PairingError> {
        self.begin_pair(wire::BeginPairRequest {
            host_id: payload.host_id.as_bytes().to_vec(),
            secret: Some(wire::begin_pair_request::Secret::QrSecret(
                payload.secret.clone(),
            )),
            cloud_url: Some(payload.cloud_url.clone()),
        })
        .await
    }

    async fn begin_pair(
        &self,
        request: wire::BeginPairRequest,
    ) -> Result<PendingPeer, PairingError> {
        let response = self
            .inner
            .clone()
            .begin_pair(wire::ProfileBeginPairRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(request),
            })
            .await
            .map_err(status_to_pairing_error)?
            .into_inner();
        let peer = response.peer.ok_or(PairingError::InvalidPin)?;
        let expires_at = DateTime::from_timestamp_millis(peer.expires_at_unix_ms)
            .ok_or(PairingError::InvalidPin)?;
        let (host_id, pubkey, name) = pairing_identity_from_wire("BeginPair", peer)?;
        let fingerprint = public_key_fingerprint(&pubkey);
        Ok(PendingPeer {
            host_id,
            name,
            fingerprint,
            expires_at,
            token: response.token,
        })
    }

    /// Grant mutual trust to the authenticated peer represented by this attempt.
    pub async fn confirm_pair(&self, pending: PendingPeer) -> Result<PeerEntry, PairingError> {
        let response = self
            .inner
            .clone()
            .confirm_pair(wire::ProfilePendingPairRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::PendingPairRequest {
                    token: pending.token,
                }),
            })
            .await
            .map_err(status_to_pairing_error)?
            .into_inner();
        Ok(peer_entry_from_wire(
            "ConfirmPair",
            response.peer.ok_or(PairingError::InvalidPin)?,
        )?)
    }

    /// Cancel without trust writes, returning only after the responder acknowledges.
    pub async fn abandon_pair(&self, pending: PendingPeer) -> Result<(), PairingError> {
        self.inner
            .clone()
            .abandon_pair(wire::ProfilePendingPairRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::PendingPairRequest {
                    token: pending.token,
                }),
            })
            .await
            .map_err(status_to_pairing_error)?;
        Ok(())
    }

    /// Reads the identity of the local daemon or embedded client runtime.
    pub async fn device_identity(&self) -> Result<DeviceIdentity, ClientError> {
        let identity = self
            .inner
            .clone()
            .get_device_identity(wire::ProfileRequest {
                profile_id: self.id.to_string(),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        let method = "/amux.v1.ProfileService/GetDeviceIdentity";
        let host_id = uuid_from_wire_bytes(method, "DeviceIdentity.host_id", identity.host_id)?;
        if identity.pubkey.len() != PAIRING_PUBKEY_LEN {
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

    pub(super) fn new(
        id: ProfileId,
        inner: wire::profile_service_client::ProfileServiceClient<Channel>,
    ) -> Self {
        Self { id, inner }
    }

    pub fn profile_id(&self) -> ProfileId {
        self.id
    }

    pub async fn list_pairing_hosts(&self) -> Result<Vec<HostEntry>, ClientError> {
        let response = self
            .inner
            .clone()
            .list_pairing_candidates(wire::ProfileRequest {
                profile_id: self.id.to_string(),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        response
            .hosts
            .into_iter()
            .map(|host| host_entry_from_wire("/amux.v1.ProfileService/ListPairingCandidates", host))
            .collect()
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
            .inner
            .clone()
            .start_pairing(wire::ProfileStartPairingRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::StartPairingRequest {
                    mode: mode as i32,
                    require_lan_direct,
                    demo,
                }),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        pairing_start_from_wire(method::PROFILE_START_PAIRING_NAME, response)
    }

    pub async fn cancel_pairing(&self) -> Result<(), ClientError> {
        self.inner
            .clone()
            .cancel_pairing(wire::ProfileOperation {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn pairing_is_active(&self) -> Result<bool, ClientError> {
        let response = self
            .inner
            .clone()
            .get_pairing_status(wire::ProfilePairingStatusRequest {
                profile_id: self.id.to_string(),
            })
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
            .inner
            .clone()
            .pair_pin_cloud_peer(wire::ProfilePairPinCloudPeerRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::PairPinCloudPeerRequest {
                    host_id: host_id.as_bytes().to_vec(),
                    pin,
                }),
            })
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
            .inner
            .clone()
            .pair_qr_cloud_peer(wire::ProfilePairQrCloudPeerRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::PairQrCloudPeerRequest {
                    host_id: host_id.as_bytes().to_vec(),
                    secret,
                }),
            })
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
            .inner
            .clone()
            .list_peers(wire::ProfileRequest {
                profile_id: self.id.to_string(),
            })
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
            .inner
            .clone()
            .get_peer(wire::ProfileGetPeerRequest {
                profile_id: self.id.to_string(),
                peer: Some(peer_ref(peer.into())),
            })
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
            .inner
            .clone()
            .unpair(wire::ProfileUnpairRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                peer: Some(peer_ref(peer.into())),
                reason: reason.into(),
            })
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
        self.inner
            .clone()
            .pair_peer(wire::ProfilePairPeerRequest {
                operation_id: Uuid::new_v4().to_string(),
                profile_id: self.id.to_string(),
                pairing: Some(wire::PairPeerRequest {
                    peer: Some(wire::PairingIdentity {
                        expires_at_unix_ms: 0,
                        host_id: peer.host_id.as_bytes().to_vec(),
                        pubkey: peer.pubkey,
                        name: peer.name,
                    }),
                    reachability,
                }),
            })
            .await
            .map_err(status_to_client_error)?;
        Ok(())
    }

    pub async fn debug_dump(&self, format: DebugFormat) -> Result<String, ClientError> {
        self.debug_dump_verbose(false, format).await
    }

    pub async fn debug_dump_verbose(
        &self,
        verbose: bool,
        format: DebugFormat,
    ) -> Result<String, ClientError> {
        let response = self
            .inner
            .clone()
            .debug_profile(wire::ProfileDebugRequest {
                profile_id: self.id.to_string(),
                debug: Some(wire::DebugRequest {
                    verbose,
                    format: debug_format_to_wire(format),
                }),
            })
            .await
            .map_err(status_to_client_error)?
            .into_inner();
        Ok(response.dump)
    }
}
