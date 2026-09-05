//! Single-writer credentials with durable staging and lifecycle invalidation.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::binding::{Binding, UserInfo, fetch_userinfo};
use crate::auth::{AccessToken, AuthError, CredentialProvider, oauth};

#[derive(Clone, Serialize, Deserialize)]
struct SavedCredential {
    binding: Binding,
    refresh_token: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    versions: BTreeMap<Uuid, SavedCredential>,
}

struct State {
    epoch: u64,
    active: Option<Uuid>,
    file: CredentialFile,
    cache: Option<AccessToken>,
    pending: Option<(AccessToken, String)>,
    account_mismatch: bool,
    host: Option<Arc<dyn CredentialProvider>>,
}

struct Core {
    path: Option<PathBuf>,
    http: reqwest::Client,
    state: Mutex<State>,
    refresh: tokio::sync::Mutex<()>,
}

pub(crate) struct ProfileCredentialStore {
    core: Arc<Core>,
}

pub(crate) struct StagedCredential {
    epoch: u64,
    credential: ValidatedCredential,
}

#[derive(Clone)]
pub(crate) struct ValidatedCredential {
    pub refresh_token: String,
    pub access: AccessToken,
    pub userinfo: UserInfo,
}

pub(crate) struct PreparedCredential {
    pub version: Uuid,
    epoch: u64,
    access: AccessToken,
}

impl ProfileCredentialStore {
    /// The registry's version is the commit point. An unreferenced stage is
    /// ignored after a crash; the previous accepted token remains available.
    pub(crate) fn open(
        path: Option<PathBuf>,
        http: reqwest::Client,
        binding: Option<&Binding>,
        active: Option<Uuid>,
    ) -> io::Result<Self> {
        let mut file = match path
            .as_ref()
            .filter(|_| active.is_some())
            .map(std::fs::read)
            .transpose()
        {
            Ok(Some(bytes)) => {
                serde_yaml::from_slice::<CredentialFile>(&bytes).map_err(io::Error::other)?
            }
            Ok(None) => CredentialFile::default(),
            Err(e) if e.kind() == io::ErrorKind::NotFound && active.is_none() => {
                CredentialFile::default()
            }
            Err(e) => return Err(e),
        };
        if let Some(version) = active {
            let saved = file
                .versions
                .get(&version)
                .ok_or_else(|| io::Error::other("committed credential is missing"))?;
            if binding != Some(&saved.binding) {
                return Err(io::Error::other(
                    "credential disagrees with profile binding",
                ));
            }
        }
        file.versions.retain(|version, _| Some(*version) == active);
        Ok(Self {
            core: Arc::new(Core {
                path,
                http,
                state: Mutex::new(State {
                    epoch: 0,
                    active,
                    file,
                    cache: None,
                    pending: None,
                    account_mismatch: false,
                    host: None,
                }),
                refresh: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// A host owns its secret storage and rotation. The profile still verifies
    /// userinfo and invalidates late responses at the same boundary as file credentials.
    pub(crate) fn use_host(&self, binding: &Binding, provider: Arc<dyn CredentialProvider>) {
        let mut state = self.core.state.lock().unwrap();
        let version = Uuid::new_v4();
        state.active = Some(version);
        state.file.versions.insert(
            version,
            SavedCredential {
                binding: binding.clone(),
                refresh_token: String::new(),
            },
        );
        state.host = Some(provider);
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.core.state.lock().unwrap().epoch
    }
    pub(crate) fn has_credential(&self) -> bool {
        self.core.state.lock().unwrap().active.is_some()
    }

    pub(crate) fn stage(&self, credential: ValidatedCredential, epoch: u64) -> StagedCredential {
        StagedCredential { epoch, credential }
    }

    /// Save a candidate alongside the active version. The registry must select
    /// it before activate is called; a failed registry write leaves the old one live.
    pub(crate) fn commit(
        &self,
        staged: StagedCredential,
        binding: &Binding,
    ) -> io::Result<PreparedCredential> {
        let mut state = self.core.state.lock().unwrap();
        if state.epoch != staged.epoch {
            return Err(cancelled());
        }
        if staged.credential.userinfo.sub != binding.account.subject {
            return Err(io::Error::other("credential account mismatch"));
        }
        let version = Uuid::new_v4();
        let mut file = state.file.clone();
        file.versions.retain(|id, _| Some(*id) == state.active);
        file.versions.insert(
            version,
            SavedCredential {
                binding: binding.clone(),
                refresh_token: staged.credential.refresh_token,
            },
        );
        self.core.persist(&mut state, file)?;
        Ok(PreparedCredential {
            version,
            epoch: staged.epoch,
            access: staged.credential.access,
        })
    }

    pub(crate) fn activate(&self, prepared: PreparedCredential) {
        let mut state = self.core.state.lock().unwrap();
        assert_eq!(
            state.epoch, prepared.epoch,
            "activation must hold the profile operation gate"
        );
        state.epoch += 1;
        state.active = Some(prepared.version);
        state.cache = Some(prepared.access);
        state.pending = None;
        state.account_mismatch = false;
        state.host = None;
        state.file.versions.retain(|id, _| *id == prepared.version);
    }

    /// Invalidates immediately, including when removing the file fails. The
    /// registry must first forget the version so a failed unlink cannot log in on restart.
    pub(crate) fn clear(&self) -> io::Result<()> {
        self.invalidate_pending();
        if let Some(path) = &self.core.path {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            #[cfg(unix)]
            std::fs::File::open(path.parent().unwrap())?.sync_all()?;
        }
        Ok(())
    }

    pub(crate) fn invalidate_pending(&self) {
        let mut state = self.core.state.lock().unwrap();
        state.epoch += 1;
        state.active = None;
        state.cache = None;
        state.pending = None;
        state.account_mismatch = false;
        state.host = None;
        state.file = CredentialFile::default();
    }
}

fn cancelled() -> io::Error {
    io::Error::other("credential operation invalidated")
}

impl Core {
    fn persist(&self, state: &mut State, file: CredentialFile) -> io::Result<()> {
        let Some(path) = &self.path else {
            state.file = file;
            return Ok(());
        };
        let parent = path.parent().unwrap();
        // Never recreate a removed profile directory on a late write.
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(
            serde_yaml::to_string(&file)
                .map_err(io::Error::other)?
                .as_bytes(),
        )?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|e| e.error)?;
        // A directory-sync failure follows a committed rename. Retain the
        // rotated token in memory as well as on disk even when reporting it.
        state.file = file;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        let _refresh = self.refresh.lock().await;
        let (epoch, version, saved, pending, host) = {
            let state = self.state.lock().unwrap();
            if state.account_mismatch {
                return Err(AuthError::AccountMismatch);
            }
            if let Some(token) = &state.cache
                && token
                    .expires_at
                    .is_none_or(|expiry| expiry > SystemTime::now() + Duration::from_secs(30))
            {
                return Ok(token.clone());
            }
            let version = state.active.ok_or(AuthError::Unauthenticated)?;
            (
                state.epoch,
                version,
                state.file.versions[&version].clone(),
                state.pending.clone(),
                state.host.clone(),
            )
        };
        let (access, refresh) = match pending {
            Some((access, refresh))
                if access
                    .expires_at
                    .is_none_or(|at| at > SystemTime::now() + Duration::from_secs(30)) =>
            {
                (access, refresh)
            }
            pending => {
                let refresh_token = pending
                    .map(|(_, refresh)| refresh)
                    .unwrap_or(saved.refresh_token);
                let (access, rotated) = if let Some(provider) = &host {
                    (provider.access_token().await?, None)
                } else {
                    oauth::refresh_access_token(
                        saved.binding.account.service.as_str(),
                        &refresh_token,
                    )
                    .await
                    .map_err(|error| match error {
                        oauth::OAuthError::RefreshTokenExpired => AuthError::Unauthenticated,
                        other => AuthError::Provider(other.to_string()),
                    })?
                };
                let refresh = rotated.unwrap_or(refresh_token);
                let mut state = self.state.lock().unwrap();
                if state.epoch != epoch || state.active != Some(version) {
                    return Err(AuthError::Unauthenticated);
                }
                // A transient userinfo or disk error must retry this result,
                // rather than spend the already-consumed refresh token again.
                state.pending = Some((access.clone(), refresh.clone()));
                (access, refresh)
            }
        };
        let info = fetch_userinfo(&self.http, saved.binding.account.service.as_str(), &access)
            .await
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        let mut state = self.state.lock().unwrap();
        if state.epoch != epoch || state.active != Some(version) {
            return Err(AuthError::Unauthenticated);
        }
        if info.sub != saved.binding.account.subject {
            state.cache = None;
            state.pending = None;
            state.account_mismatch = true;
            return Err(AuthError::AccountMismatch);
        }
        let mut file = state.file.clone();
        file.versions.get_mut(&version).unwrap().refresh_token = refresh;
        self.persist(&mut state, file)
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        state.cache = Some(access.clone());
        state.pending = None;
        Ok(access)
    }
}

#[async_trait::async_trait]
impl CredentialProvider for ProfileCredentialStore {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        // Refresh tokens are single-use. A cancelled connector must not cancel
        // the worker between consuming a token and saving its replacement.
        if self.core.state.lock().unwrap().host.is_some() {
            // A host provider owns the cancellation and refresh contract of
            // its own secrets; no profile-file transaction is pending here.
            return self.core.access_token().await;
        }
        let core = self.core.clone();
        tokio::spawn(async move { core.access_token().await })
            .await
            .map_err(|e| AuthError::Provider(e.to_string()))?
    }

    fn invalidate(&self, token: &AccessToken) {
        let mut state = self.core.state.lock().unwrap();
        if let Some(host) = &state.host {
            host.invalidate(token);
        }
        if state
            .cache
            .as_ref()
            .is_some_and(|cached| cached.bearer == token.bearer)
        {
            state.cache = None;
        }
    }
}
