use prost::Message as ProstMessage;

use crate::protocol::link::Link;
use crate::protocol::message::{Host, ProtocolError};
use crate::protocol::wire;

/// Protocol version for the Connect handshake.
pub const PROTOCOL_VERSION: u32 = 3;

/// Initial connection handshake request.
///
/// `link_name` stays wire-typed as `String` so malformed names reach the
/// server, which replies with `ProtocolError::InvalidLinkName` rather than
/// dropping the connection as an invalid handshake.
#[derive(Debug, Clone)]
pub struct Connect {
    pub link_name: String,
    pub token: Option<String>,
    pub version: u32,
    pub supported_versions: Vec<u32>,
    /// Host identity for peer/cloud/server connections. Local terminal/client
    /// connections omit this because they are already on the same host.
    pub host: Option<Host>,
}

impl Connect {
    /// Encode handshake request to protobuf bytes.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, wire::EncodeError> {
        let supported_versions = if self.supported_versions.is_empty() {
            vec![self.version]
        } else {
            self.supported_versions.clone()
        };
        let request = wire::ConnectRequest {
            supported_protocol_versions: supported_versions,
            proposed_link_name: self.link_name.clone(),
            auth_token: self.token.clone(),
            host: self.host.as_ref().map(wire::host_to_wire),
        };
        Ok(request.encode_to_vec())
    }

    /// Decode handshake request from protobuf bytes.
    pub(crate) fn decode(data: &[u8]) -> Result<Self, wire::DecodeError> {
        let request = wire::ConnectRequest::decode(data)?;
        let version = if request
            .supported_protocol_versions
            .contains(&PROTOCOL_VERSION)
        {
            PROTOCOL_VERSION
        } else {
            request
                .supported_protocol_versions
                .first()
                .copied()
                .unwrap_or_default()
        };
        let host = request.host.map(wire::host_from_wire).transpose()?;

        Ok(Self {
            link_name: request.proposed_link_name,
            token: request.auth_token,
            version,
            supported_versions: request.supported_protocol_versions,
            host,
        })
    }
}

/// Initial connection handshake response.
///
/// `idle_timeout_secs` is the negotiated idle timeout. Both peers drop the
/// connection after this many seconds without inbound traffic. `None` means
/// heartbeats are disabled (used for local Unix-socket connections).
#[derive(Debug, Clone)]
pub struct ConnectResult {
    pub error: Option<ProtocolError>,
    pub idle_timeout_secs: Option<u32>,
    pub assigned_link_name: Option<String>,
    /// Host identity for the accepting endpoint. Peer/server connections use
    /// this to learn the direct counterparty; local connections and non-host
    /// relays omit it.
    pub host: Option<Host>,
}

impl ConnectResult {
    /// Encode handshake response to protobuf bytes.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, wire::EncodeError> {
        let outcome = match &self.error {
            Some(error) => {
                wire::connect_response::Outcome::Error(wire::encode_protocol_error(error))
            }
            None => {
                let assigned_link_name = self.assigned_link_name.clone().ok_or_else(|| {
                    wire::EncodeError::Invalid(
                        "accepted ConnectResponse requires assigned_link_name".to_string(),
                    )
                })?;
                Link::new(assigned_link_name.as_str()).map_err(|e| {
                    wire::EncodeError::Invalid(format!(
                        "accepted ConnectResponse assigned invalid link name: {e}"
                    ))
                })?;
                let heartbeat = self
                    .idle_timeout_secs
                    .map(|secs| {
                        if secs == 0 {
                            return Err(wire::EncodeError::Invalid(
                                "heartbeat idle timeout must be greater than zero".to_string(),
                            ));
                        }
                        let idle_timeout_ms = secs.checked_mul(1000).ok_or_else(|| {
                            wire::EncodeError::Invalid(
                                "heartbeat idle timeout overflows milliseconds".to_string(),
                            )
                        })?;
                        Ok(wire::HeartbeatConfig {
                            role: 1,
                            idle_timeout_ms,
                        })
                    })
                    .transpose()?;
                wire::connect_response::Outcome::Accepted(wire::ConnectAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name,
                    heartbeat,
                    host: self.host.as_ref().map(wire::host_to_wire),
                })
            }
        };
        Ok(wire::ConnectResponse {
            outcome: Some(outcome),
        }
        .encode_to_vec())
    }

    /// Decode handshake response from protobuf bytes.
    pub(crate) fn decode(data: &[u8]) -> Result<Self, wire::DecodeError> {
        let response = wire::ConnectResponse::decode(data)?;
        let outcome = response
            .outcome
            .ok_or_else(|| wire::DecodeError::Invalid("missing ConnectResponse outcome".into()))?;

        match outcome {
            wire::connect_response::Outcome::Accepted(accepted) => {
                if accepted.protocol_version != PROTOCOL_VERSION {
                    return Err(wire::DecodeError::Invalid(format!(
                        "accepted protocol version {} does not match local version {}",
                        accepted.protocol_version, PROTOCOL_VERSION
                    )));
                }
                Link::new(accepted.assigned_link_name.as_str()).map_err(|e| {
                    wire::DecodeError::Invalid(format!(
                        "accepted ConnectResponse assigned invalid link name: {e}"
                    ))
                })?;
                let idle_timeout_secs = accepted
                    .heartbeat
                    .map(|heartbeat| {
                        if heartbeat.role != 1 {
                            return Err(wire::DecodeError::Invalid(format!(
                                "unsupported heartbeat role {} in ConnectResponse",
                                heartbeat.role
                            )));
                        }
                        if heartbeat.idle_timeout_ms == 0 {
                            return Err(wire::DecodeError::Invalid(
                                "heartbeat idle timeout must be greater than zero".to_string(),
                            ));
                        }
                        Ok(heartbeat.idle_timeout_ms.div_ceil(1000))
                    })
                    .transpose()?;
                Ok(Self {
                    error: None,
                    idle_timeout_secs,
                    assigned_link_name: Some(accepted.assigned_link_name),
                    host: accepted.host.map(wire::host_from_wire).transpose()?,
                })
            }
            wire::connect_response::Outcome::Error(error) => Ok(Self {
                error: Some(wire::decode_protocol_error(error)),
                idle_timeout_secs: None,
                assigned_link_name: None,
                host: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::{AGENT_TYPE_CLAUDE, Capabilities, SupportedAgentType};
    use uuid::Uuid;

    fn sample_host() -> Host {
        Host {
            id: Uuid::from_u128(7),
            name: "host-a".to_string(),
            version: "0.1.29".to_string(),
            client_name: "amux-cli".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: vec![
                    SupportedAgentType {
                        agent_type: AGENT_TYPE_CLAUDE.to_string(),
                    },
                    SupportedAgentType {
                        agent_type: "third-party.example".to_string(),
                    },
                ],
            },
        }
    }

    #[test]
    fn connect_request_roundtrip_uses_protobuf() {
        let host = sample_host();
        let msg = Connect {
            link_name: "term-123".to_string(),
            token: Some("jwt".to_string()),
            version: PROTOCOL_VERSION,
            supported_versions: vec![PROTOCOL_VERSION],
            host: Some(host.clone()),
        };
        let encoded = msg.encode().unwrap();
        let decoded = Connect::decode(&encoded).unwrap();
        assert_eq!(decoded.link_name, "term-123");
        assert_eq!(decoded.token.as_deref(), Some("jwt"));
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.supported_versions, vec![PROTOCOL_VERSION]);
        assert_eq!(decoded.host, Some(host));
    }

    #[test]
    fn connect_request_rejects_non_protobuf_bytes() {
        assert!(Connect::decode(b"\x81\xa9link_name\xa3old").is_err());
    }

    #[test]
    fn connect_request_decodes_missing_host() {
        let request = wire::ConnectRequest {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            proposed_link_name: "app-client".to_string(),
            auth_token: None,
            host: None,
        };
        let encoded = request.encode_to_vec();
        let decoded = Connect::decode(&encoded).unwrap();
        assert_eq!(decoded.link_name, "app-client");
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert!(decoded.host.is_none());
    }

    #[test]
    fn connect_request_decodes_invalid_link_names() {
        // Handshake decode must accept malformed link names so the server can
        // reply with ProtocolError::InvalidLinkName instead of dropping the
        // connection as an invalid handshake.
        for bad in ["", "bad.link", "a.b.c"] {
            let msg = Connect {
                link_name: bad.to_string(),
                token: None,
                version: PROTOCOL_VERSION,
                supported_versions: vec![PROTOCOL_VERSION],
                host: None,
            };
            let encoded = msg.encode().unwrap();
            let decoded = Connect::decode(&encoded).unwrap();
            assert_eq!(decoded.link_name, bad);
        }
    }

    #[test]
    fn connect_response_roundtrip() {
        let msg = ConnectResult {
            error: None,
            idle_timeout_secs: Some(180),
            assigned_link_name: Some("accepted-link".to_string()),
            host: Some(sample_host()),
        };
        let encoded = msg.encode().unwrap();
        let decoded = ConnectResult::decode(&encoded).unwrap();
        assert!(decoded.error.is_none());
        assert_eq!(decoded.idle_timeout_secs, Some(180));
        assert_eq!(decoded.assigned_link_name.as_deref(), Some("accepted-link"));
        assert_eq!(decoded.host, msg.host);
    }

    #[test]
    fn connect_response_omitted_idle_timeout_decodes_as_none() {
        let response = wire::ConnectResponse {
            outcome: Some(wire::connect_response::Outcome::Accepted(
                wire::ConnectAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name: "accepted-link".to_string(),
                    heartbeat: None,
                    host: None,
                },
            )),
        };
        let encoded = response.encode_to_vec();
        let decoded = ConnectResult::decode(&encoded).unwrap();
        assert!(decoded.idle_timeout_secs.is_none());
    }

    #[test]
    fn connect_response_empty_assigned_link_is_rejected() {
        let response = wire::ConnectResponse {
            outcome: Some(wire::connect_response::Outcome::Accepted(
                wire::ConnectAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name: String::new(),
                    heartbeat: None,
                    host: None,
                },
            )),
        };
        let encoded = response.encode_to_vec();
        assert!(ConnectResult::decode(&encoded).is_err());
    }

    #[test]
    fn connect_response_invalid_assigned_link_is_rejected() {
        let response = wire::ConnectResponse {
            outcome: Some(wire::connect_response::Outcome::Accepted(
                wire::ConnectAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name: "bad.link".to_string(),
                    heartbeat: None,
                    host: None,
                },
            )),
        };
        let encoded = response.encode_to_vec();
        assert!(ConnectResult::decode(&encoded).is_err());
    }

    #[test]
    fn connect_response_invalid_heartbeat_is_rejected() {
        for heartbeat in [
            wire::HeartbeatConfig {
                role: 1,
                idle_timeout_ms: 0,
            },
            wire::HeartbeatConfig {
                role: 0,
                idle_timeout_ms: 180_000,
            },
            wire::HeartbeatConfig {
                role: 2,
                idle_timeout_ms: 180_000,
            },
        ] {
            let response = wire::ConnectResponse {
                outcome: Some(wire::connect_response::Outcome::Accepted(
                    wire::ConnectAccepted {
                        protocol_version: PROTOCOL_VERSION,
                        assigned_link_name: "accepted-link".to_string(),
                        heartbeat: Some(heartbeat),
                        host: None,
                    },
                )),
            };
            let encoded = response.encode_to_vec();
            assert!(ConnectResult::decode(&encoded).is_err());
        }
    }

    #[test]
    fn connect_response_encode_requires_assigned_link_and_valid_heartbeat() {
        assert!(
            ConnectResult {
                error: None,
                idle_timeout_secs: None,
                assigned_link_name: None,
                host: None,
            }
            .encode()
            .is_err()
        );
        assert!(
            ConnectResult {
                error: None,
                idle_timeout_secs: Some(0),
                assigned_link_name: Some("accepted-link".to_string()),
                host: None,
            }
            .encode()
            .is_err()
        );
        assert!(
            ConnectResult {
                error: None,
                idle_timeout_secs: Some(u32::MAX),
                assigned_link_name: Some("accepted-link".to_string()),
                host: None,
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn connect_response_error_preserves_typed_update_required_detail() {
        let msg = ConnectResult {
            error: Some(ProtocolError::UpdateRequired {
                minimum_version: "0.3.0".to_string(),
                client_version: "0.2.0".to_string(),
            }),
            idle_timeout_secs: None,
            assigned_link_name: None,
            host: None,
        };
        let encoded = msg.encode().unwrap();
        let decoded = ConnectResult::decode(&encoded).unwrap();
        assert_eq!(decoded.error, msg.error);
    }
}
