use std::sync::atomic::Ordering;

use super::*;
use crate::auth::{AccessToken, oauth};
use crate::installation::binding::{
    AccountId, BindTarget, Binding, CloudServiceId, UserinfoError, fetch_userinfo,
};
use crate::installation::credentials::ValidatedCredential;

/// A refusal may require user confirmation. Retain the rotated token in memory
/// so that retrying that staged login does not spend its single-use token twice.
pub(super) struct PendingLogin {
    key: Vec<u8>,
    started: u64,
    refresh: String,
    access: AccessToken,
}

impl State {
    pub(super) fn revoke_credentials(&mut self, id: ProfileId) {
        self.credential_clock += 1;
        let entry = &self.profiles[&id];
        entry
            .slot
            .revoked_at
            .store(self.credential_clock, Ordering::Release);
        if let Some(binding) = &entry.status.record.binding {
            self.revoked_accounts
                .insert(binding.account.clone(), self.credential_clock);
        }
    }
}

impl Inner {
    pub(super) fn intent(&self, record: &ProfileRecord, slot: &Slot) -> Intent {
        if record.paused {
            Intent::Paused
        } else if record.binding.is_none() {
            Intent::Unbound
        } else if slot
            .credentials
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.has_credential())
        {
            Intent::Bound
        } else {
            Intent::LoggedOut
        }
    }

    pub(super) async fn logout_profile(
        &self,
        id: ProfileId,
        slot: &Slot,
    ) -> Result<ProfileStatus, InstallationError> {
        let result = {
            let mut state = self.state.lock().unwrap();
            let record = state.active(id)?.status.record.clone();
            let result = state.registry.commit_binding(record, None);
            // A directory-sync error can follow the registry's atomic rename.
            // Reconcile the actual commit point before reporting the error.
            if !state.registry.is_logged_out(id) {
                result?;
                unreachable!();
            }
            state.revoke_credentials(id);
            state.refresh_record(id);
            result
        };
        let cleared = slot
            .credentials
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.clear())
            .transpose();
        let runtime = slot.runtime.lock().await;
        if let Some(runtime) = runtime.as_ref() {
            runtime.stop_cloud().await;
            let cloud = self.state.lock().unwrap().profiles[&id]
                .status
                .record
                .binding
                .as_ref()
                .map(|b| b.account.service.to_string())
                .unwrap_or_default();
            runtime.configure_credentials(cloud, None).await;
        }
        let status = {
            let mut state = self.state.lock().unwrap();
            let entry = state.profiles.get_mut(&id).unwrap();
            entry.status.intent = if entry.status.record.binding.is_none() {
                Intent::Unbound
            } else if entry.status.record.paused {
                Intent::Paused
            } else {
                Intent::LoggedOut
            };
            state.publish(id);
            state.profiles[&id].status.clone()
        };
        result?;
        cleared?;
        Ok(status)
    }

    pub(super) async fn bind_request(
        self: &Arc<Self>,
        request: BindRequest,
    ) -> Result<ProfileStatus, BindError> {
        let service = CloudServiceId::canonicalize(&request.cloud_url)?;
        let started = {
            let state = self.state.lock().unwrap();
            if let BindTarget::Explicit(id) = request.target {
                state.active(id)?;
            }
            state.credential_clock
        };
        // Binding selects across profiles, so only binds share this lock. Logout
        // and delete remain free to revoke a login while identity HTTP is pending.
        let mut pending = self.binding.lock().await;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(service.as_str().as_bytes());
        digest.update(&[0]);
        digest.update(request.staged_refresh_token.as_bytes());
        let key = digest.finish().as_ref().to_vec();
        let index = match pending.iter().position(|login| login.key == key) {
            Some(index) => index,
            None => {
                let (access, rotated) =
                    oauth::refresh_access_token(service.as_str(), &request.staged_refresh_token)
                        .await
                        .map_err(|e| BindError::TokenRejected(e.to_string()))?;
                if pending.len() == LEDGER_CAPACITY {
                    pending.pop_front();
                }
                pending.push_back(PendingLogin {
                    key,
                    started,
                    access,
                    refresh: rotated.unwrap_or_else(|| request.staged_refresh_token.clone()),
                });
                pending.len() - 1
            }
        };
        let login = &mut pending[index];
        if login
            .access
            .expires_at
            .is_some_and(|at| at <= std::time::SystemTime::now())
        {
            let (access, rotated) = oauth::refresh_access_token(service.as_str(), &login.refresh)
                .await
                .map_err(|e| BindError::TokenRejected(e.to_string()))?;
            login.access = access;
            if let Some(token) = rotated {
                login.refresh = token;
            }
        }
        let info = fetch_userinfo(&self.identity_http, service.as_str(), &login.access)
            .await
            .map_err(|error| match error {
                UserinfoError::MissingSubject => BindError::MissingSubject,
                other => BindError::UserinfoUnavailable(other.to_string()),
            })?;
        let account = AccountId {
            service,
            subject: info.sub.clone(),
        };
        {
            let state = self.state.lock().unwrap();
            if state
                .revoked_accounts
                .get(&account)
                .is_some_and(|clock| *clock > login.started)
            {
                return Err(BindError::Cancelled);
            }
        }
        let id = match request.target {
            BindTarget::Explicit(id) => id,
            BindTarget::ByAccount => {
                let (bound, candidates) = {
                    let state = self.state.lock().unwrap();
                    let bound = state
                        .profiles
                        .values()
                        .find(|e| {
                            e.status
                                .record
                                .binding
                                .as_ref()
                                .is_some_and(|b| b.account == account)
                        })
                        .map(|e| e.status.record.id);
                    let candidates = state
                        .profiles
                        .values()
                        .filter(|e| {
                            !e.deleting && e.status.available && e.status.record.binding.is_none()
                        })
                        .map(|e| (e.status.record.id, e.slot.clone()))
                        .collect::<Vec<_>>();
                    (bound, candidates)
                };
                if let Some(id) = bound {
                    id
                } else {
                    let mut pristine = Vec::new();
                    for (id, slot) in candidates {
                        let _operation = slot.operations.lock().await;
                        if self.state.lock().unwrap().active(id).is_err() {
                            continue;
                        }
                        if let Some(runtime) = slot.runtime.lock().await.as_ref()
                            && runtime
                                .non_pristine()
                                .await
                                .map_err(BindError::Persist)?
                                .is_none()
                        {
                            pristine.push(id);
                        }
                    }
                    if pristine.len() == 1 {
                        pristine[0]
                    } else {
                        let id = ProfileId::new();
                        let (result, record) = {
                            let mut state = self.state.lock().unwrap();
                            let result = state.registry.create(id, ProfileLabel::default());
                            (result, state.registry.get(id).ok().cloned())
                        };
                        if let Some(record) = record {
                            self.insert(record);
                            self.start(id).await?;
                        }
                        result?;
                        id
                    }
                }
            }
        };
        let slot = self.state.lock().unwrap().active(id)?.slot.clone();
        let _operation = slot.operations.lock().await;
        let mut record = {
            let state = self.state.lock().unwrap();
            let entry = state.active(id)?;
            if state
                .revoked_accounts
                .get(&account)
                .is_some_and(|clock| *clock > login.started)
                || slot.revoked_at.load(Ordering::Acquire) > login.started
            {
                return Err(BindError::Cancelled);
            }
            if let Some(binding) = &entry.status.record.binding
                && binding.account != account
            {
                return Err(BindError::ProfileBoundToOtherAccount { profile: id });
            }
            if let Some(other) = state.profiles.values().find(|e| {
                e.status.record.id != id
                    && e.status
                        .record
                        .binding
                        .as_ref()
                        .is_some_and(|b| b.account == account)
            }) {
                return Err(BindError::AccountAlreadyBound {
                    profile: other.status.record.id,
                });
            }
            entry.status.record.clone()
        };
        let runtime = slot.runtime.lock().await;
        let runtime = runtime.as_ref().ok_or_else(|| {
            InstallationError::Unavailable("profile runtime is unavailable".into())
        })?;
        if record.binding.is_none()
            && !request.adopt_non_pristine
            && let Some(reason) = runtime.non_pristine().await.map_err(BindError::Persist)?
        {
            return Err(BindError::AdoptionNeedsConfirmation {
                profile: id,
                reason,
            });
        }
        let binding = record.binding.clone().unwrap_or_else(|| Binding {
            account,
            bound_at: chrono::Utc::now(),
        });
        record.binding = Some(binding.clone());
        record.label.account_name = info.name.clone();
        record.label.email = info.email.clone();
        let store = slot.credentials.lock().unwrap().clone().ok_or_else(|| {
            InstallationError::Unavailable("credential store is unavailable".into())
        })?;
        let staged = store.stage(
            ValidatedCredential {
                refresh_token: login.refresh.clone(),
                access: login.access.clone(),
                userinfo: info,
            },
            store.epoch(),
        );
        let prepared = store.commit(staged, &binding).map_err(BindError::Persist)?;
        let result = {
            let mut state = self.state.lock().unwrap();
            let result = state
                .registry
                .commit_binding(record, Some(prepared.version));
            if state.registry.credential_version(id) != Some(prepared.version) {
                result?;
                unreachable!();
            }
            store.activate(prepared);
            state.refresh_record(id);
            let entry = state.profiles.get_mut(&id).unwrap();
            entry.status.intent = self.intent(&entry.status.record, &slot);
            state.publish(id);
            result
        };
        pending.remove(index);
        // Replace a connector only after accepting the credential. Refused
        // logins never interrupt the existing link.
        runtime.stop_cloud().await;
        runtime
            .configure_credentials(binding.account.service.to_string(), Some(store))
            .await;
        if !self.state.lock().unwrap().profiles[&id]
            .status
            .record
            .paused
        {
            let _ = runtime.start_cloud().await;
        }
        result?;
        Ok(self.state.lock().unwrap().profiles[&id].status.clone())
    }
}
