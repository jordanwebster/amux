//! Shares replay results across local connections, including pairing secrets.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use prost::Message;
use tokio::sync::watch;
use tonic::Status;

use crate::installation::OperationId;

type ResultBytes = Result<Vec<u8>, Status>;
#[derive(Default)]
pub(crate) struct Ledger(Mutex<State>);
#[derive(Default)]
struct State {
    entries: HashMap<OperationId, Entry>,
    completed: VecDeque<OperationId>,
}
struct Entry {
    method: &'static str,
    request: Vec<u8>,
    result: watch::Receiver<Option<ResultBytes>>,
}
impl Ledger {
    pub(super) async fn run<T: Message + Default>(
        self: &Arc<Self>,
        id: OperationId,
        method: &'static str,
        request: Vec<u8>,
        execute: impl Future<Output = Result<T, Status>> + Send + 'static,
    ) -> Result<T, Status> {
        let mut result = {
            let mut state = self.0.lock().unwrap();
            if let Some(entry) = state.entries.get(&id) {
                if entry.method != method || entry.request != request {
                    return Err(Status::failed_precondition(
                        "operation id reused for another request",
                    ));
                }
                entry.result.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                state.entries.insert(
                    id,
                    Entry {
                        method,
                        request,
                        result: receiver.clone(),
                    },
                );
                let ledger = self.clone();
                // Disconnecting a caller must not cancel an accepted mutation.
                tokio::spawn(async move {
                    let result = execute.await.map(|message| message.encode_to_vec());
                    sender.send_replace(Some(result));
                    let mut state = ledger.0.lock().unwrap();
                    state.completed.push_back(id);
                    while state.completed.len() > 256 {
                        let oldest = state.completed.pop_front().unwrap();
                        state.entries.remove(&oldest);
                    }
                });
                receiver
            }
        };
        loop {
            if let Some(outcome) = result.borrow_and_update().clone() {
                return T::decode(outcome?.as_slice())
                    .map_err(|_| Status::internal("invalid stored operation result"));
            }
            result
                .changed()
                .await
                .map_err(|_| Status::internal("operation ended without a result"))?;
        }
    }
}
