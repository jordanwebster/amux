use bytes::Bytes;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::agent_cache::{AgentCache, InsertOutcome};
use super::error::AmuxError;
use super::notification::{self, Notification};
use super::session::{self, SessionRegistry};
use super::types;

pub enum Cmd {
    CreateAgent(types::CreateAgentRequest),
    RenameAgent {
        id: types::AgentId,
        name: String,
    },
    DeleteAgent(types::AgentId),
    SendInput {
        id: types::AgentId,
        io_protocol: String,
        payload: Bytes,
    },
    AttachSession {
        id: types::AgentId,
        io_protocol: String,
        args: Option<Bytes>,
    },
    DetachSession(types::AgentId),
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct CmdId(Uuid);

#[derive(Clone, Debug)]
pub enum CmdResult {
    CreateAgent(types::Agent),
    RenameAgent(types::Agent),
    DeleteAgent,
    SendInput,
    AttachSession,
    DetachSession,
}

pub(crate) fn new_id() -> CmdId {
    CmdId(Uuid::new_v4())
}

pub(crate) async fn handle(
    cmd_id: CmdId,
    cmd: Cmd,
    client: amux::Client,
    tx: mpsc::Sender<Notification>,
    agents: AgentCache,
    sessions: SessionRegistry,
) {
    match cmd {
        Cmd::CreateAgent(request) => match client.create_agent(request).await {
            Ok(entry) => {
                let agent = entry.agent.clone();
                // Optimistic: insert into the cache and emit AgentAdded
                // immediately. The inventory subscription will deliver an
                // AgentUp for the same id moments later; the cache returns
                // `Same` and the duplicate notification is suppressed.
                emit_inventory_change(&tx, &agents, entry).await;
                completed(&tx, cmd_id, CmdResult::CreateAgent(agent)).await;
            }
            Err(error) => failed(&tx, cmd_id, error).await,
        },
        Cmd::RenameAgent { id, name } => {
            if agents.find_or_fetch(&client, id).await.is_none() {
                failed_error(&tx, cmd_id, AmuxError::Protocol("agent not found".into())).await;
                return;
            }
            match client.rename_agent(id, name).await {
                Ok(entry) => {
                    let agent = entry.agent.clone();
                    emit_inventory_change(&tx, &agents, entry).await;
                    completed(&tx, cmd_id, CmdResult::RenameAgent(agent)).await;
                }
                Err(error) => failed(&tx, cmd_id, error).await,
            }
        }
        Cmd::DeleteAgent(id) => {
            if agents.find_or_fetch(&client, id).await.is_none() {
                failed_error(&tx, cmd_id, AmuxError::Protocol("agent not found".into())).await;
                return;
            }
            match client.delete_agent(id).await {
                Ok(()) => {
                    if agents.remove(id).await {
                        notification::send(&tx, Notification::AgentRemoved { id, reason: None })
                            .await;
                    }
                    completed(&tx, cmd_id, CmdResult::DeleteAgent).await;
                }
                Err(error) => failed(&tx, cmd_id, error).await,
            }
        }
        Cmd::SendInput {
            id,
            io_protocol,
            payload,
        } => {
            let Some(entry) = agents.find_or_fetch(&client, id).await else {
                failed_error(&tx, cmd_id, AmuxError::Protocol("agent not found".into())).await;
                return;
            };
            let result = client
                .send_input(amux::SendInputRequest {
                    id,
                    route: entry.route,
                    io_protocol,
                    payload,
                })
                .await;
            match result {
                Ok(()) => completed(&tx, cmd_id, CmdResult::SendInput).await,
                Err(error) => failed(&tx, cmd_id, error).await,
            }
        }
        Cmd::AttachSession {
            id,
            io_protocol,
            args,
        } => {
            session::attach(
                session::AttachRequest {
                    cmd_id,
                    id,
                    io_protocol,
                    args,
                },
                client,
                tx,
                agents,
                sessions,
            )
            .await;
        }
        Cmd::DetachSession(id) => {
            session::detach(cmd_id, id, tx, sessions).await;
        }
    }
}

/// Insert an `AgentEntry` into the cache and emit the appropriate
/// notification based on the diff outcome. Used by command handlers that
/// receive a fresh `AgentEntry` from an RPC response and want immediate
/// feedback before the inventory subscription delivers the same event.
async fn emit_inventory_change(
    tx: &mpsc::Sender<Notification>,
    agents: &AgentCache,
    entry: types::AgentEntry,
) {
    match agents.insert_with_outcome(entry).await {
        InsertOutcome::Added(agent) => {
            notification::send(tx, Notification::AgentAdded(agent)).await;
        }
        InsertOutcome::Updated(agent) => {
            notification::send(tx, Notification::AgentUpdated(agent)).await;
        }
        InsertOutcome::Same => {}
    }
}

pub(crate) async fn completed(tx: &mpsc::Sender<Notification>, id: CmdId, result: CmdResult) {
    notification::send(tx, Notification::CommandCompleted { id, result }).await;
}

pub(crate) async fn failed(tx: &mpsc::Sender<Notification>, id: CmdId, error: amux::ClientError) {
    failed_error(tx, id, AmuxError::from(error)).await;
}

pub(crate) async fn failed_error(tx: &mpsc::Sender<Notification>, id: CmdId, error: AmuxError) {
    notification::send(tx, Notification::CommandFailed { id, error }).await;
}
