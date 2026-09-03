//! Chapter 9 — Artifact routing, persistence & lifetime.
//!
//! Attachments use the same routed daemon surface as sessions: their bytes
//! live only with the agent owner, while metadata rows make sent attachments
//! replayable to every viewer. These boundary cases cover cross-host RPCs,
//! daemon restart, size and missing-byte failures, and owner lifetime.

use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use amux::testnet::{TestNet, Via};
use amux::{ArtifactId, ArtifactKind, ClientError, DiffBase, ProtocolError};
use amux_artifacts::ARTIFACT_SIZE_CAP;
use tempfile::TempDir;
use uuid::Uuid;

mod cross_host {
    use super::*;

    /// A remote client stores and retrieves ordinary bytes and captures a diff
    /// for an echo agent, proving these RPCs do not depend on agent kind.
    /// A pinned ref and its owner bytes then survive the owner daemon restart.
    #[tokio::test]
    async fn artifacts_and_diffs_route_to_the_owner_and_survive_its_restart() {
        let checkout = git_checkout();
        let net = TestNet::builder()
            .daemon("viewer")
            .daemon("agent-host")
            .paired("viewer", "agent-host", Via::Tcp)
            .start()
            .await;
        let [viewer, agent_host] = net.daemons(["viewer", "agent-host"]);

        let echo = agent_host
            .spawn_echo_agent_in("echo-worker", checkout.path())
            .await;
        viewer.sees_agent_on(&agent_host, "echo-worker").await;

        let note_bytes = b"bytes stored on the agent host".to_vec();
        let note = viewer
            .put_artifact_on(
                &agent_host,
                &echo,
                ArtifactKind::File,
                "notes.txt",
                "text/plain",
                note_bytes.clone(),
            )
            .await
            .expect("remote artifact put succeeds");
        let (fetched_note, fetched_bytes) = viewer
            .get_artifact_on(&agent_host, &echo, &note.id)
            .await
            .expect("remote artifact get succeeds");
        assert_eq!(fetched_note, note);
        assert_eq!(fetched_bytes, note_bytes);

        let diff = viewer
            .diff_on(&agent_host, &echo, DiffBase::WorkingTree)
            .await
            .expect("remote diff succeeds for an echo agent");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "message.txt");
        assert!(diff.patch.contains("+after"));
        let (diff_ref, diff_bytes) = viewer
            .get_artifact_on(&agent_host, &echo, &diff.artifact.id)
            .await
            .expect("frozen diff bytes are remotely fetchable");
        assert_eq!(diff_ref, diff.artifact);
        assert_eq!(diff_bytes, diff.patch.as_bytes());

        let stable_id = Uuid::new_v4();
        let claude = agent_host
            .register_scripted_claude_agent(stable_id, "durable", checkout.path())
            .await;
        viewer.sees_agent_on(&agent_host, "durable").await;
        let image_bytes = b"deterministic image payload".to_vec();
        let image = viewer
            .put_artifact_on(
                &agent_host,
                &claude,
                ArtifactKind::Image,
                "screen.png",
                "image/png",
                image_bytes.clone(),
            )
            .await
            .expect("remote image put succeeds");
        let refs = viewer
            .send_pinned_claude_prompt_on(
                &agent_host,
                &claude,
                &format!(
                    "inspect <amux-attachment id=\"{}\" kind=\"image\" name=\"screen.png\"/>",
                    image.id
                ),
                vec![image.id.clone()],
            )
            .await;
        assert_eq!(refs, vec![image.clone()]);

        agent_host.restart().await;
        net.establish_direct(&viewer, &agent_host).await;
        let claude = agent_host
            .register_scripted_claude_agent(stable_id, "durable", checkout.path())
            .await;
        viewer.sees_agent_on(&agent_host, "durable").await;

        assert_eq!(
            viewer.replayed_artifacts_on(&agent_host, &claude).await,
            vec![image.clone()]
        );
        let (restarted_ref, restarted_bytes) = viewer
            .get_artifact_on(&agent_host, &claude, &image.id)
            .await
            .expect("artifact bytes survive the owner restart");
        assert_eq!(restarted_ref, image);
        assert_eq!(restarted_bytes, image_bytes);
    }

    /// Failures retain their structured protocol identity across a routed
    /// client boundary, so callers can keep a draft and explain the remedy.
    #[tokio::test]
    async fn oversized_put_and_missing_send_return_typed_errors() {
        let net = TestNet::builder()
            .daemon("viewer")
            .daemon("agent-host")
            .paired("viewer", "agent-host", Via::Tcp)
            .start()
            .await;
        let [viewer, agent_host] = net.daemons(["viewer", "agent-host"]);
        let echo = agent_host.spawn_echo_agent("echo-worker").await;
        viewer.sees_agent_on(&agent_host, "echo-worker").await;

        let oversized = vec![0_u8; usize::try_from(ARTIFACT_SIZE_CAP + 1).unwrap()];
        let error = viewer
            .put_artifact_on(
                &agent_host,
                &echo,
                ArtifactKind::File,
                "too-large.bin",
                "application/octet-stream",
                oversized,
            )
            .await
            .expect_err("oversized artifact is rejected");
        assert!(matches!(
            error,
            ClientError::Protocol(ProtocolError::AttachmentTooLarge { size, max })
                if size == ARTIFACT_SIZE_CAP + 1 && max == ARTIFACT_SIZE_CAP
        ));

        let missing = ArtifactId::from_str(&format!("sha256:{}", "0".repeat(64))).unwrap();
        let error = viewer
            .send_echo_with_pins_on(&agent_host, &echo, "keep this draft", vec![missing.clone()])
            .await
            .expect_err("a send referencing absent bytes is rejected");
        assert!(matches!(
            error,
            ClientError::Protocol(ProtocolError::AttachmentMissing { id })
                if id == missing.to_string()
        ));
    }
}

mod lifetime {
    use super::*;

    /// Unsent artifacts expire against an injected clock; a diff pinned by a
    /// sent review does not, and deleting the agent removes the whole store.
    #[tokio::test]
    async fn only_ephemeral_artifacts_are_swept_and_agent_delete_removes_pins() {
        let net = TestNet::builder().daemon("agent-host").start().await;
        let agent_host = net.daemon("agent-host");
        let agent = agent_host
            .register_scripted_claude_agent(Uuid::new_v4(), "reviewer", std::env::temp_dir())
            .await;

        let abandoned = agent_host
            .put_artifact_on(
                &agent_host,
                &agent,
                ArtifactKind::Diff,
                "abandoned.diff",
                "text/x-diff",
                b"abandoned patch".to_vec(),
            )
            .await
            .expect("abandoned diff stores");
        agent_host.advance_artifact_time(Duration::from_secs(2 * 60 * 60));
        assert_eq!(
            agent_host.sweep_artifacts().await,
            vec![abandoned.id.clone()]
        );
        assert!(matches!(
            agent_host
                .get_artifact_on(&agent_host, &agent, &abandoned.id)
                .await,
            Err(ClientError::Protocol(ProtocolError::AttachmentMissing { id }))
                if id == abandoned.id.to_string()
        ));

        let pinned = agent_host
            .put_artifact_on(
                &agent_host,
                &agent,
                ArtifactKind::Diff,
                "review.diff",
                "text/x-diff",
                b"review patch".to_vec(),
            )
            .await
            .expect("review diff stores");
        let refs = agent_host
            .send_pinned_claude_prompt_on(
                &agent_host,
                &agent,
                &format!(
                    "review <amux-attachment kind=\"review\" diff=\"{}\" base=\"working-tree\">comment</amux-attachment>",
                    pinned.id
                ),
                vec![pinned.id.clone()],
            )
            .await;
        assert_eq!(refs, vec![pinned.clone()]);

        agent_host.advance_artifact_time(Duration::from_secs(2 * 60 * 60));
        assert!(agent_host.sweep_artifacts().await.is_empty());
        assert_eq!(
            agent_host
                .get_artifact_on(&agent_host, &agent, &pinned.id)
                .await
                .expect("pinned diff survives sweep")
                .0,
            pinned
        );

        assert!(agent_host.artifact_root_exists(agent.id));
        agent_host
            .delete_agent_on(&agent_host, &agent)
            .await
            .expect("agent delete succeeds");
        assert!(!agent_host.artifact_root_exists(agent.id));
        assert!(
            agent_host
                .get_artifact_on(&agent_host, &agent, &pinned.id)
                .await
                .is_err()
        );
    }
}

fn git_checkout() -> TempDir {
    let checkout = tempfile::tempdir().expect("create checkout");
    git(checkout.path(), &["init", "-q"]);
    git(
        checkout.path(),
        &["config", "user.email", "spec@example.com"],
    );
    git(checkout.path(), &["config", "user.name", "Spec"]);
    std::fs::write(checkout.path().join("message.txt"), "before\n").expect("write baseline");
    git(checkout.path(), &["add", "message.txt"]);
    git(checkout.path(), &["commit", "-qm", "baseline"]);
    std::fs::write(checkout.path().join("message.txt"), "after\n").expect("write change");
    checkout
}

fn git(working_dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(working_dir)
        .status()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(status.success(), "git {args:?} failed with {status}");
}
