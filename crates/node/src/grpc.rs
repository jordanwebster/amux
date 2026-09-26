//! The client service over a local socket: one profile's socket for
//! clients, and each agent's tools socket for its MCP server, where the
//! socket is the caller's identity.
//!
//! Every call is a thin mapping onto the profile runtime. The service holds
//! the runtime weakly: a connection that outlives its runtime (a restart in
//! process, a deleted profile) is answered unavailable, never kept alive.

use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use agent_dir::local_socket::{LocalListener, LocalStream};
use futures_util::Stream;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::JoinHandle;
use tonic::{Code, Request, Response, Status};
use uuid::Uuid;
use wire::client_service_server::ClientService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, DiffRequest,
    DumpRequest, DumpResponse, Empty, Envelope, ErrorCode, FetchRequest, FetchResponse,
    GetBlobRequest, GetBlobResponse, GetRequest, InventoryEvent, Item, ListRepositoriesRequest,
    ListRepositoriesResponse, PutBlobRequest, RenameAgentRequest, ResolveAgentRequest,
    ResumeAgentRequest, SendInputRequest, SendInputResponse, SendMessageResponse, SessionEvent,
    StopAgentRequest, StopMode, SubscribeRequest, subscribe_request,
};

use crate::runtime::{AgentId, ProfileRuntime, RegistryError};

/// A wire error as a gRPC status: the coarse code for generic clients, and
/// the whole error, details included, in the status details for ours.
pub fn status(error: wire::Error) -> Status {
    let code = match ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified) {
        ErrorCode::Unspecified | ErrorCode::Internal => Code::Internal,
        ErrorCode::Cancelled => Code::Cancelled,
        ErrorCode::InvalidArgument => Code::InvalidArgument,
        ErrorCode::NotFound => Code::NotFound,
        ErrorCode::AlreadyExists => Code::AlreadyExists,
        ErrorCode::PermissionDenied | ErrorCode::PaymentRequired => Code::PermissionDenied,
        ErrorCode::Unauthenticated => Code::Unauthenticated,
        // Unavailable means this daemon is not answering, which a caller
        // retries; another host being unreachable is an answer.
        ErrorCode::FailedPrecondition | ErrorCode::Unreachable => Code::FailedPrecondition,
        ErrorCode::Aborted => Code::Aborted,
        ErrorCode::ResourceExhausted => Code::ResourceExhausted,
        ErrorCode::Unimplemented => Code::Unimplemented,
        ErrorCode::Unavailable => Code::Unavailable,
        ErrorCode::DataLoss => Code::DataLoss,
    };
    Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
}

pub(crate) fn wire_error(code: ErrorCode, message: impl Into<String>) -> wire::Error {
    wire::Error {
        code: code as i32,
        message: message.into(),
        details: Vec::new(),
    }
}

/// The client service for one profile. `caller` is set on an agent's
/// tools socket: that agent is the parent of what it spawns and the sender
/// of what it sends.
#[derive(Clone)]
pub struct ClientApi {
    runtime: Weak<ProfileRuntime>,
    caller: Option<AgentId>,
}

impl ClientApi {
    pub fn new(runtime: &Arc<ProfileRuntime>, caller: Option<AgentId>) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            caller,
        }
    }

    pub(crate) fn weak(runtime: Weak<ProfileRuntime>, caller: Option<AgentId>) -> Self {
        Self { runtime, caller }
    }

    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        self.runtime
            .upgrade()
            .ok_or_else(|| Status::unavailable("the profile is no longer running"))
    }

    /// Refuses a call an agent's tool set never makes. An agent manages
    /// other agents only by messaging them and interrupting its children;
    /// renaming, stopping, resuming, deleting, dumping and writing blobs are
    /// a person's acts, taken on the profile socket.
    fn people_only(&self, call: &str) -> Result<(), Status> {
        match self.caller {
            Some(_) => Err(status(wire_error(
                ErrorCode::PermissionDenied,
                format!("an agent cannot {call}"),
            ))),
            None => Ok(()),
        }
    }

    /// On an agent's tools socket, an input goes only to the caller's own
    /// direct child: the stop tool interrupts a child, and nothing else an
    /// agent does sends a person's input.
    async fn lineage(&self, runtime: &ProfileRuntime, target: &[u8]) -> Result<(), Status> {
        let Some(caller) = self.caller else {
            return Ok(());
        };
        let target = match runtime.agent(agent_id(target)?).await {
            Ok(target) => Some(target),
            Err(RegistryError::NotFound(_)) => None,
            Err(error) => return Err(status(error.to_wire())),
        };
        let own_child = target
            .and_then(|target| target.parent)
            .is_some_and(|parent| {
                parent.host_id == runtime.host().as_bytes() && parent.agent_id == caller.as_bytes()
            });
        if own_child {
            return Ok(());
        }
        Err(status(wire_error(
            ErrorCode::PermissionDenied,
            "an agent sends input only to its own children",
        )))
    }
}

fn agent_id(bytes: &[u8]) -> Result<AgentId, Status> {
    Uuid::from_slice(bytes).map_err(|_| {
        status(wire_error(
            ErrorCode::InvalidArgument,
            format!("an agent id is 16 bytes, not {}", bytes.len()),
        ))
    })
}

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl ClientService for ClientApi {
    type SubscribeInventoryStream = EventStream<InventoryEvent>;
    type SubscribeStream = EventStream<SessionEvent>;

    async fn subscribe_inventory(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        let subscription = self
            .runtime()?
            .subscribe_inventory()
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(InventoryEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn resolve_agent(
        &self,
        request: Request<ResolveAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        self.runtime()?
            .resolve_agent(&request.into_inner().name)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let request = request.into_inner();
        let tail = match request.from {
            Some(subscribe_request::From::Tail(tail)) => tail,
            None => 0,
            Some(subscribe_request::From::After(_)) => {
                return Err(status(wire_error(
                    ErrorCode::Unimplemented,
                    "subscribing after a revision is how a peer catches up; clients ask for a tail",
                )));
            }
        };
        let subscription = self
            .runtime()?
            .subscribe(&request.agent_id, tail)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(SessionEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        self.runtime()?
            .fetch(&request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<Item>, Status> {
        self.runtime()?
            .get(&request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        self.lineage(&runtime, &request.agent_id).await?;
        runtime
            .send_input(&request)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        self.runtime()?
            .spawn(request.into_inner(), self.caller)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        self.people_only("rename an agent")?;
        let request = request.into_inner();
        self.runtime()?
            .rename(agent_id(&request.agent_id)?, &request.name)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn stop_agent(
        &self,
        request: Request<StopAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.people_only("stop an agent")?;
        let request = request.into_inner();
        let mode = StopMode::try_from(request.mode).unwrap_or(StopMode::Graceful);
        self.runtime()?
            .stop(agent_id(&request.agent_id)?, mode)
            .await
            .map(|_| Response::new(Empty {}))
            .map_err(|error| status(error.to_wire()))
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        self.people_only("resume an agent")?;
        let request = request.into_inner();
        self.runtime()?
            .resume(agent_id(&request.agent_id)?, request.initial_prompt)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        self.people_only("delete an agent")?;
        let request = request.into_inner();
        self.runtime()?
            .delete(agent_id(&request.agent_id)?)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_message(
        &self,
        request: Request<Envelope>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        self.runtime()?
            .send_message(request.into_inner(), self.caller)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<BlobRef>, Status> {
        self.people_only("store a blob")?;
        self.runtime()?
            .put_blob(request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        self.runtime()?
            .get_blob(request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn diff(&self, request: Request<DiffRequest>) -> Result<Response<wire::Diff>, Status> {
        self.people_only("read a diff")?;
        self.runtime()?
            .diff(request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn list_repositories(
        &self,
        _request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        Err(status(wire_error(
            ErrorCode::Unimplemented,
            "listing repositories is not available yet",
        )))
    }

    async fn dump(&self, request: Request<DumpRequest>) -> Result<Response<DumpResponse>, Status> {
        self.people_only("write a debug report")?;
        // A local caller reads the bundle where it was written; the bytes
        // travel only to a caller on another machine.
        let path = self
            .runtime()?
            .dump(request.into_inner())
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(DumpResponse {
            report_path: path.to_string_lossy().into_owned(),
            bundle: Vec::new(),
        }))
    }
}

/// A connection accepted on a local socket, as tonic serves it.
pub(crate) struct Connection(LocalStream);

impl tonic::transport::server::Connected for Connection {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Every connection the listener accepts, until it fails.
pub(crate) fn incoming(
    listener: LocalListener,
) -> impl Stream<Item = std::io::Result<Connection>> + Send + 'static {
    futures_util::stream::unfold(Some(listener), |listener| async move {
        let mut listener = listener?;
        match listener.accept().await {
            Ok(stream) => Some((Ok(Connection(stream)), Some(listener))),
            Err(error) => Some((Err(error), None)),
        }
    })
}

/// Serves the client service on a listener until the task is aborted,
/// which drops the listener and removes its socket.
pub(crate) fn serve_client(listener: LocalListener, api: ClientApi) -> JoinHandle<()> {
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .add_service(wire::client_service_server(api))
            .serve_with_incoming(incoming(listener))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "a client socket stopped serving");
        }
    })
}
