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
}

#[derive(Deserialize, Serialize)]
struct WireQrPairingPayload {
    host_id: String,
    cloud_url: String,
    secret: Vec<u8>,
}

pub fn encode_qr_pairing_payload(
    pairing: &PairingStart,
    secret: &[u8],
) -> Result<String, QrPairingError> {
    encode_qr_pairing_invitation(pairing.identity.host_id, &pairing.cloud_url, secret)
}

/// The invitation a responder's QR code carries, from its parts: the
/// responder, the cloud it names, and the one-shot secret.
pub fn encode_qr_pairing_invitation(
    host_id: HostId,
    cloud_url: &str,
    secret: &[u8],
) -> Result<String, QrPairingError> {
    let payload = WireQrPairingPayload {
        host_id: host_id.to_string(),
        cloud_url: cloud_url.to_owned(),
        secret: secret.to_vec(),
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
    Ok(QrPairingPayload {
        host_id,
        cloud_url: payload.cloud_url,
        secret: payload.secret,
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
            tcp_port: None,
            cloud_url: "https://amux.sh".to_string(),
            secret: PairingSecret::QrSecret(vec![9; 32]),
        };

        let payload = encode_qr_pairing_payload(&pairing, &[9; 32]).unwrap();
        let parsed = parse_qr_pairing_payload(&payload).unwrap();

        assert_eq!(parsed.host_id, HostId::from_u128(1));
        assert_eq!(parsed.cloud_url, "https://amux.sh");
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
            tcp_port: None,
            cloud_url: "https://amux.sh".to_string(),
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
