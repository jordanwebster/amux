//! A profile's account: which cloud account the profile is bound to, the
//! credential that signs it in, and whether its connection is paused.
//!
//! One private file, `profiles/<id>/account`, written whole on every change.
//! Its absence is an unbound profile; a binding without a refresh token is a
//! profile that signed out but stays tied to its account; `paused` keeps the
//! credential and holds the cloud link down.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::binding::CloudServiceId;
use crate::auth::oauth::{self, OAuthError};
use crate::auth::{AccessToken, AuthError, CredentialProvider};
use crate::identity::atomic_replace_private;

pub const ACCOUNT_FILE: &str = "account";
/// An access token this close to expiry is refreshed before it is handed out.
const ACCESS_TOKEN_MARGIN: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountRecord {
    pub(crate) service: CloudServiceId,
    /// The account's subject at that service: what makes two logins the
    /// same account.
    pub(crate) subject: String,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) bound_at_ms: i64,
    /// Absent after a sign-out.
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) paused: bool,
}

/// The account file and the access token minted from it.
pub(crate) struct Account {
    path: PathBuf,
    record: Mutex<Option<AccountRecord>>,
    access: Mutex<Option<AccessToken>>,
    /// One refresh at a time: a refresh token may be single use.
    refresh: tokio::sync::Mutex<()>,
}

impl Account {
    pub(crate) fn open(profile_dir: &Path) -> io::Result<Arc<Self>> {
        let path = profile_dir.join(ACCOUNT_FILE);
        let record = match std::fs::read(&path) {
            Ok(bytes) => Some(
                serde_json::from_slice(&bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Arc::new(Self {
            path,
            record: Mutex::new(record),
            access: Mutex::new(None),
            refresh: tokio::sync::Mutex::new(()),
        }))
    }

    pub(crate) fn record(&self) -> Option<AccountRecord> {
        self.record.lock().unwrap().clone()
    }

    pub(crate) fn intent(&self) -> wire::Intent {
        match &*self.record.lock().unwrap() {
            None => wire::Intent::Unbound,
            Some(record) if record.paused => wire::Intent::Paused,
            Some(record) if record.refresh_token.is_some() => wire::Intent::Bound,
            Some(_) => wire::Intent::LoggedOut,
        }
    }

    /// Whether the cloud link should run: bound, signed in and not paused.
    pub(crate) fn wants_cloud(&self) -> bool {
        self.intent() == wire::Intent::Bound
    }

    pub(crate) fn service(&self) -> Option<CloudServiceId> {
        self.record
            .lock()
            .unwrap()
            .as_ref()
            .map(|record| record.service.clone())
    }

    fn write(&self, record: Option<AccountRecord>) -> io::Result<()> {
        match &record {
            Some(record) => {
                let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
                atomic_replace_private(&self.path, &bytes).map_err(io::Error::other)?;
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            },
        }
        *self.record.lock().unwrap() = record;
        Ok(())
    }

    /// Binds the profile to an account, or signs a bound profile in again.
    pub(crate) fn bind(&self, record: AccountRecord, access: AccessToken) -> io::Result<()> {
        self.write(Some(record))?;
        *self.access.lock().unwrap() = Some(access);
        Ok(())
    }

    /// Forgets the credential and keeps the binding.
    pub(crate) fn sign_out(&self) -> io::Result<()> {
        let record = self.record().map(|record| AccountRecord {
            refresh_token: None,
            ..record
        });
        self.write(record)?;
        *self.access.lock().unwrap() = None;
        Ok(())
    }

    pub(crate) fn set_paused(&self, paused: bool) -> io::Result<()> {
        let record = self
            .record()
            .ok_or_else(|| io::Error::other("the profile is not bound to an account"))?;
        self.write(Some(AccountRecord { paused, ..record }))
    }
}

/// The credential the cloud link presents: an access token minted from the
/// account's refresh token, which the identity service may rotate.
#[derive(Clone)]
pub(crate) struct AccountCredentials(pub(crate) Arc<Account>);

#[async_trait::async_trait]
impl CredentialProvider for AccountCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        let account = &self.0;
        let _refreshing = account.refresh.lock().await;
        if let Some(access) = account.access.lock().unwrap().clone()
            && access
                .expires_at
                .is_none_or(|at| at > SystemTime::now() + ACCESS_TOKEN_MARGIN)
        {
            return Ok(access);
        }
        let record = account.record().ok_or(AuthError::Unauthenticated)?;
        let refresh = record
            .refresh_token
            .clone()
            .ok_or(AuthError::Unauthenticated)?;
        let (access, rotated) = oauth::refresh_access_token(record.service.as_str(), &refresh)
            .await
            .map_err(|error| match error {
                OAuthError::RefreshTokenExpired => AuthError::Unauthenticated,
                other => AuthError::Provider(other.to_string()),
            })?;
        if let Some(rotated) = rotated
            && rotated != refresh
        {
            // Written before the access token is used: a refresh token the
            // service rotated is the only one that works from now on.
            account
                .write(Some(AccountRecord {
                    refresh_token: Some(rotated),
                    ..record
                }))
                .map_err(|error| AuthError::Provider(error.to_string()))?;
        }
        *account.access.lock().unwrap() = Some(access.clone());
        Ok(access)
    }

    fn invalidate(&self, token: &AccessToken) {
        let mut access = self.0.access.lock().unwrap();
        if access
            .as_ref()
            .is_some_and(|cached| cached.bearer == token.bearer)
        {
            *access = None;
        }
    }
}

/// Binds a profile to the account a staged login names: the profile the
/// request names, or the one already bound to that account, or the one
/// profile that holds nothing yet, or a new one. A profile that already
/// holds agents or paired hosts is adopted only when the request says so.
pub(crate) async fn bind(
    installation: &Arc<crate::daemon::Installation>,
    explicit: Option<crate::ProfileId>,
    request: wire::BindProfileRequest,
) -> Result<crate::ProfileId, tonic::Status> {
    use wire::ErrorCode;

    use crate::front_door::failed;

    let service = CloudServiceId::canonicalize(&request.cloud_url)
        .map_err(|error| failed(ErrorCode::InvalidArgument, error.to_string()))?;
    if request.staged_refresh_token.is_empty() {
        return Err(failed(
            ErrorCode::InvalidArgument,
            "the login carries no refresh token",
        ));
    }
    let (access, rotated) =
        oauth::refresh_access_token(service.as_str(), &request.staged_refresh_token)
            .await
            .map_err(|error| failed(ErrorCode::Unauthenticated, error.to_string()))?;
    let refresh = rotated.unwrap_or(request.staged_refresh_token);
    let info = super::binding::fetch_userinfo(&reqwest::Client::new(), service.as_str(), &access)
        .await
        .map_err(|error| failed(ErrorCode::Unavailable, error.to_string()))?;
    let same_account =
        |record: &AccountRecord| record.service == service && record.subject == info.sub;

    let _change = installation.registry_changes.lock().await;
    let hosted = installation
        .hosted
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(id, hosted)| {
            hosted
                .runtime
                .edge()
                .map(|edge| (*id, hosted.position, hosted.runtime.clone(), edge))
        })
        .collect::<Vec<_>>();
    let bound = hosted
        .iter()
        .find(|(_, _, _, edge)| {
            edge.account()
                .record()
                .is_some_and(|record| same_account(&record))
        })
        .map(|(id, ..)| *id);
    let id = match explicit {
        Some(id) => {
            if bound.is_some_and(|bound| bound != id) {
                return Err(failed(
                    ErrorCode::AlreadyExists,
                    "that account is already signed in on another profile",
                ));
            }
            id
        }
        None => match bound {
            Some(id) => id,
            None => {
                let mut unbound = Vec::new();
                for (id, position, runtime, edge) in &hosted {
                    if edge.account().record().is_none()
                        && pristine(runtime, edge)
                            .await
                            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?
                    {
                        unbound.push((*position, *id));
                    }
                }
                unbound.sort();
                match unbound.first() {
                    Some((_, id)) => *id,
                    None => {
                        let label = info.name.clone().unwrap_or_else(|| "account".to_owned());
                        installation
                            .create(&label)
                            .await
                            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?
                    }
                }
            }
        },
    };
    let (runtime, edge) = {
        let hosted = installation.hosted.lock().unwrap();
        let hosted = hosted
            .get(&id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        let edge = hosted
            .runtime
            .edge()
            .ok_or_else(|| failed(ErrorCode::Unavailable, "the profile is not in service"))?;
        (hosted.runtime.clone(), edge)
    };
    let existing = edge.account().record();
    match &existing {
        Some(record) if !same_account(record) => {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile belongs to another account",
            ));
        }
        None if !request.adopt_non_pristine
            && !pristine(&runtime, &edge)
                .await
                .map_err(|error| failed(ErrorCode::Internal, error.to_string()))? =>
        {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile already holds agents or paired hosts; confirm adopting it",
            ));
        }
        _ => {}
    }
    let record = AccountRecord {
        service,
        subject: info.sub,
        name: info.name,
        email: info.email,
        bound_at_ms: existing.as_ref().map_or_else(
            || crate::link::run::system_time_ms(SystemTime::now()),
            |record| record.bound_at_ms,
        ),
        refresh_token: Some(refresh),
        paused: existing.is_some_and(|record| record.paused),
    };
    edge.bind_account(record, access)
        .await
        .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
    Ok(id)
}

/// Whether a profile holds nothing an account would adopt: no agents and
/// no paired hosts.
async fn pristine(
    runtime: &crate::runtime::ProfileRuntime,
    edge: &super::Edge,
) -> Result<bool, store::StoreError> {
    use store::Store as _;
    Ok(edge.has_no_peers() && runtime.store().await.agents()?.is_empty())
}
