use serde::Deserialize;

/// Claims from a cloud routing connection token.
#[derive(Debug, Deserialize)]
pub(crate) struct ConnectionClaims {
    /// User ID (subject).
    pub(crate) sub: String,
    /// Auth client that requested the cloud connection token.
    pub(crate) client_id: String,
    /// Expected host this token is for.
    pub(crate) host: String,
    /// Expected port this token is for.
    pub(crate) port: u16,
    /// Expiration time as seconds since Unix epoch.
    pub(crate) exp: u64,
}
