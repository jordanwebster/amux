use serde::{Deserialize, Serialize};

use crate::HostId;
use crate::client::PairingStart;

const QR_FIELD_LEN: usize = 32;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QrPairingPayload {
    pub host_id: HostId,
    pub pubkey: Vec<u8>,
    pub cloud_url: String,
    pub one_shot_token: Vec<u8>,
}

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
    #[error(
        "QR payload cloud_url {payload_cloud_url:?} does not match configured cloud_url {configured_cloud_url:?}"
    )]
    CloudUrlMismatch {
        payload_cloud_url: String,
        configured_cloud_url: String,
    },
}

#[derive(Deserialize, Serialize)]
struct WireQrPairingPayload {
    host_id: String,
    pubkey: Vec<u8>,
    cloud_url: String,
    one_shot_token: Vec<u8>,
}

pub fn encode_qr_pairing_payload(
    pairing: &PairingStart,
    one_shot_token: &[u8],
) -> Result<String, QrPairingError> {
    let payload = WireQrPairingPayload {
        host_id: pairing.identity.host_id.to_string(),
        pubkey: pairing.identity.pubkey.clone(),
        cloud_url: pairing.cloud_url.clone(),
        one_shot_token: one_shot_token.to_vec(),
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
    validate_qr_payload_bytes("pubkey", &payload.pubkey)?;
    validate_qr_payload_bytes("one_shot_token", &payload.one_shot_token)?;
    Ok(QrPairingPayload {
        host_id,
        pubkey: payload.pubkey,
        cloud_url: payload.cloud_url,
        one_shot_token: payload.one_shot_token,
    })
}

pub fn parse_qr_pairing_payload_for_cloud(
    payload: &str,
    configured_cloud_url: &str,
) -> Result<QrPairingPayload, QrPairingError> {
    let payload = parse_qr_pairing_payload(payload)?;
    validate_qr_payload_cloud_url(&payload.cloud_url, configured_cloud_url)?;
    Ok(payload)
}

pub fn validate_qr_payload_cloud_url(
    payload_cloud_url: &str,
    configured_cloud_url: &str,
) -> Result<(), QrPairingError> {
    if payload_cloud_url == configured_cloud_url {
        Ok(())
    } else {
        Err(QrPairingError::CloudUrlMismatch {
            payload_cloud_url: payload_cloud_url.to_string(),
            configured_cloud_url: configured_cloud_url.to_string(),
        })
    }
}

fn validate_qr_payload_bytes(field: &'static str, bytes: &[u8]) -> Result<(), QrPairingError> {
    if bytes.len() == QR_FIELD_LEN {
        Ok(())
    } else {
        Err(QrPairingError::InvalidLength {
            field,
            expected: QR_FIELD_LEN,
            actual: bytes.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PairingSecret;
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
            cloud_url: "https://relay.example".to_string(),
            secret: PairingSecret::OneShotToken(vec![9; 32]),
        };

        let payload = encode_qr_pairing_payload(&pairing, &[9; 32]).unwrap();
        let parsed = parse_qr_pairing_payload_for_cloud(&payload, "https://relay.example").unwrap();

        assert_eq!(parsed.host_id, HostId::from_u128(1));
        assert_eq!(parsed.pubkey, vec![7; 32]);
        assert_eq!(parsed.cloud_url, "https://relay.example");
        assert_eq!(parsed.one_shot_token, vec![9; 32]);
    }

    #[test]
    fn qr_pairing_payload_validates_shape_and_cloud_url() {
        let payload = serde_json::json!({
            "host_id": "00000000-0000-0000-0000-000000000001",
            "pubkey": [7],
            "cloud_url": "https://relay.example",
            "one_shot_token": vec![9_u8; 32],
        })
        .to_string();

        assert!(matches!(
            parse_qr_pairing_payload(&payload),
            Err(QrPairingError::InvalidLength {
                field: "pubkey",
                ..
            })
        ));
        assert!(matches!(
            validate_qr_payload_cloud_url("https://a", "https://b"),
            Err(QrPairingError::CloudUrlMismatch { .. })
        ));
    }
}
