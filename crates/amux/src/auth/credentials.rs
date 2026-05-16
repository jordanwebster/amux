use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct AccessToken {
    pub bearer: String,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authentication required")]
    Unauthenticated,
    #[error("auth provider error: {0}")]
    Provider(String),
}

#[async_trait::async_trait]
pub trait CredentialProvider: Send + Sync + 'static {
    /// Return a current access token. The provider owns refresh, caching, and
    /// any external token-endpoint calls.
    async fn access_token(&self) -> Result<AccessToken, AuthError>;

    /// Called when the server learns that this token was rejected.
    fn invalidate(&self, token: &AccessToken);
}
