//! PIN pairing initiator helpers.

use std::net::SocketAddr;

use super::PairingAdmin;
use crate::audit;
use crate::pairing::ssh::SshPairingPeer;
use crate::transport::TransportError;

#[derive(Debug, thiserror::Error)]
pub enum PinPairingError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("pairing service error: {0}")]
    Pairing(String),
    #[error("client service error: {0}")]
    Client(String),
}

pub async fn pair_via_pin_direct_quic(
    addr: SocketAddr,
    pin: &str,
    client: &dyn PairingAdmin,
) -> Result<SshPairingPeer, PinPairingError> {
    let pending = client.begin_pair_pin_at(addr, pin).await.map_err(|error| {
        audit::pairing_start("direct_pin");
        audit::pairing_failure("direct_pin", &error);
        PinPairingError::Pairing(error.to_string())
    })?;
    let peer = client.confirm_pair(pending).await.map_err(|error| {
        audit::pairing_failure("direct_pin", &error);
        PinPairingError::Client(error.to_string())
    })?;
    Ok(SshPairingPeer {
        host_id: peer.host_id,
        pubkey: peer.pubkey,
        name: peer.name,
    })
}
