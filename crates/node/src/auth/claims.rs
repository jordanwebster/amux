use serde::{Deserialize, Serialize};

/// Account tier carried by cloud connection tokens.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Free,
    Pro,
}

/// Claims from a cloud routing connection token.
#[derive(Debug, Deserialize)]
pub struct ConnectionClaims {
    /// User ID (subject).
    pub sub: String,
    /// Auth client that requested the cloud connection token.
    pub client_id: String,
    /// Expected host this token is for.
    pub host: String,
    /// Expected port this token is for.
    pub port: u16,
    /// Expiration time as seconds since Unix epoch.
    pub exp: u64,
    /// Relay entitlement carried by this token.
    pub tier: Tier,
}
