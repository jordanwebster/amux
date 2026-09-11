//! Tunnel identity: a plain 16-byte UUID minted by the initiator.
//!
//! The id carries no structure — the reply address travels once, in
//! `TunnelOpen.src` — so the wire shape is just `bytes tunnel_id`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TunnelId(uuid::Uuid);

impl TunnelId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub(crate) fn to_wire(self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }

    pub(crate) fn from_wire(bytes: &[u8]) -> Result<Self, TunnelTypeError> {
        let bytes: [u8; 16] = bytes
            .try_into()
            .map_err(|_| TunnelTypeError::InvalidUuidLength {
                field: "tunnel_id",
                actual: bytes.len(),
            })?;
        Ok(Self(uuid::Uuid::from_bytes(bytes)))
    }

    #[cfg(test)]
    pub(crate) fn from_u128(value: u128) -> Self {
        Self(uuid::Uuid::from_u128(value))
    }
}

impl std::fmt::Display for TunnelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.as_simple())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TunnelTypeError {
    #[error("{field} must be 16 bytes, got {actual}")]
    InvalidUuidLength { field: &'static str, actual: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_id_roundtrips_wire_shape() {
        let id = TunnelId::new();

        assert_eq!(TunnelId::from_wire(&id.to_wire()).unwrap(), id);
    }

    #[test]
    fn tunnel_id_rejects_invalid_uuid_lengths() {
        let error = TunnelId::from_wire(&[1, 2, 3]).unwrap_err();

        assert_eq!(
            error,
            TunnelTypeError::InvalidUuidLength {
                field: "tunnel_id",
                actual: 3
            }
        );
    }
}
