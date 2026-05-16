use std::sync::Mutex as StdMutex;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::agent_cache::AgentCache;
use super::cmd::{self, Cmd, CmdId};
use super::inventory;
use super::notification::{self, DisconnectReason, Notification, NotificationStream};
use super::session::SessionRegistry;

const NOTIFICATION_BUFFER: usize = 1024;

pub struct Runtime {
    client: amux::Client,
    notifications_tx: mpsc::Sender<Notification>,
    notifications_rx: StdMutex<Option<mpsc::Receiver<Notification>>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
    agents: AgentCache,
    sessions: SessionRegistry,
}

impl Runtime {
    /// Construct a runtime over an already-built `amux::Client`.
    pub fn start_with_client(client: amux::Client) -> Self {
        let (notifications_tx, notifications_rx) = mpsc::channel(NOTIFICATION_BUFFER);
        let agents = AgentCache::new();
        let sessions = SessionRegistry::new();

        let runtime = Self {
            client: client.clone(),
            notifications_tx: notifications_tx.clone(),
            notifications_rx: StdMutex::new(Some(notifications_rx)),
            tasks: StdMutex::new(Vec::new()),
            agents: agents.clone(),
            sessions: sessions.clone(),
        };

        runtime.push_task(tokio::spawn(inventory::run(
            client,
            notifications_tx,
            agents,
        )));
        runtime
    }

    /// Dispatch a command and receive its result through notifications.
    pub fn dispatch(&self, cmd: Cmd) -> CmdId {
        let id = cmd::new_id();
        let task = tokio::spawn(cmd::handle(
            id,
            cmd,
            self.client.clone(),
            self.notifications_tx.clone(),
            self.agents.clone(),
            self.sessions.clone(),
        ));
        self.push_task(task);
        id
    }

    /// Receive the next notification. Single-consumer.
    pub fn notifications(&self) -> NotificationStream {
        let rx = self
            .notifications_rx
            .lock()
            .expect("notification receiver mutex poisoned")
            .take()
            .unwrap_or_else(notification::closed_receiver);
        NotificationStream::new(rx)
    }

    /// Tear down runtime tasks and ask the server to shut down.
    pub async fn shutdown(self) {
        if self.client.owns_embedded_server() {
            let _ = self.client.shutdown().await;
        }

        self.sessions.stop_all().await;

        for task in self
            .tasks
            .lock()
            .expect("runtime task list mutex poisoned")
            .drain(..)
        {
            task.abort();
        }

        let _ = self
            .notifications_tx
            .send(Notification::Disconnected {
                reason: DisconnectReason::ApplicationShutdown,
            })
            .await;
    }

    fn push_task(&self, task: JoinHandle<()>) {
        self.tasks
            .lock()
            .expect("runtime task list mutex poisoned")
            .push(task);
    }
}
