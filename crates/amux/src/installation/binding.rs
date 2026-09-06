//! Stable cloud account identity and the staged login contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{InstallationError, ProfileId};
use crate::auth::AccessToken;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CloudServiceId(String);

#[derive(Clone, Debug, thiserror::Error)]
#[error("cloud URL must be an HTTP(S) origin without credentials, path, query or fragment")]
pub struct CanonicalizeError;

impl CloudServiceId {
    pub fn canonicalize(value: &str) -> Result<Self, CanonicalizeError> {
        let url = reqwest::Url::parse(value).map_err(|_| CanonicalizeError)?;
        // Check the original path too: URL parsing normalizes /a/.. to /.
        let authority = value.split_once("://").ok_or(CanonicalizeError)?.1;
        let suffix = authority
            .find(['/', '?', '#'])
            .map(|i| &authority[i..])
            .unwrap_or("");
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.path() != "/"
            || value.contains('\\')
            || value.chars().any(char::is_whitespace)
            || !url.username().is_empty()
            || url.password().is_some()
            || authority
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .contains('@')
            || !matches!(suffix, "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CanonicalizeError);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for CloudServiceId {
    type Error = CanonicalizeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::canonicalize(&value)
    }
}
impl From<CloudServiceId> for String {
    fn from(value: CloudServiceId) -> Self {
        value.0
    }
}
impl std::fmt::Display for CloudServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountId {
    pub service: CloudServiceId,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub account: AccountId,
    pub bound_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindTarget {
    Explicit(ProfileId),
    ByAccount,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BindRequest {
    pub target: BindTarget,
    pub cloud_url: String,
    pub staged_refresh_token: String,
    pub adopt_non_pristine: bool,
}
impl std::fmt::Debug for BindRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindRequest")
            .field("target", &self.target)
            .field("cloud_url", &self.cloud_url)
            .field("adopt_non_pristine", &self.adopt_non_pristine)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NonPristine {
    TrustEntries(usize),
    LocalAgents(usize),
    RetainedArtifacts(usize),
    ConcurrentOperation,
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("account is already bound to profile {profile}")]
    AccountAlreadyBound { profile: ProfileId },
    #[error("profile {profile} belongs to another account")]
    ProfileBoundToOtherAccount { profile: ProfileId },
    #[error("adopting profile {profile} requires confirmation: {reason:?}")]
    AdoptionNeedsConfirmation {
        profile: ProfileId,
        reason: NonPristine,
    },
    #[error("userinfo unavailable: {0}")]
    UserinfoUnavailable(String),
    #[error("userinfo did not provide a subject")]
    MissingSubject,
    #[error("token rejected: {0}")]
    TokenRejected(String),
    #[error("cannot persist credential: {0}")]
    Persist(std::io::Error),
    #[error("login invalidated by a lifecycle operation")]
    Cancelled,
    #[error(transparent)]
    InvalidCloudUrl(#[from] CanonicalizeError),
    #[error(transparent)]
    Installation(#[from] InstallationError),
}
impl Clone for BindError {
    fn clone(&self) -> Self {
        match self {
            Self::AccountAlreadyBound { profile } => {
                Self::AccountAlreadyBound { profile: *profile }
            }
            Self::ProfileBoundToOtherAccount { profile } => {
                Self::ProfileBoundToOtherAccount { profile: *profile }
            }
            Self::AdoptionNeedsConfirmation { profile, reason } => {
                Self::AdoptionNeedsConfirmation {
                    profile: *profile,
                    reason: reason.clone(),
                }
            }
            Self::UserinfoUnavailable(s) => Self::UserinfoUnavailable(s.clone()),
            Self::MissingSubject => Self::MissingSubject,
            Self::TokenRejected(s) => Self::TokenRejected(s.clone()),
            Self::Persist(e) => Self::Persist(std::io::Error::new(e.kind(), e.to_string())),
            Self::Cancelled => Self::Cancelled,
            Self::InvalidCloudUrl(e) => Self::InvalidCloudUrl(e.clone()),
            Self::Installation(e) => Self::Installation(e.clone()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub picture: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum UserinfoError {
    #[error("userinfo did not provide a subject")]
    MissingSubject,
    #[error("userinfo request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub(crate) async fn fetch_userinfo(
    http: &reqwest::Client,
    cloud_url: &str,
    token: &AccessToken,
) -> Result<UserInfo, UserinfoError> {
    let info: UserInfo = http
        .get(format!("{cloud_url}/connect/userinfo"))
        .timeout(std::time::Duration::from_secs(15))
        .bearer_auth(&token.bearer)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if info.sub.trim().is_empty() {
        return Err(UserinfoError::MissingSubject);
    }
    Ok(info)
}

#[cfg(test)]
mod tests;
