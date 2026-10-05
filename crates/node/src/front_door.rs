//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, pairs them with other hosts,
//! signs them in to an account, and manages the installation.

use std::pin::Pin;
use std::sync::{Arc, Weak};

use agent_dir::local_socket::LocalListener;
use futures_util::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wire::installation_service_server::{InstallationService, InstallationServiceServer};
use wire::profile_service_server::{ProfileService, ProfileServiceServer};
use wire::{
    BindProfileRequest, CancelPairingResponse, CaughtUp, CreateProfileRequest,
    DeleteProfileRequest, DeleteProfileResponse, DeviceIdentity, Empty, ErrorCode, GetInfoRequest,
    GetPairingStatusResponse, GetPeerResponse, InstallationInfo, InstallationShutdownRequest,
    Intent, ListPeersResponse, ListProfilesRequest, ListProfilesResponse, PairingAbandoned,
    PendingPairResponse, ProfileBeginPairRequest, ProfileGetPeerRequest, ProfileInfo,
    ProfileOperation, ProfilePairingStatusRequest, ProfilePendingPairRequest, ProfileRequest,
    ProfileStartPairingRequest, ProfileTrustSshPeerRequest, ProfileUnpairRequest,
    RenameProfileRequest, ShutdownResponse, StartPairingResponse, UnpairResponse,
    WatchProfilesRequest, WatchProfilesResponse, watch_profiles_response,
};

use crate::Tier;
use crate::daemon::{Hosted, Installation};
use crate::edge::{Edge, Observed, RelayCarrier, account};
use crate::grpc::{self, status, wire_error};
use crate::profiles::{PROFILE_SOCKET, Registry, profile_dir};

/// A change the front door's watchers see.
#[derive(Clone, Debug)]
pub(crate) enum ProfileEvent {
    Upserted(Box<ProfileInfo>),
    Removed(String),
}

/// A hosted profile as the front door describes it.
pub(crate) fn info(hosted: &Hosted) -> ProfileInfo {
    let edge = hosted.runtime.edge();
    let record = edge.as_ref().and_then(|edge| edge.account().record());
    let observed = edge.as_ref().map(|edge| edge.observed());
    let (tier, relay_carrier) = match &observed {
        Some(Observed::Connected { tier, carrier }) => (
            match tier {
                Tier::Free => wire::Tier::Free,
                Tier::Pro => wire::Tier::Pro,
            },
            match carrier {
                RelayCarrier::Quic => wire::RelayCarrier::Quic,
                RelayCarrier::Tcp => wire::RelayCarrier::Tcp,
            },
        ),
        _ => (wire::Tier::Unspecified, wire::RelayCarrier::Unspecified),
    };
    ProfileInfo {
        email: record
            .as_ref()
            .and_then(|record| record.email.clone())
            .unwrap_or_default(),
        account_name: record
            .as_ref()
            .and_then(|record| record.name.clone())
            .unwrap_or_default(),
        account_subject: record
            .as_ref()
            .map(|record| record.subject.clone())
            .unwrap_or_default(),
        intent: edge
            .as_ref()
            .map_or(Intent::Unbound, |edge| edge.account().intent()) as i32,
        observed: observed.map_or(wire::Observed::Local, |observed| observed.to_wire()) as i32,
        tier: tier as i32,
        relay_carrier: relay_carrier as i32,
        id: hosted.entry.id.to_string(),
        label: hosted.entry.label.clone(),
        socket_path: hosted
            .runtime
            .dir()
            .join(PROFILE_SOCKET)
            .to_string_lossy()
            .into_owned(),
        host_id: hosted.runtime.host().to_string(),
        revision: hosted.entry.revision,
        available: true,
        ..ProfileInfo::default()
    }
}

/// The analytics a sign-in's outcome is recorded on: the profile it bound
/// or named, else the oldest.
fn sign_in_analytics(
    installation: &Installation,
    profile: Option<crate::ProfileId>,
) -> Option<analytics::Analytics> {
    let hosted = installation.hosted.lock().unwrap();
    profile
        .and_then(|profile| hosted.get(&profile))
        .or_else(|| hosted.values().min_by_key(|hosted| hosted.position))
        .map(|hosted| hosted.runtime.analytics().clone())
}

/// Why a sign-in failed, as analytics reads the refusal.
fn sign_in_failure(status: &Status) -> analytics::SignInFailure {
    use analytics::SignInFailure;
    match crate::net_error::code_of(status) {
        Some(ErrorCode::Unauthenticated) => SignInFailure::Rejected,
        Some(ErrorCode::Unavailable) => SignInFailure::Unreachable,
        Some(ErrorCode::AlreadyExists) => SignInFailure::AccountElsewhere,
        Some(ErrorCode::FailedPrecondition) => SignInFailure::ProfileConflict,
        _ => SignInFailure::Other,
    }
}

/// Serves the profile and installation services on the front door until
/// the task is aborted.
pub(crate) fn serve(listener: LocalListener, installation: Weak<Installation>) -> JoinHandle<()> {
    let door = FrontDoor::new(installation);
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .add_service(ProfileServiceServer::new(door.clone()))
            .add_service(InstallationServiceServer::new(door))
            .serve_with_incoming(grpc::incoming(listener))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the front door stopped serving");
        }
    })
}

/// The profile and installation services, served on the front door's
/// socket or called in process by an embedder that serves no sockets.
#[derive(Clone)]
pub struct FrontDoor {
    installation: Weak<Installation>,
}

impl FrontDoor {
    pub(crate) fn new(installation: Weak<Installation>) -> FrontDoor {
        FrontDoor { installation }
    }
}

pub(crate) fn failed(code: ErrorCode, message: impl Into<String>) -> Status {
    status(wire_error(code, message))
}

fn profile_id(text: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(text).map_err(|_| {
        failed(
            ErrorCode::InvalidArgument,
            format!("{text:?} is not a profile id"),
        )
    })
}

impl FrontDoor {
    fn installation(&self) -> Result<Arc<Installation>, Status> {
        self.installation
            .upgrade()
            .ok_or_else(|| Status::unavailable("the daemon is shutting down"))
    }

    /// Every hosted profile, oldest first.
    fn list(installation: &Installation) -> Vec<ProfileInfo> {
        let hosted = installation.hosted.lock().unwrap();
        let mut hosted: Vec<&Hosted> = hosted.values().collect();
        hosted.sort_by_key(|hosted| hosted.position);
        hosted.into_iter().map(info).collect()
    }

    /// A hosted profile's network edge.
    fn edge(installation: &Installation, id: &str) -> Result<(Uuid, Arc<Edge>), Status> {
        let id = profile_id(id)?;
        let edge = installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?
            .runtime
            .edge()
            .ok_or_else(|| failed(ErrorCode::Unavailable, "the profile is not in service"))?;
        Ok((id, edge))
    }

    async fn set_paused(
        &self,
        request: ProfileOperation,
        paused: bool,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let (id, edge) = Self::edge(&installation, &request.profile_id)?;
        if edge.account().record().is_none() {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile is not bound to an account",
            ));
        }
        edge.set_paused(paused)
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    fn one(installation: &Installation, id: Uuid) -> Result<ProfileInfo, Status> {
        installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(info)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))
    }
}

type Events = Pin<Box<dyn Stream<Item = Result<WatchProfilesResponse, Status>> + Send>>;

#[tonic::async_trait]
impl ProfileService for FrontDoor {
    type WatchProfilesStream = Events;

    async fn list_profiles(
        &self,
        _request: Request<ListProfilesRequest>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        let installation = self.installation()?;
        Ok(Response::new(ListProfilesResponse {
            profiles: Self::list(&installation),
        }))
    }

    async fn watch_profiles(
        &self,
        _request: Request<WatchProfilesRequest>,
    ) -> Result<Response<Self::WatchProfilesStream>, Status> {
        let installation = self.installation()?;
        // The sequence lock is held while every change is published, so
        // the list read under it is exactly the state before the first
        // event the receiver sees.
        let (sequence, profiles, events) = {
            let sequence = installation.sequence.lock().unwrap();
            let events = installation.events.subscribe();
            (*sequence, Self::list(&installation), events)
        };
        let opening = profiles
            .into_iter()
            .map(|info| WatchProfilesResponse {
                sequence,
                event: Some(watch_profiles_response::Event::Upserted(info)),
            })
            .chain(std::iter::once(WatchProfilesResponse {
                sequence,
                event: Some(watch_profiles_response::Event::CaughtUp(CaughtUp {
                    revision: sequence,
                })),
            }))
            .map(Ok)
            .collect::<Vec<_>>();
        let live = futures_util::stream::unfold(Some(events), |events| async move {
            let mut events = events?;
            match events.recv().await {
                Ok((sequence, event)) => {
                    let event = match event {
                        ProfileEvent::Upserted(info) => {
                            watch_profiles_response::Event::Upserted(*info)
                        }
                        ProfileEvent::Removed(id) => watch_profiles_response::Event::RemovedId(id),
                    };
                    Some((
                        Ok(WatchProfilesResponse {
                            sequence,
                            event: Some(event),
                        }),
                        Some(events),
                    ))
                }
                // A watcher that fell behind watches again for a fresh list.
                Err(RecvError::Lagged(_)) => Some((
                    Err(failed(
                        ErrorCode::Aborted,
                        "the watch fell behind; watch again",
                    )),
                    None,
                )),
                Err(RecvError::Closed) => None,
            }
        });
        Ok(Response::new(Box::pin(futures_util::StreamExt::chain(
            futures_util::stream::iter(opening),
            live,
        ))))
    }

    async fn create_profile(
        &self,
        request: Request<CreateProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let label = request
            .into_inner()
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "profile".to_owned());
        let _change = installation.registry_changes.lock().await;
        let id = installation
            .create(label.trim())
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn rename_profile(
        &self,
        request: Request<RenameProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        let label = request
            .override_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "a profile needs a name"))?;
        let _change = installation.registry_changes.lock().await;
        let mut registry = Registry::read(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        if registry
            .profiles
            .iter()
            .any(|entry| entry.id != id && entry.label == label)
        {
            return Err(failed(
                ErrorCode::AlreadyExists,
                format!("another profile is named {label}"),
            ));
        }
        let entry = registry
            .profiles
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        if request.expected_revision != 0 && request.expected_revision != entry.revision {
            return Err(failed(
                ErrorCode::Aborted,
                "the profile changed since it was read; read it again",
            ));
        }
        entry.label = label;
        entry.revision += 1;
        let entry = entry.clone();
        registry
            .write(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let info = {
            let mut hosted = installation.hosted.lock().unwrap();
            let hosted = hosted
                .get_mut(&id)
                .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
            hosted.entry = entry;
            info(hosted)
        };
        installation.publish(crate::front_door::ProfileEvent::Upserted(Box::new(
            info.clone(),
        )));
        Ok(Response::new(info))
    }

    async fn delete_profile(
        &self,
        request: Request<DeleteProfileRequest>,
    ) -> Result<Response<DeleteProfileResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        let _change = installation.registry_changes.lock().await;
        let mut registry = Registry::read(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let entry = registry
            .entry(id)
            .cloned()
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        if request.confirm_revision != entry.revision {
            return Err(failed(
                ErrorCode::Aborted,
                "the profile changed since it was read; read it again",
            ));
        }
        if registry.profiles.len() == 1 {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "an installation keeps at least one profile",
            ));
        }
        let runtime = installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(|hosted| hosted.runtime.clone());
        if runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.live().is_empty())
        {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile has running agents; stop them first",
            ));
        }
        // The registry entry goes first: a crash after it leaves a
        // directory nothing lists, never a listed profile half removed.
        registry.profiles.retain(|entry| entry.id != id);
        registry
            .write(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let hosted = installation.hosted.lock().unwrap().remove(&id);
        if let Some(hosted) = hosted {
            hosted
                .runtime
                .stop_edge(wire::LinkCloseReason::UserShutdown)
                .await;
            hosted.runtime.stop_background().await;
            hosted.runtime.stop_watching().await;
        }
        drop(runtime);
        std::fs::remove_dir_all(profile_dir(&installation.data_dir, id))
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        installation.publish(ProfileEvent::Removed(id.to_string()));
        Ok(Response::new(DeleteProfileResponse {}))
    }

    async fn bind_profile(
        &self,
        request: Request<BindProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let explicit = request.profile_id.as_deref().map(profile_id).transpose()?;
        let bound = account::bind(&installation, explicit, request).await;
        // A failure that named no profile is the installation's: its oldest
        // profile records it.
        let profile = bound.as_ref().ok().copied().or(explicit);
        if let Some(analytics) = sign_in_analytics(&installation, profile) {
            analytics.record(match &bound {
                Ok(_) => analytics::Event::SignedIn,
                Err(status) => analytics::Event::SignInFailed {
                    reason: sign_in_failure(status),
                },
            });
        }
        let id = bound?;
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn logout_profile(
        &self,
        request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let (id, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        edge.sign_out()
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        edge.analytics().record(analytics::Event::SignedOut);
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn pause_profile(
        &self,
        request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        self.set_paused(request.into_inner(), true).await
    }

    async fn resume_profile(
        &self,
        request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        self.set_paused(request.into_inner(), false).await
    }

    async fn start_pairing(
        &self,
        request: Request<ProfileStartPairingRequest>,
    ) -> Result<Response<StartPairingResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let pairing = request
            .pairing
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "the pairing request is missing"))?;
        Ok(Response::new(edge.start_pairing(pairing).await?))
    }

    async fn get_pairing_status(
        &self,
        request: Request<ProfilePairingStatusRequest>,
    ) -> Result<Response<GetPairingStatusResponse>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(GetPairingStatusResponse {
            active: edge.pairing_active(),
        }))
    }

    async fn cancel_pairing(
        &self,
        request: Request<ProfileOperation>,
    ) -> Result<Response<CancelPairingResponse>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        edge.cancel_pairing().await?;
        Ok(Response::new(CancelPairingResponse {}))
    }

    async fn begin_pair(
        &self,
        request: Request<ProfileBeginPairRequest>,
    ) -> Result<Response<PendingPairResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let pairing = request
            .pairing
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "the pairing request is missing"))?;
        Ok(Response::new(edge.begin_pair(pairing).await?))
    }

    async fn confirm_pair(
        &self,
        request: Request<ProfilePendingPairRequest>,
    ) -> Result<Response<GetPeerResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let token = request
            .pairing
            .map(|pairing| pairing.token)
            .unwrap_or_default();
        Ok(Response::new(GetPeerResponse {
            peer: Some(edge.confirm_pair(&token).await?),
        }))
    }

    async fn abandon_pair(
        &self,
        request: Request<ProfilePendingPairRequest>,
    ) -> Result<Response<PairingAbandoned>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let token = request
            .pairing
            .map(|pairing| pairing.token)
            .unwrap_or_default();
        edge.abandon_pair(&token).await?;
        Ok(Response::new(PairingAbandoned {}))
    }

    async fn get_device_identity(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(edge.device_identity()))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<ProfileTrustSshPeerRequest>,
    ) -> Result<Response<Empty>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let pairing = request
            .pairing
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "the pairing request is missing"))?;
        edge.trust_ssh_peer(pairing).await?;
        Ok(Response::new(Empty {}))
    }

    async fn list_peers(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(ListPeersResponse {
            peers: edge.list_peers()?,
        }))
    }

    async fn get_peer(
        &self,
        request: Request<ProfileGetPeerRequest>,
    ) -> Result<Response<GetPeerResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to read"))?;
        Ok(Response::new(GetPeerResponse {
            peer: Some(edge.peer_entry(peer)?),
        }))
    }

    async fn unpair(
        &self,
        request: Request<ProfileUnpairRequest>,
    ) -> Result<Response<UnpairResponse>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to unpair"))?;
        Ok(Response::new(UnpairResponse {
            removed_peer: Some(edge.unpair(peer, request.reason).await?),
        }))
    }
}

#[tonic::async_trait]
impl InstallationService for FrontDoor {
    async fn get_info(
        &self,
        _request: Request<GetInfoRequest>,
    ) -> Result<Response<InstallationInfo>, Status> {
        let installation = self.installation()?;
        Ok(Response::new(InstallationInfo {
            version: crate::version().to_owned(),
            root: installation.data_dir.to_string_lossy().into_owned(),
            front_door_path: installation
                .front_door
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<InstallationShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        self.installation()?.request_shutdown();
        Ok(Response::new(ShutdownResponse {}))
    }
}
