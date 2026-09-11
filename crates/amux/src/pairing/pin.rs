//! PIN pairing initiator helpers.

use std::net::SocketAddr;
use std::path::Path;

use super::PairingAdmin;
use crate::audit;
use crate::identity::load_or_create_device_identity_in;
use crate::pairing::ssh::SshPairingPeer;
use crate::transport::TransportError;

#[derive(Debug, thiserror::Error)]
pub enum PinPairingError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("identity store error: {0}")]
    Identity(String),
    #[error("pairing service error: {0}")]
    Pairing(String),
    #[error("client service error: {0}")]
    Client(String),
}

pub async fn pair_via_pin_direct_tcp<P, N>(
    data_dir: P,
    local_name: N,
    addr: SocketAddr,
    pin: &str,
    client: &dyn PairingAdmin,
) -> Result<SshPairingPeer, PinPairingError>
where
    P: AsRef<Path>,
    N: AsRef<str>,
{
    let _ = local_name.as_ref();
    let _local_identity =
        load_or_create_device_identity_in(data_dir.as_ref()).map_err(|error| {
            audit::pairing_start("direct_pin");
            audit::pairing_failure("direct_pin", &error);
            identity_error(error)
        })?;
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

fn identity_error(error: crate::identity::IdentityError) -> PinPairingError {
    PinPairingError::Identity(error.to_string())
}
