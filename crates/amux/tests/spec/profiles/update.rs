//! Updating an installation restores exactly the sessions that were running.

use amux::installation::{AgentResumeStatus, OperationId, SuspendReason};
use amux::testnet::TestNet;

async fn devices() -> TestNet {
    TestNet::builder()
        .installation("laptop")
        .persistent()
        .profile("personal")
        .profile("work")
        .start()
        .await
}

#[tokio::test]
async fn suspend_and_concurrent_resume_preserve_previously_parked_agents() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let old = a.spawn_echo_agent("already-parked").await;
    a.park_agents().await;
    let active_a = a.spawn_echo_agent("personal").await;
    let active_b = b.spawn_echo_agent("work").await;
    let mut session_a = a.attach(&a, "personal").await;
    session_a.send("before-update").await;
    session_a.expect_output("before-update").await;
    let installation = laptop.front_door();
    let op = OperationId::new();
    let suspended = installation
        .suspend_all(op, SuspendReason::Update)
        .await
        .unwrap();
    assert_eq!(suspended.profiles.len(), 2);
    assert_eq!(
        suspended.profiles.iter().flat_map(|p| &p.agent_ids).count(),
        2
    );
    assert!(
        !suspended
            .profiles
            .iter()
            .any(|p| p.agent_ids.contains(&old.id))
    );
    assert_eq!(
        installation
            .suspend_all(op, SuspendReason::Update)
            .await
            .unwrap(),
        suspended
    );
    session_a.expect_disconnect().await;
    assert!(
        a.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        b.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .is_empty()
    );
    let (first, second) = tokio::join!(
        installation.resume_all(OperationId::new()),
        installation.resume_all(OperationId::new())
    );
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert_eq!(
        first
            .profiles
            .iter()
            .flat_map(|p| &p.agents)
            .filter(|a| a.status == AgentResumeStatus::Resumed)
            .count(),
        2
    );
    assert_eq!(
        a.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>(),
        vec![active_a.id]
    );
    assert_eq!(
        b.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>(),
        vec![active_b.id]
    );
    assert_eq!(a.suspended_agent_ids(), vec![old.id]);
    let mut session = a.attach(&a, "personal").await;
    installation.resume_all(OperationId::new()).await.unwrap();
    session.send("same-session-after-retry").await;
    session.expect_output("same-session-after-retry").await;
    println!(
        "Two profiles suspend two active agents, and concurrent resumes return their per-agent results. A repeated resume leaves the attached echo session alive; the previously parked agent remains on disk and never appears in either fleet."
    );
}

#[tokio::test]
async fn preparation_failure_leaves_every_agent_alive_and_reopens_admission() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let mut profiles = [laptop.profile("personal"), laptop.profile("work")];
    profiles.sort_by_key(|p| p.id);
    for profile in &profiles {
        profile.spawn_echo_agent("active").await;
    }
    let mut first = profiles[0].attach(&profiles[0], "active").await;
    let mut second = profiles[1].attach(&profiles[1], "active").await;
    let bad = profiles[1]
        .paths()
        .state_path
        .with_file_name("suspended.yaml");
    std::fs::write(&bad, "not valid suspended state").unwrap();
    assert!(
        laptop
            .front_door()
            .suspend_all(OperationId::new(), SuspendReason::Update)
            .await
            .is_err()
    );
    assert!(!laptop.root().join("update.json").exists());
    assert_eq!(
        std::fs::read_to_string(&bad).unwrap(),
        "not valid suspended state"
    );
    for session in [&mut first, &mut second] {
        session.send("still-alive").await;
        session.expect_output("still-alive").await;
    }
    profiles[0].spawn_echo_agent("after-abort").await;
    std::fs::remove_file(bad).unwrap();
    println!(
        "The second profile refuses preparation after the first profile saved its state. Both existing sessions still echo, no update journal commits, and a new agent can be created after the abort."
    );
}

#[tokio::test]
async fn creation_during_preparation_is_rejected_before_storage_and_lifecycle_waits() {
    use amux::{AgentType, CreateAgentRequest};
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    a.spawn_echo_agent("active").await;
    let client = a.socket_client().await;
    let hold = a.hold_update_preparation().await;
    let installation = laptop.front_door();
    let worker = installation.clone();
    let suspend = tokio::spawn(async move {
        worker
            .suspend_all(OperationId::new(), SuspendReason::Update)
            .await
    });
    hold.wait_until_frozen().await;
    let agent_id = uuid::Uuid::new_v4();
    let error = client
        .create_agent(CreateAgentRequest {
            agent_id,
            host_id: None,
            name: Some("too-late".into()),
            agent_type: AgentType::TestAgent {
                command: "__amux_test_echo__".into(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        })
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("update is in progress"),
        "{error}"
    );
    assert!(
        !a.paths()
            .data_dir
            .join("agents")
            .join(agent_id.to_string())
            .exists()
    );
    let worker = installation.clone();
    let create = tokio::spawn(async move {
        worker
            .create(OperationId::new(), Some("late-profile".into()))
            .await
    });
    tokio::task::yield_now().await;
    assert!(!create.is_finished());
    drop(hold);
    suspend.await.unwrap().unwrap();
    assert!(
        create
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("update is in progress")
    );
    installation.resume_all(OperationId::new()).await.unwrap();
    a.spawn_echo_agent("after-resume").await;
    println!(
        "While preparation is held open, creating an agent returns an update-in-progress error before its directory exists. Profile creation waits for the transaction and is refused until resume completes; agent creation then works again."
    );
}

#[tokio::test]
async fn restart_recovers_interrupted_replacement_without_automatically_resuming() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let old = a.spawn_echo_agent("parked").await;
    a.park_agents().await;
    a.spawn_echo_agent("personal").await;
    b.spawn_echo_agent("work").await;
    let suspend_op = OperationId::new();
    let suspended = laptop
        .front_door()
        .suspend_all(suspend_op, SuspendReason::Update)
        .await
        .unwrap();
    laptop.restart().await;
    assert!(
        a.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        b.socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .is_empty()
    );
    let installation = laptop.front_door();
    assert!(installation.pause(OperationId::new(), a.id).await.is_err());
    assert_eq!(
        installation
            .suspend_all(suspend_op, SuspendReason::Update)
            .await
            .unwrap(),
        suspended
    );
    let resumed = installation.resume_all(OperationId::new()).await.unwrap();
    assert!(resumed.profiles.iter().all(|p| p.error.is_none()
        && p.agents.len() == 1
        && p.agents[0].status == AgentResumeStatus::Resumed));
    for (profile, name) in [(&a, "personal"), (&b, "work")] {
        let mut session = profile.attach(profile, name).await;
        session.send("recovered").await;
        session.expect_output("recovered").await;
    }
    assert_eq!(a.suspended_agent_ids(), vec![old.id]);
    println!(
        "After an interrupted replacement, reopening the installation starts no saved agents. Explicit resume restores each profile's one active-before-update echo session and leaves the older parked record untouched."
    );
}

#[tokio::test]
async fn resume_retries_cleanup_without_starting_a_running_agent_twice() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    a.spawn_echo_agent("personal").await;
    b.spawn_echo_agent("work").await;
    let installation = laptop.front_door();
    installation
        .suspend_all(OperationId::new(), SuspendReason::Update)
        .await
        .unwrap();
    let path = a.paths().state_path.with_file_name("suspended.yaml");
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, "unreadable retained state").unwrap();
    let first = installation.resume_all(OperationId::new()).await.unwrap();
    assert!(
        first
            .profiles
            .iter()
            .find(|p| p.profile_id == a.id)
            .unwrap()
            .error
            .is_some()
    );
    assert!(
        first
            .profiles
            .iter()
            .flat_map(|p| &p.agents)
            .all(|a| a.status == AgentResumeStatus::Resumed)
    );
    let mut session = a.attach(&a, "personal").await;
    std::fs::write(&path, saved).unwrap();
    let retried = installation.resume_all(OperationId::new()).await.unwrap();
    assert!(retried.profiles.iter().all(|p| p.error.is_none()));
    assert!(
        retried
            .profiles
            .iter()
            .flat_map(|p| &p.agents)
            .all(|a| a.status == AgentResumeStatus::AlreadyRunning)
    );
    session.send("same-agent-after-cleanup-retry").await;
    session
        .expect_output("same-agent-after-cleanup-retry")
        .await;
    println!(
        "A resume that starts agents but cannot clean up saved state reports the profile error and retains its recovery journal. Retrying reports both agents already running and keeps the existing attachment alive."
    );
}

#[tokio::test]
async fn restart_during_incomplete_resume_restores_even_consumed_profile_records() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    a.spawn_echo_agent("personal").await;
    b.spawn_echo_agent("work").await;
    laptop
        .front_door()
        .suspend_all(OperationId::new(), SuspendReason::Update)
        .await
        .unwrap();
    let path = a.paths().state_path.with_file_name("suspended.yaml");
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, "unreadable retained state").unwrap();
    let partial = laptop
        .front_door()
        .resume_all(OperationId::new())
        .await
        .unwrap();
    assert!(partial.profiles.iter().any(|p| p.error.is_some()));
    assert!(b.suspended_agent_ids().is_empty());
    std::fs::write(&path, saved).unwrap();
    laptop.restart().await;
    let recovered = laptop
        .front_door()
        .resume_all(OperationId::new())
        .await
        .unwrap();
    assert!(recovered.profiles.iter().all(|p| p.error.is_none()
        && p.agents.len() == 1
        && p.agents[0].status == AgentResumeStatus::Resumed));
    for (profile, name) in [(&a, "personal"), (&b, "work")] {
        let mut session = profile.attach(profile, name).await;
        session.send("restored-from-journal").await;
        session.expect_output("restored-from-journal").await;
    }
    println!(
        "Restarting after a partially completed resume restores both profiles, including the profile whose suspended file was already consumed. The installation journal retains the session snapshots until the whole resume succeeds."
    );
}
