use prost::Message as ProstMessage;

use super::{
    AmbiguousAgentName, ConnectResponse, Error, ErrorDetail, InvalidLinkName,
    ProtocolVersionMismatch, SequenceNumberMismatch, UpdateRequired, connect_response,
};
use crate::protocol::message::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum EncodeError {
    #[error("protobuf encode error: {0}")]
    Protobuf(#[from] prost::EncodeError),
    #[error("invalid protobuf message: {0}")]
    Invalid(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("protobuf decode error: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("invalid protobuf message: {0}")]
    Invalid(String),
}

pub(crate) fn encode_protocol_error(error: &ProtocolError) -> Error {
    match error {
        ProtocolError::NoAgentFound => simple_error(3, error.to_string()),
        ProtocolError::Unimplemented { message } => simple_error(10, message.clone()),
        ProtocolError::Cancelled { message } => simple_error(1, message.clone()),
        ProtocolError::InvalidArgument { message } => simple_error(2, message.clone()),
        ProtocolError::AlreadyExists { message } => simple_error(4, message.clone()),
        ProtocolError::PermissionDenied { message } => simple_error(5, message.clone()),
        ProtocolError::Unreachable { message } => simple_error(12, message.clone()),
        ProtocolError::ServerError { message } => simple_error(13, message.clone()),
        ProtocolError::InvalidCredentials => simple_error(6, error.to_string()),
        ProtocolError::ResourceExhausted { message } => simple_error(9, message.clone()),
        ProtocolError::InvalidLinkName { name, reason } => invalid_link_name_error(name, reason),
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
        ProtocolError::Unknown => simple_error(13, error.to_string()),
    }
}

pub(crate) fn decode_protocol_error(error: Error) -> ProtocolError {
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
            "amux.v1.InvalidLinkName" => {
                if let Ok(detail) = InvalidLinkName::decode(detail.value.as_slice()) {
                    return ProtocolError::InvalidLinkName {
                        name: detail.name,
                        reason: detail.reason,
                    };
                }
            }
            "amux.v1.AmbiguousAgentName" => {
                if let Ok(detail) = AmbiguousAgentName::decode(detail.value.as_slice()) {
                    return ProtocolError::ServerError {
                        message: format!("ambiguous agent name: {}", detail.name),
                    };
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
        _ => ProtocolError::ServerError {
            message: error.message,
        },
    }
}

pub(crate) fn encode_connect_invalid_link_name_response(name: &str, reason: &str) -> Vec<u8> {
    encode_connect_wire_error_response(invalid_link_name_error(name, reason))
}

pub(crate) fn encode_connect_protocol_version_mismatch_response(
    supported_versions: &[u32],
    peer_supported_versions: &[u32],
) -> Vec<u8> {
    encode_connect_wire_error_response(protocol_version_mismatch_error(
        supported_versions,
        peer_supported_versions,
    ))
}

pub(crate) fn invalid_link_name_error(name: &str, reason: &str) -> Error {
    let detail = InvalidLinkName {
        name: name.to_string(),
        reason: reason.to_string(),
    };
    detailed_error(
        2,
        if name.is_empty() {
            "invalid link name".to_string()
        } else {
            format!("invalid link name `{name}`: {reason}")
        },
        "amux.v1.InvalidLinkName",
        detail,
    )
}

fn encode_connect_wire_error_response(error: Error) -> Vec<u8> {
    ConnectResponse {
        outcome: Some(connect_response::Outcome::Error(error)),
    }
    .encode_to_vec()
}

pub(crate) fn protocol_version_mismatch_error(
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
}
