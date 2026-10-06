//! The repository facts of the agent's folder, read off the event loop at
//! start and at each turn end and handed to the interpreter as an event.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Notify, mpsc};

/// Reads the facts each time `wanted` is notified, in order, so a slow read
/// is never overtaken by a later one. Notifications that arrive while a
/// read runs fold into one more read.
pub(crate) fn reader(
    cwd: PathBuf,
    wanted: Arc<Notify>,
    facts: mpsc::UnboundedSender<Option<wire::Git>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            wanted.notified().await;
            let read = match git_facts::facts(&cwd, None).await {
                Ok(read) => read.map(to_wire),
                Err(error) => {
                    // Without git the row shows no branch; nothing else
                    // depends on it.
                    eprintln!("amux agent: reading git facts: {error}");
                    None
                }
            };
            if facts.send(read).is_err() {
                return;
            }
        }
    })
}

fn to_wire(facts: git_facts::GitFacts) -> wire::Git {
    let totals = |totals: Option<git_facts::ChangeTotals>| {
        totals.map(|totals| wire::ChangeTotals {
            files: totals.files,
            added: totals.added,
            removed: totals.removed,
        })
    };
    wire::Git {
        branch: facts.branch,
        base_branch: facts.base_branch,
        uncommitted: totals(facts.uncommitted),
        on_branch: totals(facts.on_branch),
    }
}
