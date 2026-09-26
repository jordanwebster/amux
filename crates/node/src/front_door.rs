//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, and manages the installation.
//!
//! Profile calls that need the network edge — binding to an account,
//! pairing, peers and the device identity — answer unimplemented until
//! that edge is served again.

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
    Intent, ListPeersResponse, ListProfilesRequest, ListProfilesResponse, Observed,
    PairingAbandoned, PendingPairResponse, ProfileBeginPairRequest, ProfileGetPeerRequest,
    ProfileInfo, ProfileOperation, ProfilePairingStatusRequest, ProfilePendingPairRequest,
    ProfileRequest, ProfileStartPairingRequest, ProfileTrustSshPeerRequest, ProfileUnpairRequest,
    RenameProfileRequest, ShutdownResponse, StartPairingResponse, UnpairResponse,
    WatchProfilesRequest, WatchProfilesResponse, watch_profiles_response,
};

use crate::daemon::{Hosted, Installation};
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
    ProfileInfo {
        id: hosted.entry.id.to_string(),
        label: hosted.entry.label.clone(),
        socket_path: hosted
            .runtime
            .dir()
            .join(PROFILE_SOCKET)
            .to_string_lossy()
            .into_owned(),
        host_id: hosted.runtime.host().to_string(),
        intent: Intent::Unbound as i32,
        observed: Observed::Local as i32,
        revision: hosted.entry.revision,
        available: true,
        ..ProfileInfo::default()
    }
}

/// Serves the profile and installation services on the front door until
/// the task is aborted.
pub(crate) fn serve(listener: LocalListener, installation: Weak<Installation>) -> JoinHandle<()> {
    let door = FrontDoor { installation };
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

#[derive(Clone)]
struct FrontDoor {
    installation: Weak<Installation>,
}

fn failed(code: ErrorCode, message: impl Into<String>) -> Status {
    status(wire_error(code, message))
}

fn not_yet(what: &str) -> Status {
    failed(
        ErrorCode::Unimplemented,
        format!("{what} is not available in this build"),
    )
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
        _request: Request<BindProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        Err(not_yet("signing a profile in to an account"))
    }

    async fn logout_profile(
        &self,
        _request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        Err(not_yet("signing a profile out"))
    }

    async fn pause_profile(
        &self,
        _request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        Err(not_yet("pausing a profile's connection"))
    }

    async fn resume_profile(
        &self,
        _request: Request<ProfileOperation>,
    ) -> Result<Response<ProfileInfo>, Status> {
        Err(not_yet("resuming a profile's connection"))
    }

    async fn start_pairing(
        &self,
        _request: Request<ProfileStartPairingRequest>,
    ) -> Result<Response<StartPairingResponse>, Status> {
        Err(not_yet("pairing"))
    }

    async fn get_pairing_status(
        &self,
        _request: Request<ProfilePairingStatusRequest>,
    ) -> Result<Response<GetPairingStatusResponse>, Status> {
        Err(not_yet("pairing"))
    }

    async fn cancel_pairing(
        &self,
        _request: Request<ProfileOperation>,
    ) -> Result<Response<CancelPairingResponse>, Status> {
        Err(not_yet("pairing"))
    }

    async fn begin_pair(
        &self,
        _request: Request<ProfileBeginPairRequest>,
    ) -> Result<Response<PendingPairResponse>, Status> {
        Err(not_yet("pairing"))
    }

    async fn confirm_pair(
        &self,
        _request: Request<ProfilePendingPairRequest>,
    ) -> Result<Response<GetPeerResponse>, Status> {
        Err(not_yet("pairing"))
    }

    async fn abandon_pair(
        &self,
        _request: Request<ProfilePendingPairRequest>,
    ) -> Result<Response<PairingAbandoned>, Status> {
        Err(not_yet("pairing"))
    }

    async fn get_device_identity(
        &self,
        _request: Request<ProfileRequest>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        Err(not_yet("the device identity"))
    }

    async fn trust_ssh_peer(
        &self,
        _request: Request<ProfileTrustSshPeerRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(not_yet("pairing over SSH"))
    }

    async fn list_peers(
        &self,
        _request: Request<ProfileRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        Err(not_yet("listing peers"))
    }

    async fn get_peer(
        &self,
        _request: Request<ProfileGetPeerRequest>,
    ) -> Result<Response<GetPeerResponse>, Status> {
        Err(not_yet("reading a peer"))
    }

    async fn unpair(
        &self,
        _request: Request<ProfileUnpairRequest>,
    ) -> Result<Response<UnpairResponse>, Status> {
        Err(not_yet("unpairing"))
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
            version: crate::VERSION.to_owned(),
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
