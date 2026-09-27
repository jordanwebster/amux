//! Calls on another host's agents. The host that owns an agent is the only
//! one that can act on it, so a runtime asked to act on a replica makes the
//! same call on the owner's daemon over the peer link, exactly as a phone
//! would, and answers with what the owner answered. Forwarding goes one hop:
//! a call that arrived from a peer is answered only for this host's own
//! agents, never passed on.

use std::future::Future;
use std::time::Duration;

use store::{AgentKey, AgentRow, Store as _, StoreError};
use tonic::{Code, Response, Status};
use uuid::Uuid;
use wire::{
    AmbiguousHostName, DeleteAgentRequest, DeleteAgentResponse, ErrorCode, ErrorDetail, HostEntry,
    Trust,
};

use crate::HostId;
use crate::grpc::wire_error;
use crate::runtime::{ProfileRuntime, to_wire};

/// How long one forwarded call may take before its host counts as
/// unreachable. A spawn waits for the child's Hello on the far side, so
/// this covers a start as well as a round trip.
const FORWARD_PATIENCE: Duration = Duration::from_secs(30);

/// Where an agent lives, as this runtime's store knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// One of this host's own agents.
    Here,
    /// A replica: the agent lives on that host.
    Peer(HostId),
    /// No row names it.
    Unknown,
}

/// A forwarded call that did not get an answer from the owner, or got a
/// refusal.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("host {0} cannot be reached")]
    Unreachable(HostId),
    #[error("{}", .0.message)]
    Remote(wire::Error),
}

impl ForwardError {
    /// The owner's own error passes through unchanged, so a caller sees
    /// the same answer it would have had from the owner directly.
    pub fn to_wire(&self) -> wire::Error {
        match self {
            Self::Unreachable(_) => wire_error(ErrorCode::Unreachable, self.to_string()),
            Self::Remote(error) => error.clone(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Unreachable(_) => ErrorCode::Unreachable,
            Self::Remote(error) => {
                ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified)
            }
        }
    }
}

/// A spawn's host name that answers to no trusted host, or to several.
#[derive(Debug, thiserror::Error)]
pub enum HostNameError {
    #[error("no trusted host is named {name}; trusted hosts: {}", .known.join(", "))]
    NotFound { name: String, known: Vec<String> },
    #[error("{} trusted hosts answer to {}", .0.candidates.len(), .0.name)]
    Ambiguous(AmbiguousHostName),
}

impl HostNameError {
    pub fn to_wire(&self) -> wire::Error {
        match self {
            Self::NotFound { .. } => wire_error(ErrorCode::NotFound, self.to_string()),
            Self::Ambiguous(ambiguous) => {
                use prost::{Message as _, Name as _};
                wire::Error {
                    code: ErrorCode::FailedPrecondition as i32,
                    message: self.to_string(),
                    details: vec![ErrorDetail {
                        r#type: AmbiguousHostName::full_name(),
                        value: ambiguous.encode_to_vec(),
                    }],
                }
            }
        }
    }
}

/// The owner's error from a status: the wire error our daemons put in the
/// details, or one made from the code. A transport failure, which never
/// reached the owner's service, is the host being unreachable.
fn from_status(host: HostId, status: &Status) -> ForwardError {
    if let Some(error) = crate::net_error::from_status(status) {
        return ForwardError::Remote(error);
    }
    match status.code() {
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled | Code::Unknown => {
            ForwardError::Unreachable(host)
        }
        code => ForwardError::Remote(wire_error(
            match code {
                Code::NotFound => ErrorCode::NotFound,
                Code::InvalidArgument => ErrorCode::InvalidArgument,
                Code::PermissionDenied => ErrorCode::PermissionDenied,
                Code::FailedPrecondition => ErrorCode::FailedPrecondition,
                Code::AlreadyExists => ErrorCode::AlreadyExists,
                Code::Unimplemented => ErrorCode::Unimplemented,
                Code::Aborted => ErrorCode::Aborted,
                _ => ErrorCode::Internal,
            },
            status.message(),
        )),
    }
}

impl ProfileRuntime {
    /// Which host an agent id belongs to: this one's own row first, then
    /// any replica.
    pub async fn owner(&self, agent_id: &[u8]) -> Result<Owner, StoreError> {
        Ok(match self.row_by_id(agent_id).await? {
            None => Owner::Unknown,
            Some(row) if row.agent.host == self.host().as_bytes() => Owner::Here,
            Some(row) => match Uuid::from_slice(&row.agent.host) {
                Ok(host) => Owner::Peer(host),
                Err(_) => Owner::Unknown,
            },
        })
    }

    /// The row an agent id names, own or replica.
    pub(crate) async fn row_by_id(&self, agent_id: &[u8]) -> Result<Option<AgentRow>, StoreError> {
        let store = self.store.lock().await;
        let own = AgentKey::new(self.host().as_bytes().to_vec(), agent_id.to_vec());
        if let Some(row) = store.agent(&own)? {
            return Ok(Some(row));
        }
        Ok(store
            .agents()?
            .into_iter()
            .find(|row| row.agent.agent == agent_id))
    }

    /// Makes one call on `host`'s daemon, bounded by the forwarding
    /// patience.
    pub(crate) async fn on_peer<T, F, Fut>(&self, host: HostId, call: F) -> Result<T, ForwardError>
    where
        F: FnOnce(crate::PeerClient) -> Fut,
        Fut: Future<Output = Result<Response<T>, Status>>,
    {
        let edge = self.edge().ok_or(ForwardError::Unreachable(host))?;
        if !edge.is_trusted(host) {
            return Err(ForwardError::Unreachable(host));
        }
        let answer = tokio::time::timeout(FORWARD_PATIENCE, async move {
            let client = edge
                .peer(host)
                .await
                .map_err(|_| ForwardError::Unreachable(host))?;
            drop(edge);
            call(client)
                .await
                .map(Response::into_inner)
                .map_err(|status| from_status(host, &status))
        })
        .await;
        answer.unwrap_or(Err(ForwardError::Unreachable(host)))
    }

    /// The cascade's step for a child on another host: the delete is
    /// forwarded to the child's host, which deletes the child's own
    /// children in turn. A child whose host cannot be reached stays there,
    /// its parent edge pointing at nothing, listed under its own host as
    /// an orphan the person can delete; nothing is queued.
    pub(crate) async fn delete_remote_child(
        &self,
        child: AgentRow,
        response: &mut DeleteAgentResponse,
    ) {
        let wire = to_wire(&child);
        let Ok(host) = Uuid::from_slice(&child.agent.host) else {
            response.unreachable_children.push(wire);
            return;
        };
        let request = DeleteAgentRequest {
            agent_id: child.agent.agent.clone(),
        };
        match self
            .on_peer(host, |mut client| async move {
                client.delete_agent(request).await
            })
            .await
        {
            Ok(answer) => {
                response.removed_children.push(wire);
                response.removed_children.extend(answer.removed_children);
                response
                    .unreachable_children
                    .extend(answer.unreachable_children);
            }
            // Already gone on its host.
            Err(error) if error.code() == ErrorCode::NotFound => {
                response.removed_children.push(wire);
            }
            Err(error) => {
                tracing::warn!(%host, %error, "a cascaded delete could not reach the child's host");
                response.unreachable_children.push(wire);
            }
        }
    }

    /// A host by the name a person uses for it, among this host and the
    /// hosts this profile trusts: an exact match, else one ignoring case.
    pub fn resolve_host(&self, name: &str) -> Result<HostId, HostNameError> {
        // Trust as it stands now, not as last published: a host paired a
        // moment ago is a host a spawn may name.
        let published = self.published_hosts();
        let mut hosts = vec![self.host_entry()];
        for (host, name, _) in self.edge().map(|edge| edge.trusted()).unwrap_or_default() {
            let id = host.as_bytes().to_vec();
            hosts.push(
                published
                    .iter()
                    .find(|entry| entry.host_id == id)
                    .cloned()
                    .unwrap_or(HostEntry {
                        host_id: id,
                        name,
                        trust: Trust::Trusted as i32,
                        ..HostEntry::default()
                    }),
            );
        }
        let mut matching: Vec<&HostEntry> = hosts.iter().filter(|host| host.name == name).collect();
        if matching.is_empty() {
            matching = hosts
                .iter()
                .filter(|host| host.name.eq_ignore_ascii_case(name))
                .collect();
        }
        match matching.as_slice() {
            [host] => Uuid::from_slice(&host.host_id).map_err(|_| HostNameError::NotFound {
                name: name.to_owned(),
                known: Vec::new(),
            }),
            [] => Err(HostNameError::NotFound {
                name: name.to_owned(),
                known: hosts.iter().map(|host| host.name.clone()).collect(),
            }),
            several => Err(HostNameError::Ambiguous(AmbiguousHostName {
                name: name.to_owned(),
                candidates: several.iter().map(|host| (*host).clone()).collect(),
            })),
        }
    }
}
