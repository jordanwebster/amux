//! Own retention: keeps the profile's own rows under their budget.
//!
//! The store decides what goes (exited agents whole, least recently active
//! first, never an exited child whose parent is live; then the largest live
//! agent trimmed a chunk at a time, never below its newest rows) and says
//! which agents it removed. The daemon then does what the store cannot:
//! ends their streams, tells the fleet they are gone and deletes their
//! directories, which takes their blobs with them. The sweep runs when the
//! background work starts and then on its interval, on the runtime's clock.

use std::io;

use store::{Store as _, Sweep};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{AgentRemoved, inventory_event};

use crate::runtime::{ProfileRuntime, RegistryError};
use crate::serve::inventory;

/// What the fleet is told when retention removes an agent.
pub const REMOVED_BY_RETENTION: &str = "removed by retention";

/// The retention sweeps run so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// How many sweeps have finished.
    pub runs: u64,
    /// What the last one did.
    pub last: Option<Sweep>,
}

impl ProfileRuntime {
    /// Runs one own retention sweep under the current budget.
    pub async fn sweep_retention(&self) -> Result<Sweep, RegistryError> {
        let launch = self.launch();
        let sweep = {
            let mut store = self.store.lock().await;
            let sweep = store.sweep_own(
                launch.own_budget_bytes,
                launch.retention_chunk_bytes,
                launch.tail_rows,
            )?;
            for key in &sweep.removed {
                self.fanout.close(key);
                self.fanout
                    .publish_inventory(inventory(inventory_event::Of::AgentRemoved(
                        AgentRemoved {
                            host_id: key.host.clone(),
                            agent_id: key.agent.clone(),
                            reason: Some(REMOVED_BY_RETENTION.to_owned()),
                        },
                    )));
            }
            sweep
        };
        for key in &sweep.removed {
            let Ok(id) = Uuid::from_slice(&key.agent) else {
                continue;
            };
            // Under the agent's operation lock, and only if no resume put
            // its row back in the meantime: a running process keeps its
            // directory. A directory left behind by a crash here has no
            // row, and the next start removes it.
            let operation = self.operation(id);
            let _operation = operation.lock().await;
            if self.store.lock().await.agent(key)?.is_some() {
                continue;
            }
            match std::fs::remove_dir_all(self.agent_dir(id)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.retention.send_modify(|retention| {
            retention.runs += 1;
            retention.last = Some(sweep.clone());
        });
        Ok(sweep)
    }

    /// The sweeps run so far, and each one as it finishes.
    pub fn retention(&self) -> tokio::sync::watch::Receiver<Retention> {
        self.retention.subscribe()
    }

    pub(crate) fn start_retention(&self) -> JoinHandle<()> {
        let runtime = self.me.clone();
        tokio::spawn(async move {
            loop {
                let Some(me) = runtime.upgrade() else { return };
                if let Err(error) = me.sweep_retention().await {
                    tracing::warn!(%error, "the retention sweep failed");
                }
                let clock = me.clock().clone();
                let next = clock.now_ms() + me.launch().retention_interval_ms;
                drop(me);
                clock.sleep_until(next).await;
            }
        })
    }
}
