use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use amux::AgentParent;
use amux::derived_rows_test_support::{ClaudeSdkA2aHarness, SdkRecipientRows};
use amux::envelope::{AgentSender, Envelope, EnvelopeKind, Sender};
use anyhow::{Context as _, Result};
use claude::sdk::{QueryOptions, Session, UserMessage};
use serde_json::{Value, json};
use uuid::Uuid;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a2a")
}

async fn provider(script: &Path, role: &str, id: Uuid) -> Result<Session> {
    Ok(claude::sdk::spawn(QueryOptions {
        cli_path: Some(script.to_path_buf()),
        session_id: Some(id.to_string()),
        env: Some(HashMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "A2A_FIXTURE_DIR".into(),
                fixtures().to_string_lossy().into_owned(),
            ),
            ("A2A_ROLE".into(), role.into()),
        ])),
        ..QueryOptions::default()
    })
    .await?)
}

// The peer echoes the exact prompt it reads as a provider user row. Waiting
// for both rows proves delivery crossed stdin, regardless of ingest ordering.
async fn delivered_rows(rows: &mut SdkRecipientRows, envelope: &Envelope) -> Result<Value> {
    let mut message = None;
    let mut echoed = false;
    while message.is_none() || !echoed {
        let row = rows.next().await?;
        match row["type"].as_str() {
            Some("amux.claude_sdk.message") => {
                assert!(message.is_none(), "delivery is logged once");
                assert_eq!(row["delivery"], "stream");
                assert_eq!(row["envelope"], serde_json::to_value(envelope)?);
                message = Some(row);
            }
            Some("user") => {
                assert_eq!(row["message"]["content"], amux::envelope::format(envelope));
                echoed = true;
            }
            _ => {}
        }
    }
    Ok(message.unwrap())
}

fn lifecycle(value: Value, kind: EnvelopeKind, text: &str, parent: AgentParent) -> Envelope {
    let envelope: Envelope = serde_json::from_value(value).unwrap();
    assert_eq!(envelope.kind, kind);
    assert_eq!(envelope.text, text);
    assert_eq!(envelope.to, parent);
    assert_eq!(envelope.context, None);
    assert!(!envelope.id.is_nil());
    assert_eq!(
        envelope.from,
        Sender::Agent(AgentSender {
            agent_id: Uuid::from_u128(3),
            host_id: Uuid::from_u128(1),
            name: "child".into(),
            kind: "claude".into(),
        })
    );
    envelope
}

#[tokio::test]
async fn a2a_fixture_claude_sdk_delivery_completion_and_process_exit() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("sdk-peer");
        std::fs::copy(fixtures().join("sdk_provider.sh"), &script)?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        let host_id = Uuid::from_u128(1);
        let parent = AgentParent {
            agent_id: Uuid::from_u128(2),
            host_id,
        };
        let child_id = Uuid::from_u128(3);
        let mut host = ClaudeSdkA2aHarness::new(directory.path(), host_id)
            .await
            .context("create local fixture host")?;
        let mut parent_rows = host
            .register(
                "parent",
                None,
                provider(&script, "parent", parent.agent_id)
                    .await
                    .context("spawn parent peer")?,
            )
            .await
            .context("register parent peer")?;
        let child = provider(&script, "child", child_id)
            .await
            .context("spawn child peer")?;
        let child_control = child.control.clone();
        let mut child_rows = host.register("child", Some(parent), child).await?;
        let request = Envelope {
            id: Uuid::from_u128(10),
            context: Some(Uuid::from_u128(11)),
            from: Sender::Agent(AgentSender {
                agent_id: parent.agent_id,
                host_id,
                name: "parent".into(),
                kind: "claude".into(),
            }),
            to: AgentParent {
                agent_id: child_id,
                host_id,
            },
            kind: EnvelopeKind::Message,
            text: "Report whether <checks> & \"quotes\" survive.\nThen finish.".into(),
        };
        host.deliver(serde_json::to_value(&request)?).await?;
        let recipient = delivered_rows(&mut child_rows, &request).await?;
        let completed = lifecycle(
            host.next_envelope().await?,
            EnvelopeKind::Completed,
            "The child finishes its assigned work.",
            parent,
        );
        assert!(
            host.contains(child_id).await,
            "turn completion keeps the child alive"
        );
        assert!(child_control.process_exit().is_none());
        host.deliver(serde_json::to_value(&completed)?).await?;
        let mut completion = delivered_rows(&mut parent_rows, &completed).await?;

        child_control
            .prompt(UserMessage::text("exit fixture process"))
            .await?;
        let exited = lifecycle(
            host.next_envelope().await?,
            EnvelopeKind::Exited,
            "",
            parent,
        );
        assert_ne!(completed.id, exited.id);
        assert!(
            !host.contains(child_id).await,
            "process exit withdraws the child"
        );
        let exit = child_control
            .process_exit()
            .expect("real subprocess exit is retained");
        assert_eq!(exit.code, Some(7));
        assert!(!exit.success);
        host.deliver(serde_json::to_value(&exited)?).await?;
        let mut exit_row = delivered_rows(&mut parent_rows, &exited).await?;
        assert!(
            host.deliver(serde_json::to_value(&request)?).await.is_err(),
            "exited child rejects delivery"
        );

        // Lifecycle IDs are generated by the host; every other byte is fixed.
        completion["envelope"]["id"] = json!(Uuid::from_u128(12));
        exit_row["envelope"]["id"] = json!(Uuid::from_u128(13));
        let captures = [
            ("sdk_recipient.rows.jsonl", recipient),
            ("sdk_completed.rows.jsonl", completion),
            ("sdk_exited.rows.jsonl", exit_row),
        ];
        for (name, row) in captures {
            let actual = format!("{}\n", serde_json::to_string(&row)?);
            let path = fixtures().join(name);
            if std::env::var_os("UPDATE_A2A_FIXTURES").is_some() {
                std::fs::write(&path, &actual)?;
            }
            assert_eq!(actual, std::fs::read_to_string(path)?, "{name}");
            if let Some(output) = std::env::var_os("CLAUDE_SDK_A2A_EVIDENCE") {
                std::fs::create_dir_all(&output)?;
                std::fs::write(Path::new(&output).join(name), actual)?;
            }
        }
        host.stop().await;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
