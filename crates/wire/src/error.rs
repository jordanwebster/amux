use bytes::Bytes;
use model::{Protocol, ProtocolError};
use prost::Message as ProstMessage;
use uuid::Uuid;

use super::amux::v1::{
    AgentProtocol, AmbiguousAgentName, ArtifactCorrupt, AttachmentMissing, AttachmentTooLarge,
    DiffUnavailable, Error, ErrorCode, ErrorDetail, ProtocolNotExposed, ProtocolVersionMismatch,
    SequenceNumberMismatch, UpdateRequired,
};

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("protobuf encode error: {0}")]
    Protobuf(#[from] prost::EncodeError),
    #[error("invalid protobuf message: {0}")]
    Invalid(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("protobuf decode error: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("invalid protobuf message: {0}")]
    Invalid(String),
}

pub fn encode_protocol_error(error: &ProtocolError) -> Error {
    match error {
        ProtocolError::NoAgentFound => simple_error(3, error.to_string()),
        ProtocolError::Unimplemented { message } => simple_error(10, message.clone()),
        ProtocolError::Cancelled { message } => simple_error(1, message.clone()),
        ProtocolError::InvalidArgument { message } => simple_error(2, message.clone()),
        ProtocolError::NotExposed { kind, protocol } => detailed_error(
            7,
            error.to_string(),
            "amux.v1.ProtocolNotExposed",
            ProtocolNotExposed {
                kind: Some(crate::agent_kind_to_wire(*kind)),
                protocol: agent_protocol_to_wire(*protocol) as i32,
            },
        ),
        ProtocolError::AlreadyExists { message } => simple_error(4, message.clone()),
        ProtocolError::PermissionDenied { message } => simple_error(5, message.clone()),
        ProtocolError::FailedPrecondition { message } => simple_error(7, message.clone()),
        ProtocolError::Unreachable { message } => simple_error(12, message.clone()),
        ProtocolError::AmbiguousAgentName { name, agent_ids } => {
            ambiguous_agent_name_error(name, agent_ids)
        }
        ProtocolError::ServerError { message } => simple_error(13, message.clone()),
        ProtocolError::InvalidCredentials => simple_error(6, error.to_string()),
        ProtocolError::PaymentRequired => {
            simple_error(ErrorCode::PaymentRequired as i32, error.to_string())
        }
        ProtocolError::ResourceExhausted { message } => simple_error(9, message.clone()),
        ProtocolError::ProtocolMismatch {
            supported_versions,
            peer_supported_versions,
        } => protocol_version_mismatch_error(supported_versions, peer_supported_versions),
        ProtocolError::UpdateRequired {
            minimum_version,
            client_version,
        } => update_required_error(minimum_version, client_version, None),
        ProtocolError::SequenceNumberMismatch {
            client_seq,
            current_seq,
        } => sequence_number_mismatch_error(*current_seq, *client_seq),
        ProtocolError::AttachmentMissing { id } => detailed_error(
            ErrorCode::NotFound as i32,
            error.to_string(),
            "amux.v1.AttachmentMissing",
            AttachmentMissing { id: id.clone() },
        ),
        ProtocolError::AttachmentTooLarge { size, max } => detailed_error(
            ErrorCode::ResourceExhausted as i32,
            error.to_string(),
            "amux.v1.AttachmentTooLarge",
            AttachmentTooLarge {
                size: *size,
                max: *max,
            },
        ),
        ProtocolError::ArtifactCorrupt { id } => detailed_error(
            ErrorCode::DataLoss as i32,
            error.to_string(),
            "amux.v1.ArtifactCorrupt",
            ArtifactCorrupt { id: id.clone() },
        ),
        ProtocolError::DiffUnavailable { message } => detailed_error(
            ErrorCode::FailedPrecondition as i32,
            message.clone(),
            "amux.v1.DiffUnavailable",
            DiffUnavailable {
                message: message.clone(),
            },
        ),
    }
}

pub fn decode_protocol_error(error: Error) -> ProtocolError {
    for detail in &error.details {
        match detail.r#type.as_str() {
            "amux.v1.ProtocolVersionMismatch" => {
                if let Ok(detail) = ProtocolVersionMismatch::decode(detail.value.as_slice()) {
                    return ProtocolError::ProtocolMismatch {
                        supported_versions: detail.supported_protocol_versions,
                        peer_supported_versions: detail.peer_supported_protocol_versions,
                    };
                }
            }
            "amux.v1.UpdateRequired" => {
                if let Ok(detail) = UpdateRequired::decode(detail.value.as_slice()) {
                    return ProtocolError::UpdateRequired {
                        minimum_version: detail.minimum_version,
                        client_version: detail.current_version,
                    };
                }
            }
            "amux.v1.SequenceNumberMismatch" => {
                if let Ok(detail) = SequenceNumberMismatch::decode(detail.value.as_slice()) {
                    return ProtocolError::SequenceNumberMismatch {
                        client_seq: detail.actual,
                        current_seq: detail.expected,
                    };
                }
            }
            "amux.v1.AttachmentMissing" => {
                if let Ok(detail) = AttachmentMissing::decode(detail.value.as_slice()) {
                    return ProtocolError::AttachmentMissing { id: detail.id };
                }
            }
            "amux.v1.AttachmentTooLarge" => {
                if let Ok(detail) = AttachmentTooLarge::decode(detail.value.as_slice()) {
                    return ProtocolError::AttachmentTooLarge {
                        size: detail.size,
                        max: detail.max,
                    };
                }
            }
            "amux.v1.ArtifactCorrupt" => {
                if let Ok(detail) = ArtifactCorrupt::decode(detail.value.as_slice()) {
                    return ProtocolError::ArtifactCorrupt { id: detail.id };
                }
            }
            "amux.v1.DiffUnavailable" => {
                if let Ok(detail) = DiffUnavailable::decode(detail.value.as_slice()) {
                    return ProtocolError::DiffUnavailable {
                        message: detail.message,
                    };
                }
            }
            "amux.v1.AmbiguousAgentName" => {
                if let Ok(detail) = AmbiguousAgentName::decode(detail.value.as_slice()) {
                    let mut agent_ids = Vec::with_capacity(detail.agent_ids.len());
                    for agent_id in detail.agent_ids {
                        let Ok(agent_id) = Uuid::from_slice(&agent_id) else {
                            return ProtocolError::ServerError {
                                message: format!(
                                    "invalid AmbiguousAgentName detail for {}",
                                    detail.name
                                ),
                            };
                        };
                        agent_ids.push(agent_id);
                    }
                    return ProtocolError::AmbiguousAgentName {
                        name: detail.name,
                        agent_ids,
                    };
                }
            }
            "amux.v1.ProtocolNotExposed" => {
                if let Ok(detail) = ProtocolNotExposed::decode(detail.value.as_slice())
                    && let Some(kind) = detail.kind
                    && let Ok(kind) = crate::agent_kind_from_wire(kind)
                    && let Ok(protocol) = AgentProtocol::try_from(detail.protocol)
                    && let Some(protocol) = agent_protocol_from_wire(protocol)
                {
                    return ProtocolError::NotExposed { kind, protocol };
                }
            }
            _ => {}
        }
    }

    match error.code {
        1 => ProtocolError::Cancelled {
            message: error.message,
        },
        2 => ProtocolError::InvalidArgument {
            message: error.message,
        },
        3 => ProtocolError::NoAgentFound,
        4 => ProtocolError::AlreadyExists {
            message: error.message,
        },
        5 => ProtocolError::PermissionDenied {
            message: error.message,
        },
        6 => ProtocolError::InvalidCredentials,
        7 => ProtocolError::FailedPrecondition {
            message: error.message,
        },
        9 => ProtocolError::ResourceExhausted {
            message: error.message,
        },
        10 => ProtocolError::Unimplemented {
            message: error.message,
        },
        12 => ProtocolError::Unreachable {
            message: error.message,
        },
        13 => ProtocolError::ServerError {
            message: error.message,
        },
        code if code == ErrorCode::PaymentRequired as i32 => ProtocolError::PaymentRequired,
        _ => ProtocolError::ServerError {
            message: error.message,
        },
    }
}

fn agent_protocol_to_wire(protocol: Protocol) -> AgentProtocol {
    match protocol {
        Protocol::TerminalV1 => AgentProtocol::TerminalV1,
        Protocol::ClaudePtyTranscriptV1 => AgentProtocol::ClaudePtyTranscriptV1,
        Protocol::ClaudeSdkV1 => AgentProtocol::ClaudeSdkV1,
        Protocol::CodexSdkV1 => AgentProtocol::CodexSdkV1,
        Protocol::TestEchoV1 => AgentProtocol::TestEchoV1,
    }
}

fn agent_protocol_from_wire(protocol: AgentProtocol) -> Option<Protocol> {
    match protocol {
        AgentProtocol::Unspecified => None,
        AgentProtocol::TerminalV1 => Some(Protocol::TerminalV1),
        AgentProtocol::ClaudePtyTranscriptV1 => Some(Protocol::ClaudePtyTranscriptV1),
        AgentProtocol::ClaudeSdkV1 => Some(Protocol::ClaudeSdkV1),
        AgentProtocol::CodexSdkV1 => Some(Protocol::CodexSdkV1),
        AgentProtocol::TestEchoV1 => Some(Protocol::TestEchoV1),
    }
}

pub fn protocol_status(error: ProtocolError) -> tonic::Status {
    let encoded = encode_protocol_error(&error);
    let details = encoded.encode_to_vec();
    match error {
        ProtocolError::NoAgentFound => {
            protocol_status_with_details(tonic::Code::NotFound, error.to_string(), details)
        }
        ProtocolError::Unimplemented { message } => {
            protocol_status_with_details(tonic::Code::Unimplemented, message, details)
        }
        ProtocolError::Cancelled { message } => {
            protocol_status_with_details(tonic::Code::Cancelled, message, details)
        }
        ProtocolError::InvalidArgument { message } => {
            protocol_status_with_details(tonic::Code::InvalidArgument, message, details)
        }
        ProtocolError::NotExposed { .. } => protocol_status_with_details(
            tonic::Code::FailedPrecondition,
            error.to_string(),
            details,
        ),
        ProtocolError::AlreadyExists { message } => {
            protocol_status_with_details(tonic::Code::AlreadyExists, message, details)
        }
        ProtocolError::PermissionDenied { message } => {
            protocol_status_with_details(tonic::Code::PermissionDenied, message, details)
        }
        ProtocolError::FailedPrecondition { message } => {
            protocol_status_with_details(tonic::Code::FailedPrecondition, message, details)
        }
        ProtocolError::Unreachable { message } => {
            protocol_status_with_details(tonic::Code::Unavailable, message, details)
        }
        ProtocolError::AmbiguousAgentName { .. } => {
            protocol_status_with_details(tonic::Code::InvalidArgument, error.to_string(), details)
        }
        ProtocolError::ServerError { message } => {
            protocol_status_with_details(tonic::Code::Internal, message, details)
        }
        ProtocolError::InvalidCredentials => {
            protocol_status_with_details(tonic::Code::Unauthenticated, error.to_string(), details)
        }
        ProtocolError::PaymentRequired => {
            protocol_status_with_details(tonic::Code::PermissionDenied, error.to_string(), details)
        }
        ProtocolError::ResourceExhausted { message } => {
            protocol_status_with_details(tonic::Code::ResourceExhausted, message, details)
        }
        ProtocolError::ProtocolMismatch { .. }
        | ProtocolError::UpdateRequired { .. }
        | ProtocolError::SequenceNumberMismatch { .. } => protocol_status_with_details(
            tonic::Code::FailedPrecondition,
            error.to_string(),
            details,
        ),
        ProtocolError::AttachmentMissing { .. } => {
            protocol_status_with_details(tonic::Code::NotFound, error.to_string(), details)
        }
        ProtocolError::AttachmentTooLarge { .. } => {
            protocol_status_with_details(tonic::Code::ResourceExhausted, error.to_string(), details)
        }
        ProtocolError::ArtifactCorrupt { .. } => {
            protocol_status_with_details(tonic::Code::DataLoss, error.to_string(), details)
        }
        ProtocolError::DiffUnavailable { message } => {
            protocol_status_with_details(tonic::Code::FailedPrecondition, message, details)
        }
    }
}

pub fn protocol_error_from_status_details(status: &tonic::Status) -> Option<ProtocolError> {
    if status.details().is_empty() {
        return None;
    }
    Error::decode(status.details())
        .ok()
        .map(decode_protocol_error)
}

fn protocol_status_with_details(
    code: tonic::Code,
    message: impl Into<String>,
    details: Vec<u8>,
) -> tonic::Status {
    tonic::Status::with_details(code, message, Bytes::from(details))
}

pub fn protocol_version_mismatch_error(
    supported_versions: &[u32],
    peer_supported_versions: &[u32],
) -> Error {
    detailed_error(
        7,
        "protocol version mismatch".to_string(),
        "amux.v1.ProtocolVersionMismatch",
        ProtocolVersionMismatch {
            supported_protocol_versions: supported_versions.to_vec(),
            peer_supported_protocol_versions: peer_supported_versions.to_vec(),
        },
    )
}

fn update_required_error(
    minimum_version: &str,
    current_version: &str,
    update_url: Option<String>,
) -> Error {
    detailed_error(
        7,
        format!("amux update required: minimum {minimum_version}, current {current_version}"),
        "amux.v1.UpdateRequired",
        UpdateRequired {
            minimum_version: minimum_version.to_string(),
            current_version: current_version.to_string(),
            update_url,
        },
    )
}

fn sequence_number_mismatch_error(expected: u64, actual: u64) -> Error {
    detailed_error(
        7,
        format!("sequence number mismatch: expected {expected}, actual {actual}"),
        "amux.v1.SequenceNumberMismatch",
        SequenceNumberMismatch { expected, actual },
    )
}

fn ambiguous_agent_name_error(name: &str, agent_ids: &[Uuid]) -> Error {
    detailed_error(
        2,
        format!("ambiguous agent name `{name}`"),
        "amux.v1.AmbiguousAgentName",
        AmbiguousAgentName {
            name: name.to_string(),
            agent_ids: agent_ids
                .iter()
                .map(|agent_id| agent_id.as_bytes().to_vec())
                .collect(),
        },
    )
}

fn simple_error(code: i32, message: String) -> Error {
    Error {
        code,
        message,
        details: Vec::new(),
    }
}

fn detailed_error<M>(code: i32, message: String, detail_type: &str, detail: M) -> Error
where
    M: ProstMessage,
{
    Error {
        code,
        message,
        details: vec![ErrorDetail {
            r#type: detail_type.to_string(),
            value: detail.encode_to_vec(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_uses_stable_wire_code() {
        let error = ProtocolError::PermissionDenied {
            message: "wrong scope".to_string(),
        };

        let encoded = encode_protocol_error(&error);
        assert_eq!(encoded.code, 5);
        assert!(encoded.details.is_empty());
        assert_eq!(decode_protocol_error(encoded), error);
    }

    #[test]
    fn payment_required_is_distinct_from_other_permission_denials() {
        let error = ProtocolError::PaymentRequired;

        let encoded = encode_protocol_error(&error);
        assert_eq!(encoded.code, ErrorCode::PaymentRequired as i32);
        assert!(encoded.details.is_empty());
        assert_eq!(decode_protocol_error(encoded), error);
    }

    #[test]
    fn ambiguous_agent_name_uses_typed_detail() {
        let error = ProtocolError::AmbiguousAgentName {
            name: "review".to_string(),
            agent_ids: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
        };

        let encoded = encode_protocol_error(&error);
        assert_eq!(encoded.code, 2);
        assert_eq!(encoded.details.len(), 1);
        assert_eq!(decode_protocol_error(encoded), error);
    }

    #[test]
    fn not_exposed_uses_typed_detail() {
        let error = ProtocolError::NotExposed {
            kind: model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            protocol: Protocol::ClaudeSdkV1,
        };

        let encoded = encode_protocol_error(&error);
        assert_eq!(encoded.code, 7);
        assert_eq!(encoded.details.len(), 1);
        assert_eq!(decode_protocol_error(encoded), error);

        let status = protocol_status(error.clone());
        assert_eq!(protocol_error_from_status_details(&status), Some(error));
    }

    #[test]
    fn attachment_and_diff_errors_round_trip_with_typed_details() {
        let cases = [
            (
                ProtocolError::AttachmentMissing {
                    id: "sha256:missing".to_string(),
                },
                "amux.v1.AttachmentMissing",
                tonic::Code::NotFound,
            ),
            (
                ProtocolError::AttachmentTooLarge {
                    size: 10_485_761,
                    max: 10_485_760,
                },
                "amux.v1.AttachmentTooLarge",
                tonic::Code::ResourceExhausted,
            ),
            (
                ProtocolError::ArtifactCorrupt {
                    id: "sha256:corrupt".to_string(),
                },
                "amux.v1.ArtifactCorrupt",
                tonic::Code::DataLoss,
            ),
            (
                ProtocolError::DiffUnavailable {
                    message: "working directory is not a git checkout".to_string(),
                },
                "amux.v1.DiffUnavailable",
                tonic::Code::FailedPrecondition,
            ),
        ];

        for (error, detail_type, status_code) in cases {
            let encoded = encode_protocol_error(&error);
            assert_eq!(encoded.details.len(), 1);
            assert_eq!(encoded.details[0].r#type, detail_type);
            assert_eq!(decode_protocol_error(encoded), error);

            let status = protocol_status(error.clone());
            assert_eq!(status.code(), status_code);
            assert_eq!(protocol_error_from_status_details(&status), Some(error));
        }
    }
}
