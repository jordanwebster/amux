//! Tier-2 integration: the Runtime shell against a real embedded server.
//!
//! The tier-1 spec suite proves the reducer; this proves the shell edges —
//! connection task, inventory pump, RPC effects — by asserting on the Model
//! after driving a live daemon.

use std::time::Duration;

use amux::{Config, Server};
use amux_ui::{AgentPhase, Attention, Command, Model, Runtime, RuntimeOptions};
use tempfile::tempdir;

async fn wait_for(runtime: &mut Runtime, what: &str, predicate: impl Fn(&Model) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if predicate(runtime.model()) {
            return;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_else(|| panic!("timed out waiting for {what}"));
        match tokio::time::timeout(remaining, runtime.next()).await {
            Ok(true) => {}
            Ok(false) => panic!("runtime shut down while waiting for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_reflects_daemon_state_in_the_model() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let client = Server::builder()
        .config(config)
        .embedded()
        .open()
        .await
        .unwrap();

    let mut runtime = Runtime::start_with_client(client, RuntimeOptions::default());

    wait_for(&mut runtime, "snapshot synchronization", |model| {
        model.is_synchronized()
    })
    .await;
    assert!(runtime.model().host_count() >= 1, "the daemon lists itself");

    let op = runtime.dispatch(Command::CreateAgent {
        host: None,
        name: "ui-integration".to_string(),
        agent_type: amux::AgentType::TestAgent {
            command: "cat".to_string(),
        },
        working_dir: std::env::temp_dir(),
    });

    wait_for(&mut runtime, "create op to finish", move |model| {
        model.finished_op(op).is_some()
    })
    .await;
    let finished = runtime.model().finished_op(op).unwrap();
    assert!(
        !finished.outcome.is_error(),
        "create failed: {:?}",
        finished.outcome
    );

    wait_for(&mut runtime, "agent to appear via subscription", |model| {
        model
            .agents()
            .any(|card| card.agent.name.as_deref() == Some("ui-integration"))
    })
    .await;
    let card = runtime
        .model()
        .agents()
        .find(|card| card.agent.name.as_deref() == Some("ui-integration"))
        .unwrap();
    assert_eq!(card.attention, Attention::Unknown);
    assert_eq!(card.phase, AgentPhase::Running);
    let agent_id = card.agent.id;

    let delete = runtime.dispatch(Command::DeleteAgent { agent: agent_id });
    wait_for(&mut runtime, "delete op to finish", move |model| {
        model.finished_op(delete).is_some()
    })
    .await;
    wait_for(&mut runtime, "agent to disappear", move |model| {
        model.agent(agent_id).is_none()
    })
    .await;
}
