use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::agent_cache::AgentCache;
use super::cmd::{self, CmdId, CmdResult};
use super::error::{AmuxError, protocol_failure_reason, session_failure_reason};
use super::notification::{self, Notification, SessionPhase};
use super::types;

#[derive(Clone)]
pub(crate) struct SessionRegistry {
    sessions: Arc<Mutex<HashMap<types::AgentId, SessionSubscription>>>,
}

impl SessionRegistry {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn stop_all(&self) {
        let sessions: Vec<_> = self.sessions.lock().await.drain().map(|(_, s)| s).collect();
        for session in sessions {
            stop(session).await;
        }
    }
}

pub(crate) struct AttachRequest {
    pub(crate) cmd_id: CmdId,
    pub(crate) id: types::AgentId,
    pub(crate) io_protocol: String,
    pub(crate) args: Option<Bytes>,
}

struct SessionSubscription {
    refcount: usize,
    cancel: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

pub(crate) async fn attach(
    request: AttachRequest,
    client: amux::Client,
    tx: mpsc::Sender<Notification>,
    agents: AgentCache,
    sessions: SessionRegistry,
) {
    let mut sessions_guard = sessions.sessions.lock().await;
    if let Some(session) = sessions_guard.get_mut(&request.id) {
        session.refcount += 1;
        drop(sessions_guard);
        cmd::completed(&tx, request.cmd_id, CmdResult::AttachSession).await;
        return;
    }

    let Some(entry) = agents.find_or_fetch(&client, request.id).await else {
        drop(sessions_guard);
        cmd::failed_error(
            &tx,
            request.cmd_id,
            AmuxError::Protocol("agent not found".into()),
        )
        .await;
        return;
    };

    let session = match client
        .subscribe_session(amux::SubscribeSessionRequest {
            id: request.id,
            route: entry.route,
            io_protocol: request.io_protocol,
            args: request.args,
        })
        .await
    {
        Ok(session) => session,
        Err(error) => {
            drop(sessions_guard);
            notification::send(
                &tx,
                Notification::SessionFailed {
                    id: request.id,
                    reason: session_failure_reason(&error),
                },
            )
            .await;
            cmd::failed(&tx, request.cmd_id, error).await;
            return;
        }
    };

    notification::send(&tx, Notification::SessionOpened(request.id)).await;
    let session_tx = tx.clone();
    let id = request.id;
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        run(id, session, session_tx, cancel_rx).await;
    });
    sessions_guard.insert(
        request.id,
        SessionSubscription {
            refcount: 1,
            cancel: Some(cancel_tx),
            task,
        },
    );
    drop(sessions_guard);
    cmd::completed(&tx, request.cmd_id, CmdResult::AttachSession).await;
}

pub(crate) async fn detach(
    cmd_id: CmdId,
    id: types::AgentId,
    tx: mpsc::Sender<Notification>,
    sessions: SessionRegistry,
) {
    let mut sessions_guard = sessions.sessions.lock().await;
    let mut removed = None;
    if let Some(session) = sessions_guard.get_mut(&id) {
        session.refcount = session.refcount.saturating_sub(1);
        if session.refcount == 0 {
            removed = sessions_guard.remove(&id);
        }
    }
    drop(sessions_guard);

    if let Some(session) = removed {
        stop(session).await;
    }
    cmd::completed(&tx, cmd_id, CmdResult::DetachSession).await;
}

async fn stop(mut session: SessionSubscription) {
    if let Some(cancel) = session.cancel.take() {
        let _ = cancel.send(());
    }
    match tokio::time::timeout(Duration::from_millis(250), &mut session.task).await {
        Ok(_) => {}
        Err(_) => session.task.abort(),
    }
}

async fn run(
    id: types::AgentId,
    session: amux::SessionStream,
    tx: mpsc::Sender<Notification>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    let mut phase = SessionPhase::Replay;
    loop {
        let frame = tokio::select! {
            _ = &mut cancel_rx => {
                let _ = session.cancel().await;
                return;
            }
            frame = session.recv() => frame,
        };
        match frame {
            Ok(amux::protocol::session::SubscribeSessionFrame::Event(
                amux::protocol::session::SubscribeSessionEvent::Output { payload },
            )) => {
                notification::send_session_output(&tx, id, Bytes::from(payload), phase).await;
            }
            Ok(amux::protocol::session::SubscribeSessionFrame::Event(
                amux::protocol::session::SubscribeSessionEvent::ReplayComplete { .. },
            )) => {
                phase = SessionPhase::Live;
                notification::send(&tx, Notification::SessionReplayComplete(id)).await;
            }
            Ok(amux::protocol::session::SubscribeSessionFrame::Event(
                amux::protocol::session::SubscribeSessionEvent::Opened,
            )) => {}
            Ok(amux::protocol::session::SubscribeSessionFrame::Response(Ok(()))) => return,
            Ok(amux::protocol::session::SubscribeSessionFrame::Response(Err(error))) => {
                notification::send(
                    &tx,
                    Notification::SessionFailed {
                        id,
                        reason: protocol_failure_reason(&error),
                    },
                )
                .await;
                return;
            }
            Err(error) => {
                notification::send(
                    &tx,
                    Notification::SessionFailed {
                        id,
                        reason: session_failure_reason(&error),
                    },
                )
                .await;
                return;
            }
        }
    }
}
