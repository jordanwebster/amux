//! PIN pairing initiator helpers.

use std::net::SocketAddr;
use std::path::Path;

use crate::audit;
use crate::client::{Client, ClientError};
use crate::identity::load_or_create_device_identity_in;
use crate::pairing::ssh::SshPairingPeer;
use crate::services::{LocalPairingIdentity, pair_initiator};
use crate::transport::{TransportError, pairing_channel};

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
    client: &Client,
) -> Result<SshPairingPeer, PinPairingError>
where
    P: AsRef<Path>,
    N: AsRef<str>,
{
    let local_identity = load_or_create_device_identity_in(data_dir.as_ref()).map_err(|error| {
        audit::pairing_start("direct_pin");
        audit::pairing_failure("direct_pin", &error);
        identity_error(error)
    })?;
    let local_identity = LocalPairingIdentity::from_device_identity(&local_identity);
    let channel = pairing_channel(addr).inspect_err(|error| {
        audit::pairing_start("direct_pin");
        audit::pairing_failure("direct_pin", error);
    })?;
    let mut pairing_client =
        crate::protocol::wire::pairing_service_client::PairingServiceClient::new(channel);
    let peer = pair_initiator(
        &mut pairing_client,
        &local_identity,
        local_name.as_ref(),
        pin.as_bytes(),
    )
    .await
    .map_err(|error| {
        audit::pairing_start("direct_pin");
        audit::pairing_failure("direct_pin", &error);
        PinPairingError::Pairing(error.to_string())
    })?;
    client
        .pair_direct_peer(peer.clone(), addr)
        .await
        .map_err(|error| {
            audit::pairing_failure("direct_pin", &error);
            client_error(error)
        })?;
    Ok(peer)
}

fn client_error(error: ClientError) -> PinPairingError {
    PinPairingError::Client(error.to_string())
}

fn identity_error(error: crate::identity::IdentityError) -> PinPairingError {
    PinPairingError::Identity(error.to_string())
}
