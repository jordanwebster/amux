use std::net::SocketAddr;

use client::PairingStart;
pub use model::QrPairingPayload;
use serde::{Deserialize, Serialize};

use crate::HostId;

const QR_SECRET_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum QrPairingError {
    #[error("failed to decode QR pairing payload: {0}")]
    Json(#[from] serde_json::Error),
    #[error("QR pairing host_id {value} is invalid: {source}")]
    InvalidHostId { value: String, source: uuid::Error },
    #[error("QR pairing {field} must be {expected} bytes, got {actual}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("QR pairing address {value:?} is invalid: {source}")]
    InvalidAddress {
        value: String,
        source: std::net::AddrParseError,
    },
}

#[derive(Deserialize, Serialize)]
struct WireQrPairingPayload {
    host_id: String,
    secret: Vec<u8>,
    addrs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_url: Option<String>,
}

pub fn encode_qr_pairing_payload(
    pairing: &PairingStart,
    secret: &[u8],
) -> Result<String, QrPairingError> {
    encode_qr_pairing_invitation(
        pairing.identity.host_id,
        &pairing.addrs,
        pairing.cloud_url.as_deref(),
        secret,
    )
}

/// The invitation a responder's QR code carries, from its parts: the
/// responder, the addresses it can be dialled at directly, the cloud it names
/// if it has one, and the one-shot secret. A responder with no account still
/// issues an invitation: the addresses alone are enough on a shared network.
pub fn encode_qr_pairing_invitation(
    host_id: HostId,
    addrs: &[SocketAddr],
    cloud_url: Option<&str>,
    secret: &[u8],
) -> Result<String, QrPairingError> {
    let payload = WireQrPairingPayload {
        host_id: host_id.to_string(),
        secret: secret.to_vec(),
        addrs: addrs.iter().map(ToString::to_string).collect(),
        cloud_url: cloud_url.map(ToOwned::to_owned),
    };
    Ok(serde_json::to_string(&payload)?)
}

pub fn parse_qr_pairing_payload(payload: &str) -> Result<QrPairingPayload, QrPairingError> {
    let payload: WireQrPairingPayload = serde_json::from_str(payload)?;
    let host_id =
        HostId::parse_str(&payload.host_id).map_err(|source| QrPairingError::InvalidHostId {
            value: payload.host_id.clone(),
            source,
        })?;
    validate_qr_payload_bytes("secret", &payload.secret)?;
    let addrs = payload
        .addrs
        .into_iter()
        .map(|value| {
            value
                .parse()
                .map_err(|source| QrPairingError::InvalidAddress { value, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(QrPairingPayload {
        host_id,
        secret: payload.secret,
        addrs,
        cloud_url: payload.cloud_url,
    })
}

fn validate_qr_payload_bytes(field: &'static str, bytes: &[u8]) -> Result<(), QrPairingError> {
    if bytes.len() == QR_SECRET_LEN {
        Ok(())
    } else {
        Err(QrPairingError::InvalidLength {
            field,
            expected: QR_SECRET_LEN,
            actual: bytes.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use client::PairingSecret;

    use super::*;
    use crate::pairing::ssh::SshPairingPeer;

    #[test]
    fn qr_pairing_payload_round_trips() {
        let pairing = PairingStart {
            identity: SshPairingPeer {
                host_id: HostId::from_u128(1),
                pubkey: vec![7; 32],
                name: "desktop".to_string(),
            },
            ttl_seconds: 300,
            addrs: vec!["192.0.2.4:9001".parse().unwrap()],
            cloud_url: Some("https://relay.example".to_string()),
            secret: PairingSecret::QrSecret(vec![9; 32]),
        };

        let payload = encode_qr_pairing_payload(&pairing, &[9; 32]).unwrap();
        let parsed = parse_qr_pairing_payload(&payload).unwrap();

        assert_eq!(parsed.host_id, HostId::from_u128(1));
        assert_eq!(parsed.cloud_url.as_deref(), Some("https://relay.example"));
        assert_eq!(parsed.addrs, vec!["192.0.2.4:9001".parse().unwrap()]);
        assert_eq!(parsed.secret, vec![9; 32]);
    }

    #[test]
    fn qr_pairing_payload_carries_no_pubkey() {
        let pairing = PairingStart {
            identity: SshPairingPeer {
                host_id: HostId::from_u128(1),
                pubkey: vec![7; 32],
                name: "desktop".to_string(),
            },
            ttl_seconds: 300,
            addrs: Vec::new(),
            cloud_url: None,
            secret: PairingSecret::QrSecret(vec![9; 32]),
        };

        let payload = encode_qr_pairing_payload(&pairing, &[9; 32]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(value.get("pubkey").is_none());
        assert!(value.get("name").is_none());
    }

    #[test]
    fn qr_pairing_payload_validates_secret_length() {
        let payload = serde_json::json!({
            "host_id": "00000000-0000-0000-0000-000000000001",
            "cloud_url": "https://amux.sh",
            "secret": [9],
            "addrs": [],
        })
        .to_string();

        assert!(matches!(
            parse_qr_pairing_payload(&payload),
            Err(QrPairingError::InvalidLength {
                field: "secret",
                ..
            })
        ));
    }
}
