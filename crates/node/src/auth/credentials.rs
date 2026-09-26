use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct AccessToken {
    pub bearer: String,
    pub expires_at: Option<SystemTime>,
    /// What the account service said this token buys, where it said anything.
    ///
    /// A bearer is opaque to this process, so nothing here can read a tier out
    /// of one; the only honest source is whoever obtained it. A provider that
    /// says nothing leaves this absent and every reader treats the account as
    /// free, which is the assumption that can never grant more than was paid
    /// for.
    pub tier: Option<crate::Tier>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authentication required")]
    Unauthenticated,
    #[error("credential belongs to a different account")]
    AccountMismatch,
    #[error("auth provider error: {0}")]
    Provider(String),
}

#[async_trait::async_trait]
pub trait CredentialProvider: Send + Sync + 'static {
    /// Return a current access token.
    ///
    /// The provider owns credential-source-specific freshness work, such as
    /// token refresh, access-token caching, credential persistence, refresh
    /// concurrency control, and classification of failures as unauthenticated
    /// vs retriable provider errors. The core only consumes the returned bearer.
    async fn access_token(&self) -> Result<AccessToken, AuthError>;

    /// Called when the server learns that this token was rejected.
    fn invalidate(&self, token: &AccessToken);
}
