use std::pin::Pin;

use futures_util::{Stream, stream};
use prost::Message;
use tonic::{Request, Response, Status};

use super::FrontDoor;
use super::mapping::*;
use crate::installation::{BindRequest, BindTarget};
use crate::protocol::wire;
use crate::protocol::wire::client_service_server::ClientService;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo};

type Rpc<T> = Result<Response<T>, Status>;

fn local<T>(value: T) -> Request<T> {
    let mut request = Request::new(value);
    request.extensions_mut().insert(BoxedGrpcConnectInfo {
        auth: BoxedGrpcAuth::LocalTrusted,
    });
    request
}

#[tonic::async_trait]
impl wire::profile_service_server::ProfileService for FrontDoor {
    type WatchProfilesStream =
        Pin<Box<dyn Stream<Item = Result<wire::WatchProfilesResponse, Status>> + Send>>;

    async fn list_profiles(
        &self,
        _: Request<wire::ListProfilesRequest>,
    ) -> Rpc<wire::ListProfilesResponse> {
        Ok(Response::new(wire::ListProfilesResponse {
            profiles: self
                .installation
                .profiles()
                .into_iter()
                .map(profile_info)
                .collect(),
        }))
    }
    async fn watch_profiles(
        &self,
        _: Request<wire::WatchProfilesRequest>,
    ) -> Rpc<Self::WatchProfilesStream> {
        let watch = self.installation.watch();
        Ok(Response::new(Box::pin(stream::unfold(
            watch,
            |mut watch| async move { watch.recv().await.map(|event| (watch_event(event), watch)) },
        ))))
    }
    async fn create_profile(
        &self,
        request: Request<wire::CreateProfileRequest>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "create_profile", encoded, async move {
                installation
                    .create(op, request.label)
                    .await
                    .map(profile_info)
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn bind_profile(
        &self,
        request: Request<wire::BindProfileRequest>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "bind_profile", encoded, async move {
                installation
                    .bind(
                        op,
                        BindRequest {
                            target: request
                                .profile_id
                                .as_deref()
                                .map(profile_id)
                                .transpose()?
                                .map(BindTarget::Explicit)
                                .unwrap_or(BindTarget::ByAccount),
                            cloud_url: request.cloud_url,
                            staged_refresh_token: request.staged_refresh_token,
                            adopt_non_pristine: request.adopt_non_pristine,
                        },
                    )
                    .await
                    .map(profile_info)
                    .map_err(bind_error)
            })
            .await
            .map(Response::new)
    }
    async fn logout_profile(
        &self,
        request: Request<wire::ProfileOperation>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "logout_profile", encoded, async move {
                installation
                    .logout(op, profile_id(&request.profile_id)?)
                    .await
                    .map(profile_info)
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn pause_profile(
        &self,
        request: Request<wire::ProfileOperation>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "pause_profile", encoded, async move {
                installation
                    .pause(op, profile_id(&request.profile_id)?)
                    .await
                    .map(profile_info)
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn resume_profile(
        &self,
        request: Request<wire::ProfileOperation>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "resume_profile", encoded, async move {
                installation
                    .resume(op, profile_id(&request.profile_id)?)
                    .await
                    .map(profile_info)
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn rename_profile(
        &self,
        request: Request<wire::RenameProfileRequest>,
    ) -> Rpc<wire::ProfileInfo> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "rename_profile", encoded, async move {
                installation
                    .rename(
                        op,
                        profile_id(&request.profile_id)?,
                        request.expected_revision,
                        request.override_name,
                    )
                    .await
                    .map(profile_info)
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn delete_profile(
        &self,
        request: Request<wire::DeleteProfileRequest>,
    ) -> Rpc<wire::DeleteProfileResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "delete_profile", encoded, async move {
                installation
                    .delete(
                        op,
                        profile_id(&request.profile_id)?,
                        request.confirm_revision,
                    )
                    .await
                    .map(|()| wire::DeleteProfileResponse {})
                    .map_err(installation_error)
            })
            .await
            .map(Response::new)
    }
    async fn start_pairing(
        &self,
        request: Request<wire::ProfileStartPairingRequest>,
    ) -> Rpc<wire::StartPairingResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "start_pairing", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .start_pairing(local(
                        request
                            .pairing
                            .ok_or_else(|| Status::invalid_argument("pairing is required"))?,
                    ))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn cancel_pairing(
        &self,
        request: Request<wire::ProfileOperation>,
    ) -> Rpc<wire::CancelPairingResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "cancel_pairing", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .cancel_pairing(local(wire::CancelPairingRequest {}))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn pair_peer(
        &self,
        request: Request<wire::ProfilePairPeerRequest>,
    ) -> Rpc<wire::PairPeerResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "pair_peer", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .pair_peer(local(
                        request
                            .pairing
                            .ok_or_else(|| Status::invalid_argument("pairing is required"))?,
                    ))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn pair_pin_cloud_peer(
        &self,
        request: Request<wire::ProfilePairPinCloudPeerRequest>,
    ) -> Rpc<wire::PairPinCloudPeerResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "pair_pin_cloud_peer", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .pair_pin_cloud_peer(local(
                        request
                            .pairing
                            .ok_or_else(|| Status::invalid_argument("pairing is required"))?,
                    ))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn pair_qr_cloud_peer(
        &self,
        request: Request<wire::ProfilePairQrCloudPeerRequest>,
    ) -> Rpc<wire::PairQrCloudPeerResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "pair_qr_cloud_peer", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .pair_qr_cloud_peer(local(
                        request
                            .pairing
                            .ok_or_else(|| Status::invalid_argument("pairing is required"))?,
                    ))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn unpair(
        &self,
        request: Request<wire::ProfileUnpairRequest>,
    ) -> Rpc<wire::UnpairResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let encoded = request.encode_to_vec();
        let installation = self.installation.clone();
        self.operations
            .run(op, "unpair", encoded, async move {
                let admin = installation
                    .admin_service(profile_id(&request.profile_id)?)
                    .await
                    .map_err(installation_error)?;
                admin
                    .unpair(local(wire::UnpairRequest {
                        peer: request.peer,
                        reason: request.reason,
                    }))
                    .await
                    .map(Response::into_inner)
            })
            .await
            .map(Response::new)
    }
    async fn get_pairing_status(
        &self,
        request: Request<wire::ProfilePairingStatusRequest>,
    ) -> Rpc<wire::GetPairingStatusResponse> {
        let request = request.into_inner();
        let admin = self
            .installation
            .admin_service(profile_id(&request.profile_id)?)
            .await
            .map_err(installation_error)?;
        admin
            .get_pairing_status(local(wire::GetPairingStatusRequest {}))
            .await
    }
    async fn list_peers(
        &self,
        request: Request<wire::ProfileRequest>,
    ) -> Rpc<wire::ListPeersResponse> {
        let request = request.into_inner();
        let admin = self
            .installation
            .admin_service(profile_id(&request.profile_id)?)
            .await
            .map_err(installation_error)?;
        admin.list_peers(local(wire::ListPeersRequest {})).await
    }
    async fn get_peer(
        &self,
        request: Request<wire::ProfileGetPeerRequest>,
    ) -> Rpc<wire::GetPeerResponse> {
        let request = request.into_inner();
        let admin = self
            .installation
            .admin_service(profile_id(&request.profile_id)?)
            .await
            .map_err(installation_error)?;
        admin
            .get_peer(local(wire::GetPeerRequest { peer: request.peer }))
            .await
    }
    async fn debug_profile(
        &self,
        request: Request<wire::ProfileDebugRequest>,
    ) -> Rpc<wire::DebugResponse> {
        let request = request.into_inner();
        let admin = self
            .installation
            .admin_service(profile_id(&request.profile_id)?)
            .await
            .map_err(installation_error)?;
        admin.debug(local(request.debug.unwrap_or_default())).await
    }
    async fn list_pairing_candidates(
        &self,
        request: Request<wire::ProfileRequest>,
    ) -> Rpc<wire::ListPairingCandidatesResponse> {
        let admin = self
            .installation
            .admin_service(profile_id(&request.into_inner().profile_id)?)
            .await
            .map_err(installation_error)?;
        let hosts = ClientService::list_hosts(
            &admin,
            local(wire::ListHostsRequest {
                scope: wire::list_hosts_request::Scope::PairingCandidates.into(),
            }),
        )
        .await?
        .into_inner()
        .hosts;
        Ok(Response::new(wire::ListPairingCandidatesResponse { hosts }))
    }
}

#[tonic::async_trait]
impl wire::installation_service_server::InstallationService for FrontDoor {
    async fn get_info(&self, _: Request<wire::GetInfoRequest>) -> Rpc<wire::InstallationInfo> {
        Ok(Response::new(wire::InstallationInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            root: self.installation.root().to_string_lossy().into_owned(),
            front_door_path: self
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }))
    }
    async fn suspend_all(
        &self,
        request: Request<wire::SuspendAllRequest>,
    ) -> Rpc<wire::SuspendAllResponse> {
        operation_id(&request.into_inner().operation_id)?;
        Err(Status::unimplemented(
            "installation-wide suspension is not available",
        ))
    }
    async fn resume_all(
        &self,
        request: Request<wire::ResumeAllRequest>,
    ) -> Rpc<wire::ResumeAllResponse> {
        operation_id(&request.into_inner().operation_id)?;
        Err(Status::unimplemented(
            "installation-wide resumption is not available",
        ))
    }
    async fn shutdown(
        &self,
        request: Request<wire::InstallationShutdownRequest>,
    ) -> Rpc<wire::ShutdownResponse> {
        let request = request.into_inner();
        let op = operation_id(&request.operation_id)?;
        let installation = self.installation.clone();
        self.operations
            .run(op, "shutdown", request.encode_to_vec(), async move {
                installation
                    .stop(crate::server::ShutdownReason::UserRequested)
                    .await;
                Ok(wire::ShutdownResponse {})
            })
            .await
            .map(Response::new)
    }
    async fn debug_installation(
        &self,
        request: Request<wire::DebugRequest>,
    ) -> Rpc<wire::DebugResponse> {
        let format = wire::DebugFormat::try_from(request.into_inner().format)
            .map_err(|_| Status::invalid_argument("unknown debug format"))?;
        let profiles: Vec<_> = self.installation.profiles().into_iter().map(profile_info).map(|p| serde_json::json!({
            "id": p.id, "label": p.label, "email": p.email, "host_id": p.host_id,
            "socket_path": p.socket_path, "intent": p.intent, "observed": p.observed,
            "revision": p.revision, "available": p.available, "startup_error": p.startup_error,
        })).collect();
        let dump = serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "root": self.installation.root(), "front_door_path": self.path, "profiles": profiles });
        let dump = match format {
            wire::DebugFormat::Json => {
                serde_json::to_string_pretty(&dump).map_err(|e| Status::internal(e.to_string()))?
            }
            wire::DebugFormat::Yaml | wire::DebugFormat::Unspecified => {
                serde_yaml::to_string(&dump).map_err(|e| Status::internal(e.to_string()))?
            }
        };
        Ok(Response::new(wire::DebugResponse { dump }))
    }
}
