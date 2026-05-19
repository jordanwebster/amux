use crate::HostId;
use crate::protocol::wire as pb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TunnelId {
    pub(crate) initiator: HostId,
    pub(crate) nonce: uuid::Uuid,
}

impl TunnelId {
    pub(crate) fn new(initiator: HostId) -> Self {
        Self {
            initiator,
            nonce: uuid::Uuid::new_v4(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_parts(initiator: HostId, nonce: uuid::Uuid) -> Self {
        Self { initiator, nonce }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TunnelTypeError {
    #[error("{field} must be 16 bytes, got {actual}")]
    InvalidUuidLength { field: &'static str, actual: usize },
}

impl TryFrom<pb::TunnelId> for TunnelId {
    type Error = TunnelTypeError;

    fn try_from(value: pb::TunnelId) -> Result<Self, Self::Error> {
        Ok(Self {
            initiator: uuid_from_bytes("TunnelId.initiator", value.initiator)?,
            nonce: uuid_from_bytes("TunnelId.nonce", value.nonce)?,
        })
    }
}

impl From<TunnelId> for pb::TunnelId {
    fn from(value: TunnelId) -> Self {
        Self {
            initiator: value.initiator.as_bytes().to_vec(),
            nonce: value.nonce.as_bytes().to_vec(),
        }
    }
}

fn uuid_from_bytes(field: &'static str, bytes: Vec<u8>) -> Result<HostId, TunnelTypeError> {
    let actual = bytes.len();
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| TunnelTypeError::InvalidUuidLength { field, actual })?;
    Ok(HostId::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_id_roundtrips_wire_shape() {
        let id = TunnelId::from_parts(HostId::from_u128(1), uuid::Uuid::from_u128(2));
        let wire: pb::TunnelId = id.into();

        assert_eq!(TunnelId::try_from(wire).unwrap(), id);
    }

    #[test]
    fn tunnel_id_rejects_invalid_uuid_lengths() {
        let error = TunnelId::try_from(pb::TunnelId {
            initiator: vec![1, 2, 3],
            nonce: uuid::Uuid::from_u128(2).as_bytes().to_vec(),
        })
        .unwrap_err();

        assert_eq!(
            error,
            TunnelTypeError::InvalidUuidLength {
                field: "TunnelId.initiator",
                actual: 3
            }
        );

        let error = TunnelId::try_from(pb::TunnelId {
            initiator: HostId::from_u128(1).as_bytes().to_vec(),
            nonce: vec![1, 2, 3],
        })
        .unwrap_err();

        assert_eq!(
            error,
            TunnelTypeError::InvalidUuidLength {
                field: "TunnelId.nonce",
                actual: 3
            }
        );
    }
}
