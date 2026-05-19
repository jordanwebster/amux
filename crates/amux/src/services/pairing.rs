use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity as _;
use futures_util::{Stream, stream};
use prost::Message as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::{aead, digest, hkdf, hmac};
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};
use tonic::{Code, Status};

use crate::connection::ConnectionManager;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::pairing::ssh::SshPairingPeer;
use crate::pairing::{PairMode, PairModePinAttempt, PairModeTokenAttempt};
use crate::protocol::{PROTOCOL_VERSION, wire};
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, PreTrustPairingReachability};
use crate::trust::{Reachability, SharedTrustStore, TrustStore, TrustStorePairingUpdate};
use crate::tunnel::TunnelId;
use crate::{HostId, audit};

const HOST_ID_LEN: usize = 16;
const PUBKEY_LEN: usize = 32;
const TOKEN_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;
const PAIRING_HKDF_SALT: &[u8] = b"amux-pair-spake2-v1";
const PAIR_CONFIRM_A: &[u8] = b"amux-pair-confirm-A";
const PAIR_CONFIRM_B: &[u8] = b"amux-pair-confirm-B";
const PAIR_ID_AAD: &[u8] = b"amux-pair-id";
const AEAD_A_TO_B_INFO: &[u8] = "aead/A→B".as_bytes();
const AEAD_B_TO_A_INFO: &[u8] = "aead/B→A".as_bytes();
const A_TO_B_DIRECTION: u8 = 0x01;
const B_TO_A_DIRECTION: u8 = 0x02;
pub(crate) const PAIR_BY_SPAKE2_INITIATOR_TIMEOUT: Duration = Duration::from_secs(30);
const PAIR_BY_SPAKE2_RESPONDER_TIMEOUT: Duration = Duration::from_secs(30);
const SPAKE2_ED25519_M: [u8; 32] = [
    0xd0, 0x48, 0x03, 0x2c, 0x6e, 0xa0, 0xb6, 0xd6, 0x97, 0xdd, 0xc2, 0xe8, 0x6b, 0xda, 0x85, 0xa3,
    0x3a, 0xda, 0xc9, 0x20, 0xf1, 0xbf, 0x18, 0xe1, 0xb0, 0xc6, 0xd1, 0x66, 0xa5, 0xce, 0xcd, 0xaf,
];
const SPAKE2_ED25519_N: [u8; 32] = [
    0xd3, 0xbf, 0xb5, 0x18, 0xf4, 0x4f, 0x34, 0x30, 0xf2, 0x9d, 0x0c, 0x92, 0xaf, 0x50, 0x38, 0x65,
    0xa1, 0xed, 0x32, 0x81, 0xdc, 0x69, 0xb3, 0x5d, 0xd8, 0x68, 0xba, 0x85, 0xf8, 0x86, 0xc4, 0xab,
];

type PairBySpake2Stream =
    Pin<Box<dyn Stream<Item = Result<wire::pb::PairBySpake2Message, Status>> + Send>>;
pub(crate) type SharedTrustCommitLock = Arc<Mutex<()>>;

#[derive(Clone)]
pub(crate) struct PeerTrustCommitContext {
    trust_store: SharedTrustStore,
    trust_commit_lock: SharedTrustCommitLock,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
}

impl PeerTrustCommitContext {
    pub(crate) fn new(
        trust_store: SharedTrustStore,
        trust_commit_lock: SharedTrustCommitLock,
        connections: Arc<ConnectionManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            trust_store,
            trust_commit_lock,
            connections,
            data_dir,
        }
    }
}

pub(crate) struct PeerTrustUpdate {
    host_id: HostId,
    pubkey: Vec<u8>,
    name: String,
    reachability: Option<Reachability>,
    preserve_tunnel_id: Option<TunnelId>,
}

impl PeerTrustUpdate {
    pub(crate) fn new(
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
    ) -> Self {
        Self {
            host_id,
            pubkey,
            name,
            reachability,
            preserve_tunnel_id: None,
        }
    }

    fn preserving_tunnel(mut self, tunnel_id: Option<TunnelId>) -> Self {
        self.preserve_tunnel_id = tunnel_id;
        self
    }
}

#[derive(Clone)]
pub(crate) struct LocalPairingIdentity {
    host_id: HostId,
    pubkey: Vec<u8>,
}

impl LocalPairingIdentity {
    pub(crate) fn new(host_id: HostId, pubkey: Vec<u8>) -> Self {
        Self { host_id, pubkey }
    }

    pub(crate) fn from_device_identity(identity: &DeviceIdentity) -> Self {
        Self::new(identity.host_id, identity.public_key().to_vec())
    }
}

#[derive(Clone)]
pub(crate) struct PairingService {
    pair_mode: Arc<PairMode>,
    local_identity: LocalPairingIdentity,
    host_name: String,
    trust_store: SharedTrustStore,
    trust_commit_lock: SharedTrustCommitLock,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
    spake2_responder_timeout: Duration,
}

impl PairingService {
    pub(crate) fn new(
        pair_mode: Arc<PairMode>,
        local_identity: LocalPairingIdentity,
        host_name: String,
        trust_store: SharedTrustStore,
        trust_commit_lock: SharedTrustCommitLock,
        connections: Arc<ConnectionManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            pair_mode,
            local_identity,
            host_name,
            trust_store,
            trust_commit_lock,
            connections,
            data_dir,
            spake2_responder_timeout: PAIR_BY_SPAKE2_RESPONDER_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_spake2_responder_timeout(mut self, timeout: Duration) -> Self {
        self.spake2_responder_timeout = timeout;
        self
    }

    async fn stage_peer_trust(
        &self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
        preserve_tunnel_id: Option<TunnelId>,
    ) -> Result<PeerTrustCommitGuard, Status> {
        stage_peer_trust_update(
            PeerTrustCommitContext::new(
                self.trust_store.clone(),
                self.trust_commit_lock.clone(),
                self.connections.clone(),
                self.data_dir.clone(),
            ),
            PeerTrustUpdate::new(host_id, pubkey, name, reachability)
                .preserving_tunnel(preserve_tunnel_id),
        )
        .await
    }

    async fn commit_pairing_token(
        &self,
        mut attempt: PairModeTokenAttempt,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
        preserve_tunnel_id: Option<TunnelId>,
    ) -> Result<(), Status> {
        match self
            .stage_peer_trust(host_id, pubkey, name, reachability, preserve_tunnel_id)
            .await
        {
            Ok(trust_commit) => match self.pair_mode.complete_token_success(&mut attempt) {
                Ok(()) => trust_commit.commit().await,
                Err(error) => {
                    trust_commit.rollback().await;
                    self.pair_mode.abort_token_attempt(&mut attempt);
                    Err(pair_mode_status(error))
                }
            },
            Err(error) => {
                self.pair_mode.abort_token_attempt(&mut attempt);
                Err(error)
            }
        }
    }

    async fn commit_pairing_pin(
        &self,
        attempt: &mut PairModePinAttempt,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
        preserve_tunnel_id: Option<TunnelId>,
    ) -> Result<(), Status> {
        let mut commit = self
            .pair_mode
            .begin_pin_commit(attempt)
            .map_err(pin_mode_status)?;
        match self
            .stage_peer_trust(host_id, pubkey, name, reachability, preserve_tunnel_id)
            .await
        {
            Ok(trust_commit) => match self.pair_mode.complete_pin_success(&mut commit) {
                Ok(()) => trust_commit.commit().await,
                Err(error) => {
                    trust_commit.rollback().await;
                    self.pair_mode.abort_pin_commit(&mut commit);
                    Err(pin_mode_status(error))
                }
            },
            Err(error) => {
                self.pair_mode.abort_pin_commit(&mut commit);
                Err(error)
            }
        }
    }

    async fn run_spake2_responder(
        self,
        mut pin_attempt: PairModePinAttempt,
        reachability: Option<Reachability>,
        preserve_tunnel_id: Option<TunnelId>,
        mut inbound: tonic::Streaming<wire::pb::PairBySpake2Message>,
        outbound: mpsc::Sender<Result<wire::pb::PairBySpake2Message, Status>>,
    ) -> Result<(), Status> {
        validate_name(&self.host_name)?;
        let peer_spake_msg = match read_spake2_message(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(_) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before SPAKE2 message");
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected SPAKE2 initiator message",
                )
                .await;
                return Ok(());
            }
        };

        let (spake, local_spake_msg) = Spake2ResponderState::start(pin_attempt.pin().as_bytes())?;
        send_body(
            &outbound,
            wire::pb::pair_by_spake2_message::Body::Spake2Message(local_spake_msg.clone()),
        )
        .await?;

        let shared = match spake.finish(&peer_spake_msg) {
            Ok(shared) => shared,
            Err(_) => {
                let _ = self.pair_mode.record_pin_failure(&mut pin_attempt);
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::InvalidPin,
                    "invalid PIN",
                )
                .await;
                return Ok(());
            }
        };
        let keys = derive_spake2_keys(&shared, &peer_spake_msg, &local_spake_msg)?;

        let peer_confirmation = match read_key_confirmation(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(_) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before key confirmation");
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected key confirmation",
                )
                .await;
                return Ok(());
            }
        };
        if !verify_hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
            &peer_confirmation,
        ) {
            let _ = self.pair_mode.record_pin_failure(&mut pin_attempt);
            send_pairing_error(
                &outbound,
                wire::pb::pairing_error::Reason::InvalidPin,
                "invalid PIN",
            )
            .await;
            return Ok(());
        }
        send_body(
            &outbound,
            wire::pb::pair_by_spake2_message::Body::KeyConfirmation(keys.confirm_a.clone()),
        )
        .await?;

        let local_identity = wire::pb::PairingIdentity {
            host_id: self.local_identity.host_id.as_bytes().to_vec(),
            pubkey: self.local_identity.pubkey.clone(),
            name: self.host_name.clone(),
        };
        send_body(
            &outbound,
            wire::pb::pair_by_spake2_message::Body::SealedIdentity(seal_identity(
                &keys.aead_a_to_b,
                A_TO_B_DIRECTION,
                &keys.transcript_hash,
                &local_identity,
            )?),
        )
        .await?;

        let sealed_peer_identity = match read_sealed_identity(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(_) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before sealed identity");
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected sealed identity",
                )
                .await;
                return Ok(());
            }
        };
        let peer_identity = match open_identity(
            &keys.aead_b_to_a,
            B_TO_A_DIRECTION,
            &keys.transcript_hash,
            sealed_peer_identity,
        ) {
            Ok(identity) => identity,
            Err(()) => {
                let _ = self.pair_mode.record_pin_failure(&mut pin_attempt);
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::InvalidPin,
                    "invalid PIN",
                )
                .await;
                return Ok(());
            }
        };
        let peer_host_id = match validate_pairing_identity(&peer_identity) {
            Ok(host_id) => host_id,
            Err(error) => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    error,
                )
                .await;
                return Ok(());
            }
        };
        if peer_host_id == self.local_identity.host_id
            || peer_identity.pubkey.as_slice() == self.local_identity.pubkey.as_slice()
        {
            send_pairing_error(
                &outbound,
                wire::pb::pairing_error::Reason::SelfPairing,
                "self pairing is not allowed",
            )
            .await;
            return Ok(());
        }
        self.commit_pairing_pin(
            &mut pin_attempt,
            peer_host_id,
            peer_identity.pubkey,
            peer_identity.name,
            reachability,
            preserve_tunnel_id,
        )
        .await?;
        send_body(
            &outbound,
            wire::pb::pair_by_spake2_message::Body::PairingComplete(wire::pb::PairingComplete {}),
        )
        .await?;
        audit::pairing_success("spake2", peer_host_id);
        Ok(())
    }
}

pub(crate) async fn commit_peer_trust(
    context: PeerTrustCommitContext,
    update: PeerTrustUpdate,
) -> Result<(), Status> {
    stage_peer_trust_update(context, update)
        .await?
        .commit()
        .await
}

pub(crate) async fn pair_by_token_initiator(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    expected_peer_host_id: HostId,
    expected_peer_pubkey: Vec<u8>,
    one_shot_token: Vec<u8>,
) -> Result<SshPairingPeer, Status> {
    validate_name(local_name)?;
    validate_pubkey(&expected_peer_pubkey)?;
    if one_shot_token.len() != TOKEN_LEN {
        return Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: one_shot_token must be 32 bytes",
        ));
    }
    if expected_peer_host_id == local_identity.host_id
        || expected_peer_pubkey.as_slice() == local_identity.pubkey.as_slice()
    {
        return Err(pairing_status(Code::InvalidArgument, "SELF_PAIRING"));
    }

    let response = client
        .pair_by_token(wire::pb::PairByTokenRequest {
            one_shot_token,
            host_id: local_identity.host_id.as_bytes().to_vec(),
            pubkey: local_identity.pubkey.clone(),
            name: local_name.to_string(),
        })
        .await?
        .into_inner();
    let peer_host_id = host_id_from_wire(&response.host_id)?;
    if peer_host_id != expected_peer_host_id {
        return Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: paired identity did not match QR payload",
        ));
    }
    validate_name(&response.name)?;
    Ok(SshPairingPeer {
        host_id: peer_host_id,
        pubkey: expected_peer_pubkey,
        name: response.name,
    })
}

pub(crate) async fn pair_by_spake2_initiator(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    pin: &str,
) -> Result<SshPairingPeer, Status> {
    pair_by_spake2_initiator_with_timeout(
        client,
        local_identity,
        local_name,
        pin,
        PAIR_BY_SPAKE2_INITIATOR_TIMEOUT,
    )
    .await
}

async fn pair_by_spake2_initiator_inner(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    pin: &str,
) -> Result<SshPairingPeer, Status> {
    validate_name(local_name)?;
    let (tx, rx) = mpsc::channel(8);
    let outbound = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|message| (message, rx))
    });
    let mut inbound = client
        .pair_by_spake2(tonic::Request::new(outbound))
        .await?
        .into_inner();

    let (spake, msg_b) = Spake2InitiatorState::start(pin.as_bytes())?;
    send_client_pairing_body(
        &tx,
        wire::pb::pair_by_spake2_message::Body::Spake2Message(msg_b.clone()),
    )
    .await?;

    let msg_a = match read_spake2_message(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected SPAKE2 responder message",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    let shared = spake
        .finish(&msg_a)
        .map_err(|_| pairing_status(Code::PermissionDenied, "INVALID_PIN"))?;
    let keys = derive_spake2_keys(&shared, &msg_b, &msg_a)?;
    send_client_pairing_body(
        &tx,
        wire::pb::pair_by_spake2_message::Body::KeyConfirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )),
    )
    .await?;

    let peer_confirmation = match read_key_confirmation(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected key confirmation",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    if !verify_hmac_confirm(
        &keys.kc_a,
        PAIR_CONFIRM_A,
        &keys.transcript_hash,
        &peer_confirmation,
    ) {
        return Err(pairing_status(Code::PermissionDenied, "INVALID_PIN"));
    }

    let sealed_peer_identity = match read_sealed_identity(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected sealed identity",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    let peer_identity = open_identity(
        &keys.aead_a_to_b,
        A_TO_B_DIRECTION,
        &keys.transcript_hash,
        sealed_peer_identity,
    )
    .map_err(|_| pairing_status(Code::PermissionDenied, "INVALID_PIN"))?;
    let peer_host_id = validate_pairing_identity(&peer_identity)
        .map_err(|error| pairing_status(Code::InvalidArgument, error))?;
    if peer_host_id == local_identity.host_id
        || peer_identity.pubkey.as_slice() == local_identity.pubkey.as_slice()
    {
        return Err(pairing_status(Code::InvalidArgument, "SELF_PAIRING"));
    }

    let local_pairing_identity = wire::pb::PairingIdentity {
        host_id: local_identity.host_id.as_bytes().to_vec(),
        pubkey: local_identity.pubkey.clone(),
        name: local_name.to_string(),
    };
    send_client_pairing_body(
        &tx,
        wire::pb::pair_by_spake2_message::Body::SealedIdentity(seal_identity(
            &keys.aead_b_to_a,
            B_TO_A_DIRECTION,
            &keys.transcript_hash,
            &local_pairing_identity,
        )?),
    )
    .await?;
    drop(tx);
    let completion = inbound
        .message()
        .await?
        .ok_or_else(|| Status::unavailable("pairing stream closed"))?;
    match completion.body {
        Some(wire::pb::pair_by_spake2_message::Body::PairingComplete(_)) => {}
        Some(wire::pb::pair_by_spake2_message::Body::Error(error)) => {
            return Err(peer_pairing_error_status(error));
        }
        Some(_) | None => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected pairing completion",
            ));
        }
    }

    Ok(SshPairingPeer {
        host_id: peer_host_id,
        pubkey: peer_identity.pubkey,
        name: peer_identity.name,
    })
}

async fn pair_by_spake2_initiator_with_timeout(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    pin: &str,
    timeout: Duration,
) -> Result<SshPairingPeer, Status> {
    match tokio::time::timeout(
        timeout,
        pair_by_spake2_initiator_inner(client, local_identity, local_name, pin),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(pairing_status(Code::DeadlineExceeded, "PAIRING_TIMEOUT")),
    }
}

async fn stage_peer_trust_update(
    context: PeerTrustCommitContext,
    update: PeerTrustUpdate,
) -> Result<PeerTrustCommitGuard, Status> {
    let trust_commit_lock = context.trust_commit_lock.clone().lock_owned().await;
    let host_id = update.host_id;
    let (before, mut staged, mut outcome) = {
        let store = context
            .trust_store
            .read()
            .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
        let before = store.clone();
        let mut staged = before.clone();
        let outcome = staged
            .upsert_paired_peer(
                host_id,
                update.pubkey.clone(),
                update.name.clone(),
                update.reachability.clone(),
                Utc::now(),
            )
            .map_err(identity_status)?;
        (before, staged, outcome)
    };
    let mut guard = PeerTrustCommitGuard::new(
        context.clone(),
        host_id,
        PeerTrustCommitState::new(before.clone(), staged.clone(), outcome.clone())
            .preserving_tunnel(update.preserve_tunnel_id),
        trust_commit_lock,
    );

    if outcome == TrustStorePairingUpdate::PubkeyReplacementRequired {
        {
            let mut store = context
                .trust_store
                .write()
                .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
            let _ = store
                .upsert_paired_peer(
                    host_id,
                    update.pubkey.clone(),
                    update.name.clone(),
                    update.reachability.clone(),
                    Utc::now(),
                )
                .map_err(identity_status)?;
        }
        guard = PeerTrustCommitGuard::new(
            context.clone(),
            host_id,
            PeerTrustCommitState::new(before, staged.clone(), outcome.clone())
                .finish_connection()
                .preserving_tunnel(update.preserve_tunnel_id),
            guard
                .trust_commit_lock
                .take()
                .expect("trust commit lock held"),
        );
        context
            .connections
            .teardown_host_preserving_tunnel(host_id, update.preserve_tunnel_id)
            .await;
        outcome = staged
            .replace_paired_peer_after_teardown(
                host_id,
                update.pubkey,
                update.name,
                update.reachability,
                Utc::now(),
            )
            .map_err(identity_status)?;
        guard.outcome = outcome.clone();
        guard.staged = Some(staged);
    }
    if let Err(error) = guard.save_staged() {
        guard.rollback().await;
        return Err(error);
    }
    Ok(guard)
}

struct PeerTrustCommitGuard {
    trust_store: SharedTrustStore,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
    host_id: HostId,
    before: Option<TrustStore>,
    staged: Option<TrustStore>,
    outcome: TrustStorePairingUpdate,
    finish_connection: bool,
    preserve_tunnel_id: Option<TunnelId>,
    trust_commit_lock: Option<OwnedMutexGuard<()>>,
}

struct PeerTrustCommitState {
    before: TrustStore,
    staged: TrustStore,
    outcome: TrustStorePairingUpdate,
    finish_connection: bool,
    preserve_tunnel_id: Option<TunnelId>,
}

impl PeerTrustCommitState {
    fn new(before: TrustStore, staged: TrustStore, outcome: TrustStorePairingUpdate) -> Self {
        Self {
            before,
            staged,
            outcome,
            finish_connection: false,
            preserve_tunnel_id: None,
        }
    }

    fn finish_connection(mut self) -> Self {
        self.finish_connection = true;
        self
    }

    fn preserving_tunnel(mut self, tunnel_id: Option<TunnelId>) -> Self {
        self.preserve_tunnel_id = tunnel_id;
        self
    }
}

impl PeerTrustCommitGuard {
    fn new(
        context: PeerTrustCommitContext,
        host_id: HostId,
        state: PeerTrustCommitState,
        trust_commit_lock: OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            trust_store: context.trust_store,
            connections: context.connections,
            data_dir: context.data_dir,
            host_id,
            before: Some(state.before),
            staged: Some(state.staged),
            outcome: state.outcome,
            finish_connection: state.finish_connection,
            preserve_tunnel_id: state.preserve_tunnel_id,
            trust_commit_lock: Some(trust_commit_lock),
        }
    }

    fn save_staged(&self) -> Result<(), Status> {
        let Some(staged) = self.staged.as_ref() else {
            return Ok(());
        };
        staged.save_in(&self.data_dir).map_err(identity_status)
    }

    async fn commit(mut self) -> Result<(), Status> {
        if let Some(staged) = self.staged.take() {
            let mut store = self
                .trust_store
                .write()
                .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
            *store = staged;
        }
        self.before = None;
        self.finish_replacement().await;
        audit::trust_pairing_update(self.host_id, &self.outcome);
        Ok(())
    }

    fn rollback_trust(&mut self) {
        let Some(before) = self.before.take() else {
            return;
        };
        if let Ok(mut store) = self.trust_store.write() {
            *store = before;
            let _ = store.save_in(&self.data_dir);
        }
        self.staged = None;
    }

    async fn finish_replacement(&mut self) {
        if self.finish_connection {
            self.connections
                .finish_host_replacement_preserving_tunnel(self.host_id, self.preserve_tunnel_id)
                .await;
            self.finish_connection = false;
        }
    }

    async fn rollback(mut self) {
        self.rollback_trust();
        self.finish_replacement().await;
        self.before = None;
        self.staged = None;
    }
}

impl Drop for PeerTrustCommitGuard {
    fn drop(&mut self) {
        self.rollback_trust();
        if self.finish_connection {
            let connections = self.connections.clone();
            let host_id = self.host_id;
            let preserve_tunnel_id = self.preserve_tunnel_id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    connections
                        .finish_host_replacement_preserving_tunnel(host_id, preserve_tunnel_id)
                        .await;
                });
            }
        }
    }
}

#[tonic::async_trait]
impl wire::pairing_service_server::PairingService for PairingService {
    async fn pair_by_token(
        &self,
        request: tonic::Request<wire::pb::PairByTokenRequest>,
    ) -> Result<tonic::Response<wire::pb::PairByTokenResponse>, Status> {
        audit::pairing_start("token");
        let result = async {
            let (reachability, preserve_tunnel_id) = token_pairing_request_reachability(&request)?;
            if !self.pair_mode.is_active() {
                return Err(pairing_status(
                    Code::FailedPrecondition,
                    "NOT_IN_PAIRING_MODE",
                ));
            }
            let request = request.into_inner();
            let peer_host_id = host_id_from_wire(&request.host_id)?;
            validate_pubkey(&request.pubkey)?;
            validate_name(&request.name)?;
            validate_name(&self.host_name)?;
            if peer_host_id == self.local_identity.host_id
                || request.pubkey.as_slice() == self.local_identity.pubkey.as_slice()
            {
                return Err(pairing_status(Code::InvalidArgument, "SELF_PAIRING"));
            }
            let attempt = self
                .pair_mode
                .begin_token_attempt(&request.one_shot_token)
                .map_err(pair_mode_status)?;
            self.commit_pairing_token(
                attempt,
                peer_host_id,
                request.pubkey,
                request.name,
                reachability,
                preserve_tunnel_id,
            )
            .await?;
            audit::pairing_success("token", peer_host_id);
            Ok(tonic::Response::new(wire::pb::PairByTokenResponse {
                host_id: self.local_identity.host_id.as_bytes().to_vec(),
                name: self.host_name.clone(),
            }))
        }
        .await;
        if let Err(error) = &result {
            audit::pairing_failure("token", error);
        }
        result
    }

    type PairBySpake2Stream = PairBySpake2Stream;

    async fn pair_by_spake2(
        &self,
        request: tonic::Request<tonic::Streaming<wire::pb::PairBySpake2Message>>,
    ) -> Result<tonic::Response<Self::PairBySpake2Stream>, Status> {
        audit::pairing_start("spake2");
        let (reachability, preserve_tunnel_id) = pairing_request_reachability(&request)
            .inspect_err(|error| {
                audit::pairing_failure("spake2", error);
            })?;
        let pin_attempt = self
            .pair_mode
            .begin_pin_attempt()
            .map_err(pin_mode_status)
            .inspect_err(|error| {
                audit::pairing_failure("spake2", error);
            })?;
        let (tx, rx) = mpsc::channel(8);
        let service = self.clone();
        let responder_timeout = self.spake2_responder_timeout;
        tokio::spawn(async move {
            let responder = service.run_spake2_responder(
                pin_attempt,
                reachability,
                preserve_tunnel_id,
                request.into_inner(),
                tx.clone(),
            );
            match tokio::time::timeout(responder_timeout, responder).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    audit::pairing_failure("spake2", &error);
                    let _ = tx.send(Err(error)).await;
                }
                Err(_) => {
                    audit::pairing_failure("spake2", "PAIRING_TIMEOUT");
                    let _ = tx
                        .send(Err(pairing_status(
                            Code::DeadlineExceeded,
                            "PAIRING_TIMEOUT",
                        )))
                        .await;
                }
            }
        });
        Ok(tonic::Response::new(Box::pin(stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
        ))))
    }
}

fn host_id_from_wire(bytes: &[u8]) -> Result<HostId, Status> {
    if bytes.len() != HOST_ID_LEN {
        return Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: host_id must be 16 bytes",
        ));
    }
    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(bytes);
    Ok(HostId::from_bytes(host_id))
}

fn validate_pubkey(pubkey: &[u8]) -> Result<(), Status> {
    if pubkey.len() == PUBKEY_LEN {
        Ok(())
    } else {
        Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: pubkey must be 32 bytes",
        ))
    }
}

fn validate_name(name: &str) -> Result<(), Status> {
    if name.len() <= MAX_PAIRING_NAME_BYTES {
        Ok(())
    } else {
        Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: name is too long",
        ))
    }
}

fn validate_pairing_identity(identity: &wire::pb::PairingIdentity) -> Result<HostId, &'static str> {
    if identity.host_id.len() != HOST_ID_LEN {
        return Err("host_id must be 16 bytes");
    }
    if identity.pubkey.len() != PUBKEY_LEN {
        return Err("pubkey must be 32 bytes");
    }
    if identity.name.len() > MAX_PAIRING_NAME_BYTES {
        return Err("name is too long");
    }
    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(&identity.host_id);
    Ok(HostId::from_bytes(host_id))
}

fn pairing_request_reachability<T>(
    request: &tonic::Request<T>,
) -> Result<(Option<Reachability>, Option<TunnelId>), Status> {
    request
        .extensions()
        .get::<BoxedGrpcConnectInfo>()
        .and_then(|info| match &info.auth {
            BoxedGrpcAuth::PreTrustPairing {
                reachability,
                tunnel_id,
            } => Some((
                match reachability {
                    PreTrustPairingReachability::Cloud => Some(Reachability::Cloud),
                    PreTrustPairingReachability::NoReusableReachability => None,
                },
                *tunnel_id,
            )),
            BoxedGrpcAuth::LocalTrusted | BoxedGrpcAuth::TlsTrusted { .. } => None,
        })
        .ok_or_else(|| {
            Status::permission_denied("pairing RPC requires pre-trust pairing transport")
        })
}

fn token_pairing_request_reachability<T>(
    request: &tonic::Request<T>,
) -> Result<(Option<Reachability>, Option<TunnelId>), Status> {
    let info = request
        .extensions()
        .get::<BoxedGrpcConnectInfo>()
        .ok_or_else(|| {
            Status::permission_denied("pairing RPC requires pre-trust pairing transport")
        })?;
    match &info.auth {
        BoxedGrpcAuth::PreTrustPairing {
            reachability: PreTrustPairingReachability::Cloud,
            tunnel_id,
        } => Ok((Some(Reachability::Cloud), *tunnel_id)),
        BoxedGrpcAuth::PreTrustPairing {
            reachability: PreTrustPairingReachability::NoReusableReachability,
            ..
        } => Err(Status::permission_denied(
            "token pairing requires cloud pre-trust pairing transport",
        )),
        BoxedGrpcAuth::LocalTrusted | BoxedGrpcAuth::TlsTrusted { .. } => Err(
            Status::permission_denied("pairing RPC requires pre-trust pairing transport"),
        ),
    }
}

struct Spake2ResponderState {
    x: Scalar,
    w: Scalar,
}

impl Spake2ResponderState {
    fn start(pin: &[u8]) -> Result<(Self, Vec<u8>), Status> {
        let x = random_spake2_scalar()?;
        let w = spake2_password_scalar(pin);
        let m = spake2_ed25519_point(&SPAKE2_ED25519_M)?;
        let msg_a = (ED25519_BASEPOINT_POINT * x + m * w)
            .compress()
            .to_bytes()
            .to_vec();
        Ok((Self { x, w }, msg_a))
    }

    fn finish(self, msg_b: &[u8]) -> Result<[u8; 32], ()> {
        let peer = spake2_peer_point(msg_b)?;
        let n = spake2_ed25519_point(&SPAKE2_ED25519_N).map_err(|_| ())?;
        let k = (peer - n * self.w) * self.x * Scalar::from(8_u8);
        if k.is_identity() {
            return Err(());
        }
        Ok(k.compress().to_bytes())
    }
}

struct Spake2InitiatorState {
    y: Scalar,
    w: Scalar,
}

impl Spake2InitiatorState {
    fn start(pin: &[u8]) -> Result<(Self, Vec<u8>), Status> {
        let y = random_spake2_scalar()?;
        let w = spake2_password_scalar(pin);
        let n = spake2_ed25519_point(&SPAKE2_ED25519_N)?;
        let msg_b = (ED25519_BASEPOINT_POINT * y + n * w)
            .compress()
            .to_bytes()
            .to_vec();
        Ok((Self { y, w }, msg_b))
    }

    fn finish(self, msg_a: &[u8]) -> Result<[u8; 32], ()> {
        let peer = spake2_peer_point(msg_a)?;
        let m = spake2_ed25519_point(&SPAKE2_ED25519_M).map_err(|_| ())?;
        let k = (peer - m * self.w) * self.y * Scalar::from(8_u8);
        if k.is_identity() {
            return Err(());
        }
        Ok(k.compress().to_bytes())
    }
}

fn random_spake2_scalar() -> Result<Scalar, Status> {
    let rng = SystemRandom::new();
    for _ in 0..8 {
        let mut bytes = [0_u8; 64];
        rng.fill(&mut bytes)
            .map_err(|_| Status::internal("failed to generate SPAKE2 scalar"))?;
        let scalar = Scalar::from_bytes_mod_order_wide(&bytes);
        if scalar != Scalar::ZERO {
            return Ok(scalar);
        }
    }
    Err(Status::internal("failed to generate nonzero SPAKE2 scalar"))
}

fn spake2_password_scalar(pin: &[u8]) -> Scalar {
    // The user PIN remains the RFC 9382 password input; this is the
    // ciphersuite-required hash-to-scalar step, not application stretching.
    let digest = digest::digest(&digest::SHA512, pin);
    let mut wide = [0_u8; 64];
    wide.copy_from_slice(digest.as_ref());
    Scalar::from_bytes_mod_order_wide(&wide)
}

fn spake2_ed25519_point(bytes: &[u8; 32]) -> Result<EdwardsPoint, Status> {
    spake2_peer_point(bytes).map_err(|_| Status::internal("invalid SPAKE2 constants"))
}

fn spake2_peer_point(bytes: &[u8]) -> Result<EdwardsPoint, ()> {
    if bytes.len() != 32 {
        return Err(());
    }
    let mut encoded = [0_u8; 32];
    encoded.copy_from_slice(bytes);
    let Some(point) = CompressedEdwardsY(encoded).decompress() else {
        return Err(());
    };
    if point.is_identity() || !point.is_torsion_free() {
        return Err(());
    }
    Ok(point)
}

struct PairSpake2Keys {
    transcript_hash: [u8; 32],
    kc_a: [u8; 32],
    kc_b: [u8; 32],
    confirm_a: Vec<u8>,
    aead_a_to_b: [u8; 32],
    aead_b_to_a: [u8; 32],
}

fn derive_spake2_keys(
    shared_secret: &[u8],
    msg_b: &[u8],
    msg_a: &[u8],
) -> Result<PairSpake2Keys, Status> {
    let transcript_hash = spake2_transcript_hash(msg_b, msg_a);
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, PAIRING_HKDF_SALT);
    let prk = salt.extract(shared_secret);
    let kc_a = hkdf_expand_32(&prk, b"kc/A", &transcript_hash)?;
    let kc_b = hkdf_expand_32(&prk, b"kc/B", &transcript_hash)?;
    let aead_a_to_b = hkdf_expand_32(&prk, AEAD_A_TO_B_INFO, &transcript_hash)?;
    let aead_b_to_a = hkdf_expand_32(&prk, AEAD_B_TO_A_INFO, &transcript_hash)?;
    Ok(PairSpake2Keys {
        transcript_hash,
        kc_a,
        kc_b,
        confirm_a: hmac_confirm(&kc_a, PAIR_CONFIRM_A, &transcript_hash),
        aead_a_to_b,
        aead_b_to_a,
    })
}

fn spake2_transcript_hash(msg_b: &[u8], msg_a: &[u8]) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(&PROTOCOL_VERSION.to_be_bytes());
    update_len_prefixed(&mut ctx, msg_b);
    update_len_prefixed(&mut ctx, msg_a);
    let digest = ctx.finish();
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(digest.as_ref());
    hash
}

fn update_len_prefixed(ctx: &mut digest::Context, bytes: &[u8]) {
    ctx.update(&(bytes.len() as u32).to_be_bytes());
    ctx.update(bytes);
}

struct Hkdf32;

impl hkdf::KeyType for Hkdf32 {
    fn len(&self) -> usize {
        32
    }
}

fn hkdf_expand_32(
    prk: &hkdf::Prk,
    label: &[u8],
    transcript_hash: &[u8; 32],
) -> Result<[u8; 32], Status> {
    let info = [label, transcript_hash.as_slice()];
    let okm = prk
        .expand(&info, Hkdf32)
        .map_err(|_| Status::internal("failed to expand pairing keys"))?;
    let mut out = [0_u8; 32];
    okm.fill(&mut out)
        .map_err(|_| Status::internal("failed to fill pairing keys"))?;
    Ok(out)
}

fn hmac_confirm(key: &[u8; 32], label: &[u8], transcript_hash: &[u8; 32]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(label);
    ctx.update(transcript_hash);
    ctx.sign().as_ref().to_vec()
}

fn verify_hmac_confirm(
    key: &[u8; 32],
    label: &[u8],
    transcript_hash: &[u8; 32],
    tag: &[u8],
) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut message = Vec::with_capacity(label.len() + transcript_hash.len());
    message.extend_from_slice(label);
    message.extend_from_slice(transcript_hash);
    hmac::verify(&key, &message, tag).is_ok()
}

fn seal_identity(
    key: &[u8; 32],
    direction: u8,
    transcript_hash: &[u8; 32],
    identity: &wire::pb::PairingIdentity,
) -> Result<Vec<u8>, Status> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| Status::internal("failed to create pairing AEAD key"))?;
    let key = aead::LessSafeKey::new(unbound);
    let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
    let aad = pairing_identity_aad(direction, transcript_hash);
    let mut ciphertext = identity.encode_to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
        .map_err(|_| Status::internal("failed to seal pairing identity"))?;
    Ok(ciphertext)
}

fn open_identity(
    key: &[u8; 32],
    direction: u8,
    transcript_hash: &[u8; 32],
    mut ciphertext: Vec<u8>,
) -> Result<wire::pb::PairingIdentity, ()> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| ())?;
    let key = aead::LessSafeKey::new(unbound);
    let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
    let aad = pairing_identity_aad(direction, transcript_hash);
    let plaintext = key
        .open_in_place(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
        .map_err(|_| ())?;
    wire::pb::PairingIdentity::decode(&*plaintext).map_err(|_| ())
}

fn pairing_identity_aad(direction: u8, transcript_hash: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(PAIR_ID_AAD.len() + 1 + transcript_hash.len());
    aad.extend_from_slice(PAIR_ID_AAD);
    aad.push(direction);
    aad.extend_from_slice(transcript_hash);
    aad
}

async fn read_spake2_message(
    inbound: &mut tonic::Streaming<wire::pb::PairBySpake2Message>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_by_spake2_message::Body::Spake2Message(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

async fn read_key_confirmation(
    inbound: &mut tonic::Streaming<wire::pb::PairBySpake2Message>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_by_spake2_message::Body::KeyConfirmation(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

async fn read_sealed_identity(
    inbound: &mut tonic::Streaming<wire::pb::PairBySpake2Message>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_by_spake2_message::Body::SealedIdentity(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

enum PairingRead<T> {
    Expected(T),
    PeerError(wire::pb::PairingError),
    Unexpected,
    Eof,
}

async fn read_expected_body<T>(
    inbound: &mut tonic::Streaming<wire::pb::PairBySpake2Message>,
    f: impl FnOnce(wire::pb::pair_by_spake2_message::Body) -> Option<T>,
) -> Result<PairingRead<T>, Status> {
    let Some(message) = inbound.message().await? else {
        return Ok(PairingRead::Eof);
    };
    let Some(body) = message.body else {
        return Ok(PairingRead::Unexpected);
    };
    if let wire::pb::pair_by_spake2_message::Body::Error(error) = body {
        return Ok(PairingRead::PeerError(error));
    }
    Ok(f(body).map_or(PairingRead::Unexpected, PairingRead::Expected))
}

async fn send_body(
    outbound: &mpsc::Sender<Result<wire::pb::PairBySpake2Message, Status>>,
    body: wire::pb::pair_by_spake2_message::Body,
) -> Result<(), Status> {
    outbound
        .send(Ok(wire::pb::PairBySpake2Message { body: Some(body) }))
        .await
        .map_err(|_| Status::cancelled("pairing stream closed"))
}

async fn send_client_pairing_body(
    outbound: &mpsc::Sender<wire::pb::PairBySpake2Message>,
    body: wire::pb::pair_by_spake2_message::Body,
) -> Result<(), Status> {
    outbound
        .send(wire::pb::PairBySpake2Message { body: Some(body) })
        .await
        .map_err(|_| Status::cancelled("pairing stream closed"))
}

async fn send_pairing_error(
    outbound: &mpsc::Sender<Result<wire::pb::PairBySpake2Message, Status>>,
    reason: wire::pb::pairing_error::Reason,
    detail: impl Into<String>,
) {
    let detail = detail.into();
    audit::pairing_failure("spake2", local_pairing_audit_reason(reason, &detail));
    let _ = send_body(
        outbound,
        wire::pb::pair_by_spake2_message::Body::Error(wire::pb::PairingError {
            reason: reason as i32,
            detail,
        }),
    )
    .await;
}

fn local_pairing_audit_reason(reason: wire::pb::pairing_error::Reason, detail: &str) -> String {
    match reason {
        wire::pb::pairing_error::Reason::InvalidPin => "invalid PIN".to_string(),
        wire::pb::pairing_error::Reason::InvalidToken => "invalid token".to_string(),
        wire::pb::pairing_error::Reason::SelfPairing => "self pairing is not allowed".to_string(),
        wire::pb::pairing_error::Reason::NotInPairingMode => "not in pairing mode".to_string(),
        wire::pb::pairing_error::Reason::Timeout => "pairing timed out".to_string(),
        wire::pb::pairing_error::Reason::UserRejected => "pairing rejected by peer".to_string(),
        wire::pb::pairing_error::Reason::ProtocolViolation => {
            if detail == "expected SPAKE2 initiator message"
                || detail == "expected key confirmation"
                || detail == "expected sealed identity"
                || detail == "host_id must be 16 bytes"
                || detail == "pubkey must be 32 bytes"
                || detail == "name is too long"
            {
                detail.to_string()
            } else {
                "pairing protocol violation".to_string()
            }
        }
        wire::pb::pairing_error::Reason::Unspecified => "pairing failed".to_string(),
    }
}

fn pair_mode_status(error: crate::pairing::PairModeError) -> Status {
    use crate::pairing::PairModeError;

    match error {
        PairModeError::NotActive => pairing_status(Code::FailedPrecondition, "NOT_IN_PAIRING_MODE"),
        PairModeError::InvalidToken => pairing_status(Code::PermissionDenied, "INVALID_TOKEN"),
        PairModeError::NotTokenMode => pairing_status(Code::PermissionDenied, "INVALID_TOKEN"),
        PairModeError::AlreadyActive
        | PairModeError::NotPinMode
        | PairModeError::InvalidPinFormat
        | PairModeError::SecretGeneration => pairing_status(Code::Internal, "PAIR_MODE_ERROR"),
    }
}

fn pin_mode_status(error: crate::pairing::PairModeError) -> Status {
    use crate::pairing::PairModeError;

    match error {
        PairModeError::NotActive | PairModeError::NotPinMode => {
            pairing_status(Code::FailedPrecondition, "NOT_IN_PAIRING_MODE")
        }
        PairModeError::AlreadyActive
        | PairModeError::NotTokenMode
        | PairModeError::InvalidToken
        | PairModeError::InvalidPinFormat
        | PairModeError::SecretGeneration => pairing_status(Code::Internal, "PAIR_MODE_ERROR"),
    }
}

fn identity_status(error: IdentityError) -> Status {
    match error {
        IdentityError::DuplicatePubkey(_) => pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: pubkey is already trusted",
        ),
        other => Status::internal(other.to_string()),
    }
}

fn peer_pairing_error_status(error: wire::pb::PairingError) -> Status {
    use wire::pb::pairing_error::Reason;

    let reason = Reason::try_from(error.reason).unwrap_or(Reason::Unspecified);
    let code = match reason {
        Reason::NotInPairingMode => Code::FailedPrecondition,
        Reason::InvalidPin | Reason::InvalidToken => Code::PermissionDenied,
        Reason::ProtocolViolation | Reason::SelfPairing | Reason::Unspecified => {
            Code::InvalidArgument
        }
        Reason::Timeout => Code::DeadlineExceeded,
        Reason::UserRejected => Code::Cancelled,
    };
    let label = match reason {
        Reason::Unspecified => "REASON_UNSPECIFIED",
        Reason::NotInPairingMode => "NOT_IN_PAIRING_MODE",
        Reason::InvalidPin => "INVALID_PIN",
        Reason::InvalidToken => "INVALID_TOKEN",
        Reason::ProtocolViolation => "PROTOCOL_VIOLATION",
        Reason::Timeout => "TIMEOUT",
        Reason::UserRejected => "USER_REJECTED",
        Reason::SelfPairing => "SELF_PAIRING",
    };
    if error.detail.is_empty() {
        pairing_status(code, label)
    } else {
        pairing_status(code, format!("{label}: {}", error.detail))
    }
}

fn pairing_status(code: Code, reason: impl Into<String>) -> Status {
    Status::new(code, reason.into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::protocol::wire::pairing_service_server::PairingService as _;
    use crate::routing::{
        Capabilities, Host, Link, Route, RoutingCore, RoutingEvent, SupportedAgentType,
    };
    use crate::transport::{BoxedGrpcIo, in_process_channel, in_process_transport_pair};
    use crate::trust::{TrustEntry, TrustStore};
    use crate::tunnel::TunnelPool;

    fn service_fixture() -> (
        TempDir,
        PairingService,
        Arc<PairMode>,
        SharedTrustStore,
        DeviceIdentity,
        DeviceIdentity,
    ) {
        let data_dir = tempfile::tempdir().unwrap();
        let responder = DeviceIdentity::for_test(HostId::from_u128(1));
        let peer = DeviceIdentity::for_test(HostId::from_u128(2));
        let pair_mode = Arc::new(PairMode::new());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            responder.host_id,
            routing.clone(),
            incoming_tx,
        ));
        let connections = Arc::new(ConnectionManager::new(routing, tunnels));
        let service = PairingService::new(
            pair_mode.clone(),
            LocalPairingIdentity::from_device_identity(&responder),
            "responder".to_string(),
            trust_store.clone(),
            Arc::new(Mutex::new(())),
            connections,
            data_dir.path().to_path_buf(),
        );
        (data_dir, service, pair_mode, trust_store, responder, peer)
    }

    fn token_request(
        token: [u8; 32],
        peer: &DeviceIdentity,
        name: &str,
    ) -> wire::pb::PairByTokenRequest {
        wire::pb::PairByTokenRequest {
            one_shot_token: token.to_vec(),
            host_id: peer.host_id.as_bytes().to_vec(),
            pubkey: peer.public_key().to_vec(),
            name: name.to_string(),
        }
    }

    fn pre_trust_pairing_request<T>(message: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        request.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::PreTrustPairing {
                reachability: PreTrustPairingReachability::Cloud,
                tunnel_id: None,
            },
        });
        request
    }

    async fn pairing_client_for(
        service: PairingService,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        pairing_client_for_reachability(service, PreTrustPairingReachability::Cloud).await
    }

    async fn pairing_client_for_reachability(
        service: PairingService,
        reachability: PreTrustPairingReachability,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                reachability,
                None,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    service,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    async fn local_trusted_pairing_client_for(
        service: PairingService,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::local_trusted(server_transport))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    service,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    #[derive(Clone)]
    struct ErrorOnlyPairingService {
        reason: wire::pb::pairing_error::Reason,
        detail: &'static str,
    }

    #[tonic::async_trait]
    impl wire::pairing_service_server::PairingService for ErrorOnlyPairingService {
        type PairBySpake2Stream = PairBySpake2Stream;

        async fn pair_by_token(
            &self,
            _request: tonic::Request<wire::pb::PairByTokenRequest>,
        ) -> Result<tonic::Response<wire::pb::PairByTokenResponse>, Status> {
            Err(Status::unimplemented("not used"))
        }

        async fn pair_by_spake2(
            &self,
            request: tonic::Request<tonic::Streaming<wire::pb::PairBySpake2Message>>,
        ) -> Result<tonic::Response<Self::PairBySpake2Stream>, Status> {
            let mut inbound = request.into_inner();
            tokio::spawn(
                async move { while inbound.message().await.is_ok_and(|m| m.is_some()) {} },
            );
            let message = wire::pb::PairBySpake2Message {
                body: Some(wire::pb::pair_by_spake2_message::Body::Error(
                    wire::pb::PairingError {
                        reason: self.reason as i32,
                        detail: self.detail.to_string(),
                    },
                )),
            };
            let output: PairBySpake2Stream = Box::pin(stream::once(async move { Ok(message) }));
            Ok(tonic::Response::new(output))
        }
    }

    #[derive(Clone)]
    struct HangingPairingService;

    #[tonic::async_trait]
    impl wire::pairing_service_server::PairingService for HangingPairingService {
        type PairBySpake2Stream = PairBySpake2Stream;

        async fn pair_by_token(
            &self,
            _request: tonic::Request<wire::pb::PairByTokenRequest>,
        ) -> Result<tonic::Response<wire::pb::PairByTokenResponse>, Status> {
            Err(Status::unimplemented("not used"))
        }

        async fn pair_by_spake2(
            &self,
            request: tonic::Request<tonic::Streaming<wire::pb::PairBySpake2Message>>,
        ) -> Result<tonic::Response<Self::PairBySpake2Stream>, Status> {
            let mut inbound = request.into_inner();
            tokio::spawn(
                async move { while inbound.message().await.is_ok_and(|m| m.is_some()) {} },
            );
            Ok(tonic::Response::new(Box::pin(stream::pending())))
        }
    }

    async fn hanging_pairing_client() -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                PreTrustPairingReachability::Cloud,
                None,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    HangingPairingService,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    async fn error_only_pairing_client_for(
        reason: wire::pb::pairing_error::Reason,
        detail: &'static str,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                PreTrustPairingReachability::Cloud,
                None,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    ErrorOnlyPairingService { reason, detail },
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    fn spake2_message(bytes: Vec<u8>) -> wire::pb::PairBySpake2Message {
        wire::pb::PairBySpake2Message {
            body: Some(wire::pb::pair_by_spake2_message::Body::Spake2Message(bytes)),
        }
    }

    fn key_confirmation(bytes: Vec<u8>) -> wire::pb::PairBySpake2Message {
        wire::pb::PairBySpake2Message {
            body: Some(wire::pb::pair_by_spake2_message::Body::KeyConfirmation(
                bytes,
            )),
        }
    }

    fn sealed_identity(bytes: Vec<u8>) -> wire::pb::PairBySpake2Message {
        wire::pb::PairBySpake2Message {
            body: Some(wire::pb::pair_by_spake2_message::Body::SealedIdentity(
                bytes,
            )),
        }
    }

    fn pairing_error(reason: wire::pb::pairing_error::Reason) -> wire::pb::PairBySpake2Message {
        wire::pb::PairBySpake2Message {
            body: Some(wire::pb::pair_by_spake2_message::Body::Error(
                wire::pb::PairingError {
                    reason: reason as i32,
                    detail: "peer abort".to_string(),
                },
            )),
        }
    }

    async fn next_spake2_body(
        stream: &mut tonic::Streaming<wire::pb::PairBySpake2Message>,
    ) -> wire::pb::pair_by_spake2_message::Body {
        stream
            .message()
            .await
            .unwrap()
            .expect("pairing stream ended")
            .body
            .expect("missing pairing body")
    }

    async fn start_spake2_client_stream(
        client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    ) -> (
        mpsc::Sender<wire::pb::PairBySpake2Message>,
        tonic::Streaming<wire::pb::PairBySpake2Message>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });
        let inbound = client
            .pair_by_spake2(tonic::Request::new(outbound))
            .await
            .unwrap()
            .into_inner();
        (tx, inbound)
    }

    #[test]
    fn spake2_ed25519_constants_are_valid_rfc9382_points() {
        assert!(spake2_ed25519_point(&SPAKE2_ED25519_M).is_ok());
        assert!(spake2_ed25519_point(&SPAKE2_ED25519_N).is_ok());
    }

    #[tokio::test]
    async fn pair_by_token_requires_active_pair_mode() {
        let (_dir, service, _pair_mode, _store, _responder, peer) = service_fixture();
        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                [7_u8; 32], &peer, "peer",
            )))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), "NOT_IN_PAIRING_MODE");
    }

    #[tokio::test]
    async fn pairing_service_rejects_non_pairing_transport() {
        let (_dir, service, _pair_mode, _store, _responder, peer) = service_fixture();
        let (mut client, task) = local_trusted_pairing_client_for(service).await;

        let error = client
            .pair_by_token(token_request([7_u8; 32], &peer, "peer"))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(
            error.message(),
            "pairing RPC requires pre-trust pairing transport"
        );
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_token_rejects_non_cloud_pairing_transport() {
        let (_dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        pair_mode
            .start_token_for_duration([7_u8; 32], Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for_reachability(
            service,
            PreTrustPairingReachability::NoReusableReachability,
        )
        .await;

        let error = client
            .pair_by_token(token_request([7_u8; 32], &peer, "peer"))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(
            error.message(),
            "token pairing requires cloud pre-trust pairing transport"
        );
        assert!(pair_mode.is_active());
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_spake2_requires_pin_pair_mode() {
        let (_dir, service, _pair_mode, _store, _responder, _peer) = service_fixture();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });

        let error = client
            .pair_by_spake2(tonic::Request::new(outbound))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), "NOT_IN_PAIRING_MODE");
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_spake2_success_stores_peer_and_consumes_pin() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;
        let (spake_b, msg_b) = Spake2InitiatorState::start(b"123456").unwrap();

        tx.send(spake2_message(msg_b.clone())).await.unwrap();
        let msg_a = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::Spake2Message(bytes) => bytes,
            other => panic!("unexpected SPAKE2 body: {other:?}"),
        };
        let shared = spake_b.finish(&msg_a).unwrap();
        let keys = derive_spake2_keys(&shared, &msg_b, &msg_a).unwrap();
        tx.send(key_confirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )))
        .await
        .unwrap();
        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::KeyConfirmation(bytes) => {
                assert_eq!(bytes, keys.confirm_a)
            }
            other => panic!("unexpected key-confirmation body: {other:?}"),
        }
        let sealed_responder = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::SealedIdentity(bytes) => bytes,
            other => panic!("unexpected sealed-identity body: {other:?}"),
        };
        let responder_identity = open_identity(
            &keys.aead_a_to_b,
            A_TO_B_DIRECTION,
            &keys.transcript_hash,
            sealed_responder,
        )
        .unwrap();
        assert_eq!(responder_identity.host_id, responder.host_id.as_bytes());
        assert_eq!(responder_identity.pubkey, responder.public_key());
        assert_eq!(responder_identity.name, "responder");

        let initiator_identity = wire::pb::PairingIdentity {
            host_id: peer.host_id.as_bytes().to_vec(),
            pubkey: peer.public_key().to_vec(),
            name: "peer".to_string(),
        };
        tx.send(sealed_identity(
            seal_identity(
                &keys.aead_b_to_a,
                B_TO_A_DIRECTION,
                &keys.transcript_hash,
                &initiator_identity,
            )
            .unwrap(),
        ))
        .await
        .unwrap();
        drop(tx);
        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::PairingComplete(_) => {}
            other => panic!("unexpected completion body: {other:?}"),
        }
        assert!(!pair_mode.is_active());

        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert_eq!(entry.pubkey, peer.public_key());
        assert_eq!(entry.name, "peer");
        assert_eq!(entry.reachabilities, vec![Reachability::Cloud]);
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    #[tokio::test]
    async fn spake2_initiator_helper_pairs_against_responder() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for_reachability(
            service,
            PreTrustPairingReachability::NoReusableReachability,
        )
        .await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let responder_peer =
            pair_by_spake2_initiator(&mut client, &peer_identity, "peer", "123456")
                .await
                .unwrap();

        assert_eq!(responder_peer.host_id, responder.host_id);
        assert_eq!(responder_peer.pubkey, responder.public_key());
        assert_eq!(responder_peer.name, "responder");
        assert!(!pair_mode.is_active());
        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert!(entry.reachabilities.is_empty());
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    #[tokio::test]
    async fn spake2_initiator_preserves_peer_error_reason() {
        let (_dir, _service, _pair_mode, _trust_store, _responder, peer) = service_fixture();
        let (mut client, task) = error_only_pairing_client_for(
            wire::pb::pairing_error::Reason::ProtocolViolation,
            "bad frame",
        )
        .await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let error = pair_by_spake2_initiator(&mut client, &peer_identity, "peer", "123456")
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(error.message(), "PROTOCOL_VIOLATION: bad frame");
        task.abort();
    }

    #[tokio::test]
    async fn spake2_initiator_timeout_returns_pairing_timeout() {
        let (_dir, _service, _pair_mode, _trust_store, _responder, peer) = service_fixture();
        let (mut client, task) = hanging_pairing_client().await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let error = pair_by_spake2_initiator_with_timeout(
            &mut client,
            &peer_identity,
            "peer",
            "123456",
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), Code::DeadlineExceeded);
        assert_eq!(error.message(), "PAIRING_TIMEOUT");
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_spake2_invalid_pin_records_failure_without_consuming_pin() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;
        let (spake_b, msg_b) = Spake2InitiatorState::start(b"000000").unwrap();

        tx.send(spake2_message(msg_b.clone())).await.unwrap();
        let msg_a = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::Spake2Message(bytes) => bytes,
            other => panic!("unexpected SPAKE2 body: {other:?}"),
        };
        let shared = spake_b.finish(&msg_a).unwrap();
        let keys = derive_spake2_keys(&shared, &msg_b, &msg_a).unwrap();
        tx.send(key_confirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )))
        .await
        .unwrap();

        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_by_spake2_message::Body::Error(error) => {
                assert_eq!(
                    error.reason,
                    wire::pb::pairing_error::Reason::InvalidPin as i32
                );
            }
            other => panic!("unexpected error body: {other:?}"),
        }
        assert!(pair_mode.is_active());
        drop(tx);
        task.abort();

        let token = [3_u8; 32];
        assert_eq!(
            pair_mode.start_token_for_duration(token, Duration::from_secs(60)),
            Err(crate::pairing::PairModeError::AlreadyActive)
        );
        let attempt = pair_mode.begin_pin_attempt().unwrap();
        assert_eq!(attempt.pin(), "123456");
    }

    #[tokio::test]
    async fn pair_by_spake2_peer_error_aborts_without_protocol_violation() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;

        tx.send(pairing_error(
            wire::pb::pairing_error::Reason::ProtocolViolation,
        ))
        .await
        .unwrap();
        assert!(inbound.message().await.unwrap().is_none());
        assert!(pair_mode.is_active());
        drop(tx);
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_spake2_caps_in_flight_pin_attempts() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let mut held_attempts = Vec::new();
        for _ in 0..crate::pairing::PIN_FAILED_ATTEMPT_LIMIT {
            held_attempts.push(start_spake2_client_stream(&mut client).await);
        }

        let (tx, rx) = mpsc::channel(1);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });
        let error = client
            .pair_by_spake2(tonic::Request::new(outbound))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), "NOT_IN_PAIRING_MODE");

        drop(tx);
        drop(held_attempts);
        task.abort();
    }

    #[tokio::test]
    async fn pair_by_spake2_responder_timeout_releases_idle_attempts() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        let service = service.with_spake2_responder_timeout(Duration::from_millis(10));
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let mut held_attempts = Vec::new();
        for _ in 0..crate::pairing::PIN_FAILED_ATTEMPT_LIMIT {
            held_attempts.push(start_spake2_client_stream(&mut client).await);
        }

        let (_tx, _inbound) = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let (tx, rx) = mpsc::channel(8);
                let outbound = stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|message| (message, rx))
                });
                match client.pair_by_spake2(tonic::Request::new(outbound)).await {
                    Ok(response) => return (tx, response.into_inner()),
                    Err(error) if error.code() == Code::FailedPrecondition => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(error) => panic!("unexpected pairing error: {error}"),
                }
            }
        })
        .await
        .expect("idle attempts were not released after responder timeout");

        drop(held_attempts);
        task.abort();
    }

    #[tokio::test]
    async fn trust_commit_rolls_back_when_pin_pair_mode_is_cancelled_before_completion() {
        let (dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let mut attempt = pair_mode.begin_pin_attempt().unwrap();
        let mut commit = pair_mode.begin_pin_commit(&mut attempt).unwrap();
        let guard = service
            .stage_peer_trust(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                Some(Reachability::Cloud),
                None,
            )
            .await
            .unwrap();
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());

        pair_mode.cancel();
        assert_eq!(
            pair_mode.complete_pin_success(&mut commit),
            Err(crate::pairing::PairModeError::NotActive)
        );
        guard.rollback().await;

        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert!(persisted.entry(peer.host_id).is_none());
    }

    #[tokio::test]
    async fn pair_by_token_rejects_invalid_token_without_consuming_secret() {
        let (dir, service, pair_mode, _store, responder, peer) = service_fixture();
        let token = [7_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                [8_u8; 32], &peer, "peer",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(error.message(), "INVALID_TOKEN");

        let response = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.host_id, responder.host_id.as_bytes());
        assert_eq!(response.name, "responder");
        assert!(!pair_mode.is_active());

        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(persisted.entry(peer.host_id).unwrap().name, "peer");
    }

    #[tokio::test]
    async fn pair_by_token_rejects_duplicate_pubkey_without_leaking_host_id() {
        let (_dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        trust_store.write().unwrap().insert_for_test(
            HostId::from_u128(99),
            TrustEntry {
                pubkey: peer.public_key().to_vec(),
                name: "other".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        let token = [4_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "PROTOCOL_VIOLATION: pubkey is already trusted"
        );
        assert!(!error.message().contains(&HostId::from_u128(99).to_string()));
        assert!(pair_mode.is_active());
    }

    #[tokio::test]
    async fn pair_by_token_aborts_token_reservation_when_trust_save_fails() {
        let (dir, mut service, pair_mode, trust_store, responder, peer) = service_fixture();
        let bad_data_dir = dir.path().join("not-a-directory");
        fs::write(&bad_data_dir, b"not a directory").unwrap();
        service.data_dir = bad_data_dir;
        let token = [9_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert!(pair_mode.is_active());
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());

        service.data_dir = dir.path().to_path_buf();
        let response = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.host_id, responder.host_id.as_bytes());
        assert!(!pair_mode.is_active());
    }

    #[tokio::test]
    async fn pair_by_token_replacement_save_failure_restores_trust_and_barriers() {
        let (dir, mut service, pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        let bad_data_dir = dir.path().join("not-a-directory");
        fs::write(&bad_data_dir, b"not a directory").unwrap();
        service.data_dir = bad_data_dir;
        let token = [10_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "new",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert!(pair_mode.is_active());
        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .unwrap(),
            old_peer.public_key()
        );

        let route = Route::from_link(Link::new("new-key").unwrap());
        service
            .connections
            .seed(vec![RoutingEvent::HostUp {
                host: Host {
                    id: peer.host_id,
                    name: "peer".to_string(),
                    version: "test".to_string(),
                    capabilities: Capabilities {
                        features: Vec::new(),
                        supported_agent_types: vec![SupportedAgentType {
                            agent_type: "test-agent".to_string(),
                        }],
                    },
                },
                route: route.clone(),
                origin_link: None,
            }])
            .await;
        assert_eq!(
            service.connections.known_routes(peer.host_id).await,
            vec![route]
        );
    }

    #[tokio::test]
    async fn dropped_replacement_guard_restores_trust_and_route_barrier() {
        let (_dir, service, _pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        let before = trust_store.read().unwrap().clone();
        assert_eq!(
            trust_store
                .write()
                .unwrap()
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "new".to_string(),
                    Reachability::Cloud,
                    Utc::now(),
                )
                .unwrap(),
            TrustStorePairingUpdate::PubkeyReplacementRequired
        );
        assert!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .is_none()
        );

        let guard = PeerTrustCommitGuard::new(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                service.trust_commit_lock.clone(),
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            peer.host_id,
            PeerTrustCommitState::new(
                before.clone(),
                before,
                TrustStorePairingUpdate::PubkeyReplacementRequired,
            )
            .finish_connection(),
            Arc::new(Mutex::new(())).lock_owned().await,
        );
        service.connections.teardown_host(peer.host_id).await;
        drop(guard);
        tokio::task::yield_now().await;

        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .unwrap(),
            old_peer.public_key()
        );
        let route = Route::from_link(Link::new("new-key").unwrap());
        service
            .connections
            .seed(vec![RoutingEvent::HostUp {
                host: Host {
                    id: peer.host_id,
                    name: "peer".to_string(),
                    version: "test".to_string(),
                    capabilities: Capabilities {
                        features: Vec::new(),
                        supported_agent_types: vec![SupportedAgentType {
                            agent_type: "test-agent".to_string(),
                        }],
                    },
                },
                route: route.clone(),
                origin_link: None,
            }])
            .await;
        assert_eq!(
            service.connections.known_routes(peer.host_id).await,
            vec![route]
        );
    }

    #[tokio::test]
    async fn pair_by_token_stores_peer_with_cloud_reachability_and_persists() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        let token = [3_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let response = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "phone",
            )))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(response.host_id, responder.host_id.as_bytes());
        assert_eq!(response.name, "responder");
        let live = trust_store.read().unwrap();
        let live_entry = live.entry(peer.host_id).unwrap();
        assert_eq!(live_entry.pubkey, peer.public_key());
        assert_eq!(live_entry.name, "phone");
        assert_eq!(live_entry.reachabilities, vec![Reachability::Cloud]);
        drop(live);

        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().reachabilities,
            vec![Reachability::Cloud]
        );
    }

    #[tokio::test]
    async fn trust_commits_are_serialized_and_keep_distinct_peers() {
        let (dir, service, _pair_mode, trust_store, _responder, peer_a) = service_fixture();
        let peer_b = DeviceIdentity::for_test(HostId::from_u128(3));
        let lock = service.trust_commit_lock.clone();

        let commit_a = commit_peer_trust(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                lock.clone(),
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer_a.host_id,
                peer_a.public_key().to_vec(),
                "peer-a".to_string(),
                Some(Reachability::Cloud),
            ),
        );
        let commit_b = commit_peer_trust(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                lock,
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer_b.host_id,
                peer_b.public_key().to_vec(),
                "peer-b".to_string(),
                Some(Reachability::Cloud),
            ),
        );

        let (result_a, result_b) = tokio::join!(commit_a, commit_b);
        result_a.unwrap();
        result_b.unwrap();

        let live = trust_store.read().unwrap();
        assert_eq!(live.entry(peer_a.host_id).unwrap().name, "peer-a");
        assert_eq!(live.entry(peer_b.host_id).unwrap().name, "peer-b");
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(persisted.entry(peer_a.host_id).unwrap().name, "peer-a");
        assert_eq!(persisted.entry(peer_b.host_id).unwrap().name, "peer-b");
    }

    #[tokio::test]
    async fn pair_by_token_rejects_self_pairing_before_consuming_token() {
        let (_dir, service, pair_mode, _store, responder, peer) = service_fixture();
        let token = [4_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &responder, "self",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");

        service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pair_by_token_rejects_matching_local_pubkey_before_consuming_token() {
        let (_dir, service, pair_mode, _store, responder, peer) = service_fixture();
        let token = [11_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();
        let mut request = token_request(token, &peer, "self-key");
        request.host_id = HostId::from_u128(99).as_bytes().to_vec();
        request.pubkey = responder.public_key().to_vec();

        let error = service
            .pair_by_token(pre_trust_pairing_request(request))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(error.message(), "SELF_PAIRING");

        service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pair_by_token_rejects_token_calls_while_pin_mode_is_active() {
        let (_dir, service, pair_mode, _store, _responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                [12_u8; 32],
                &peer,
                "peer",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(error.message(), "INVALID_TOKEN");
        assert!(pair_mode.is_active());
    }

    #[tokio::test]
    async fn pair_by_token_validates_responder_name_before_consuming_token() {
        let (_dir, mut service, pair_mode, _store, _responder, peer) = service_fixture();
        let token = [13_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();
        service.host_name = "x".repeat(MAX_PAIRING_NAME_BYTES + 1);

        let error = service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(pair_mode.is_active());

        service.host_name = "responder".to_string();
        service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pair_by_token_replaces_existing_pubkey_after_teardown() {
        let (dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        let token = [5_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "new",
            )))
            .await
            .unwrap();

        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert_eq!(entry.pubkey, peer.public_key());
        assert_eq!(entry.name, "new");
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
    }

    #[tokio::test]
    async fn pair_by_token_rejects_invalid_identity_shapes_before_consuming_token() {
        let (_dir, service, pair_mode, _store, _responder, peer) = service_fixture();
        let token = [6_u8; 32];
        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();
        let mut request = token_request(token, &peer, "peer");
        request.pubkey = vec![1, 2, 3];

        let error = service
            .pair_by_token(pre_trust_pairing_request(request))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);

        service
            .pair_by_token(pre_trust_pairing_request(token_request(
                token, &peer, "peer",
            )))
            .await
            .unwrap();
    }
}
