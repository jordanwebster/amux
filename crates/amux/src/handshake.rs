use crate::message::ProtocolError;
use serde::{Deserialize, Serialize};

/// Protocol version for the Connect handshake.
///
/// Reset to v1 after extracting handshake messages from the session Message enum.
pub const PROTOCOL_VERSION: u32 = 1;

/// Initial connection handshake request.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Connect {
    pub link_name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub token: Option<String>,
    pub version: u32,
}

impl Connect {
    /// Encode handshake request to MessagePack map bytes.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode handshake request from MessagePack map bytes.
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

/// Initial connection handshake response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConnectResult {
    pub error: Option<ProtocolError>,
}

impl ConnectResult {
    /// Encode handshake response to MessagePack map bytes.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode handshake response from MessagePack map bytes.
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_roundtrip() {
        let msg = Connect {
            link_name: "term-123".to_string(),
            token: Some("jwt".to_string()),
            version: PROTOCOL_VERSION,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Connect::decode(&encoded).unwrap();
        assert_eq!(decoded.link_name, "term-123");
        assert_eq!(decoded.token.as_deref(), Some("jwt"));
        assert_eq!(decoded.version, PROTOCOL_VERSION);
    }

    #[test]
    fn connect_requires_version_field() {
        #[derive(Serialize)]
        struct OldConnect {
            link_name: String,
            token: Option<String>,
        }

        let old = OldConnect {
            link_name: "old-client".to_string(),
            token: None,
        };
        let encoded = rmp_serde::to_vec_named(&old).unwrap();
        assert!(Connect::decode(&encoded).is_err());
    }

    #[test]
    fn connect_result_roundtrip() {
        let msg = ConnectResult { error: None };
        let encoded = msg.encode().unwrap();
        let decoded = ConnectResult::decode(&encoded).unwrap();
        assert!(decoded.error.is_none());
    }
}
