//! The diff a review page is written against.

use client::{Client, RpcError};
use wire::{Diff, DiffBase, DiffRequest, GetBlobRequest, diff_base};

/// Asks the agent's host for its working-tree diff and fetches the patch
/// the diff names. The diff comes back exactly as the host froze it, so a
/// review sent with it names the same patch the page showed.
pub async fn working_tree_review(
    client: &dyn Client,
    agent_id: &[u8],
) -> Result<(Diff, String), RpcError> {
    let diff = client
        .diff(DiffRequest {
            agent_id: agent_id.to_vec(),
            base: Some(DiffBase {
                base: Some(diff_base::Base::WorkingTree(wire::Empty {})),
            }),
            with_patch: true,
        })
        .await?;
    let patch = match &diff.patch {
        Some(blob) => {
            let fetched = client
                .get_blob(GetBlobRequest {
                    agent_id: agent_id.to_vec(),
                    hash: blob.hash.clone(),
                })
                .await?;
            String::from_utf8_lossy(&fetched.bytes).into_owned()
        }
        None => String::new(),
    };
    Ok((diff, patch))
}
