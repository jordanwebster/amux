//! PeerService: the calls a trusted host makes over a link. The same calls
//! and messages as the client service, answered by the same runtime; the
//! caller is the host whose pinned key the stream's handshake presented.

use std::sync::Weak;

use futures_util::stream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use wire::client_service_server::ClientService;
use wire::peer_service_server::PeerService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, DiffRequest,
    Empty, Envelope, FetchRequest, FetchResponse, GetBlobRequest, GetBlobResponse, GetRequest,
    Item, ListRepositoriesRequest, ListRepositoriesResponse, PutBlobRequest, RenameAgentRequest,
    ResumeAgentRequest, SendInputRequest, SendInputResponse, SendMessageResponse, StopAgentRequest,
    SubscribeRequest,
};

use crate::HostId;
use crate::grpc::{ClientApi, status};
use crate::runtime::ProfileRuntime;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcIo, tonic_server_builder};

/// Serves PeerService on the streams whose handshake presented a trusted
/// key, until shutdown.
pub(super) fn serve(
    runtime: Weak<ProfileRuntime>,
    incoming: mpsc::Receiver<BoxedGrpcIo>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let incoming = stream::unfold(incoming, |mut incoming| async move {
        incoming
            .recv()
            .await
            .map(|io| (Ok::<_, std::io::Error>(io), incoming))
    });
    let service = PeerApi {
        client: ClientApi::weak(runtime.clone(), None),
        runtime,
    };
    tokio::spawn(async move {
        let served = tonic_server_builder()
            .add_service(wire::peer_service_server(service))
            .serve_with_incoming_shutdown(incoming, super::wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the peer service stopped");
        }
    })
}

#[derive(Clone)]
struct PeerApi {
    runtime: Weak<ProfileRuntime>,
    client: ClientApi,
}

/// The host a call came from. Only a stream whose handshake presented a
/// trusted key reaches this service, so a call without one is a bug, and it
/// is refused rather than served as nobody.
fn caller<T>(request: &Request<T>) -> Result<HostId, Status> {
    match request.extensions().get::<BoxedGrpcConnectInfo>() {
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::TlsTrusted { peer },
        }) => Ok(*peer),
        _ => Err(Status::unauthenticated("the caller is not a trusted host")),
    }
}

#[tonic::async_trait]
impl PeerService for PeerApi {
    type SubscribeInventoryStream = <ClientApi as ClientService>::SubscribeInventoryStream;
    type SubscribeStream = <ClientApi as ClientService>::SubscribeStream;

    async fn subscribe_inventory(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        caller(&request)?;
        self.client.subscribe_inventory(request).await
    }

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        caller(&request)?;
        self.client.subscribe(request).await
    }

    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        caller(&request)?;
        self.client.fetch(request).await
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<Item>, Status> {
        caller(&request)?;
        self.client.get(request).await
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        caller(&request)?;
        self.client.send_input(request).await
    }

    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        caller(&request)?;
        self.client.create_agent(request).await
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        caller(&request)?;
        self.client.rename_agent(request).await
    }

    async fn stop_agent(
        &self,
        request: Request<StopAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        caller(&request)?;
        self.client.stop_agent(request).await
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        caller(&request)?;
        self.client.resume_agent(request).await
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        caller(&request)?;
        self.client.delete_agent(request).await
    }

    async fn send_message(
        &self,
        request: Request<Envelope>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let host = caller(&request)?;
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| Status::unavailable("the profile is no longer running"))?;
        runtime
            .send_peer_message(request.into_inner(), host)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<BlobRef>, Status> {
        caller(&request)?;
        self.client.put_blob(request).await
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        caller(&request)?;
        self.client.get_blob(request).await
    }

    async fn diff(&self, request: Request<DiffRequest>) -> Result<Response<wire::Diff>, Status> {
        caller(&request)?;
        self.client.diff(request).await
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        caller(&request)?;
        self.client.list_repositories(request).await
    }
}
