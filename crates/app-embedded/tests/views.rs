//! Two accounts on one device are two devices with independently owned
//! views. Closing one view, or changing which account is on screen, neither
//! cancels the other account's work nor routes an operation to its machine.

use std::path::Path;

use app_embedded::{Embedded, StartConfig};
use model::{AgentType, RelayConnection};
use serde_json::json;
use testnet::{Daemon, TestNet};
use tokio::sync::mpsc;
use ui_runtime::{Runtime, RuntimeOptions};
use ui_state::{Command, OpId};

/// A data root short enough for a profile's socket path, which every profile
/// allocates even though an embedded one never listens on it.
fn test_root() -> tempfile::TempDir {
    #[cfg(unix)]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let parent = std::env::temp_dir();
    tempfile::Builder::new()
        .prefix("av")
        .tempdir_in(parent)
        .expect("create a short test root")
}

fn config(root: &Path, relay: String, accounts: &[(&str, &str)], active: &str) -> StartConfig {
    serde_json::from_value(json!({
        "data_dir": root.join("data"), "cache_dir": root.join("cache"),
        "log_path": root.join("app.log"), "device_name": "phone",
        "relay": {"url": relay, "tls": "PlainLoopback"},
        "accounts": accounts.iter()
            .map(|(id, token)| json!({"id": id, "token": {"Static": token}}))
            .collect::<Vec<_>>(),
        "active": active
    }))
    .unwrap()
}

async fn connected(session: &app_runtime::Session) {
    let mut relay = session.relay.clone();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while *relay.borrow_and_update() != RelayConnection::Connected {
            relay.changed().await.unwrap();
        }
    })
    .await
    .expect("an account never reached the relay");
}

/// Pair one account with one machine, the way a device pointed at a screen
/// does, so the account can start agents there.
async fn pair(session: &app_runtime::Session, machine: &Daemon) {
    let pairing = machine
        .pairing_admin()
        .await
        .start_qr_pairing()
        .await
        .unwrap();
    let client::PairingSecret::QrSecret(secret) = &pairing.secret else {
        panic!("QR pairing returned a PIN")
    };
    session
        .admin
        .pair_link_now(node::encode_qr_pairing_payload(&pairing, secret).unwrap())
        .await
        .unwrap();
}

/// Wait for a view's reducer to have heard from its daemon; a command sent
/// before that is refused as not connected.
async fn ready(view: &mut Runtime) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !view.model().is_connected() {
            assert!(view.next_message().await, "a view's runtime ended");
        }
    })
    .await
    .expect("a view never connected to its daemon");
}

fn create(view: &mut Runtime, machine: &Daemon, name: &str, root: &Path) -> OpId {
    let op = OpId(uuid::Uuid::new_v4());
    view.dispatch_with_id(
        op,
        Command::CreateAgent {
            host: Some(machine.host_id()),
            name: name.into(),
            agent_type: AgentType::TestAgent {
                command: "cat".into(),
            },
            working_dir: root.to_owned(),
        },
    );
    op
}

async fn finished(view: &mut Runtime, op: OpId) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while view.model().finished_op(op).is_none() {
            assert!(view.next_message().await, "a view's runtime ended");
        }
    })
    .await
    .expect("a view never heard its operation finish");
    let outcome = &view.model().finished_op(op).unwrap().outcome;
    assert!(!outcome.is_error(), "{outcome:?}");
}

async fn names_on(machine: &Daemon) -> Vec<String> {
    let mut names: Vec<String> = machine
        .admin_client()
        .await
        .list_agents()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|agent| agent.name)
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_accounts_two_views_neither_cancels_nor_misroutes_the_other() {
    let net = TestNet::builder()
        .cloud()
        .daemon("home")
        .cloud_only()
        .cloud_user("personal")
        .daemon("office")
        .cloud_only()
        .cloud_user("work")
        .start()
        .await;
    let home = net.daemon("home");
    let office = net.daemon("office");
    let (_, personal) = net.user_credentials("personal");
    let (_, work) = net.user_credentials("work");
    let root = test_root();
    let config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        &[("personal", &personal), ("work", &work)],
        "personal",
    );
    let (requests, _receive) = mpsc::channel(1);
    let mut embedded = Embedded::open(&config, requests).await.unwrap();
    for session in &embedded.sessions.sessions {
        connected(session).await;
    }
    pair(&embedded.sessions.sessions[0], &home).await;
    pair(&embedded.sessions.sessions[1], &office).await;

    // Two independently owned views: the screen's own, on the personal
    // account, and a second one held directly on the work account.
    let work_session = &embedded.sessions.sessions[1];
    let mut work_view = Runtime::start_with_client(
        work_session.client.clone(),
        RuntimeOptions {
            local_host_id: Some(work_session.host),
            host_inventory: Some(work_session.inventory.clone()),
            ..Default::default()
        },
    );
    ready(&mut embedded.sessions.ui).await;
    ready(&mut work_view).await;
    let personal_op = create(
        &mut embedded.sessions.ui,
        &home,
        "personal-first",
        root.path(),
    );
    let work_op = create(&mut work_view, &office, "work-first", root.path());
    finished(&mut embedded.sessions.ui, personal_op).await;
    finished(&mut work_view, work_op).await;
    assert_eq!(names_on(&home).await, ["personal-first"]);
    assert_eq!(names_on(&office).await, ["work-first"]);

    // Closing the work view cancels its own subscriptions and nothing else:
    // the personal view keeps working against its own machine.
    drop(work_view);
    let personal_again = create(
        &mut embedded.sessions.ui,
        &home,
        "personal-second",
        root.path(),
    );
    finished(&mut embedded.sessions.ui, personal_again).await;
    assert_eq!(names_on(&home).await, ["personal-first", "personal-second"]);
    assert_eq!(names_on(&office).await, ["work-first"]);

    // Changing the screen's selection rebinds it to the work account. An
    // operation raised there lands on the work account's machine, and the
    // personal account's machine is untouched.
    embedded.sessions.switch("work").unwrap();
    ready(&mut embedded.sessions.ui).await;
    let work_again = create(
        &mut embedded.sessions.ui,
        &office,
        "work-second",
        root.path(),
    );
    finished(&mut embedded.sessions.ui, work_again).await;
    assert_eq!(names_on(&office).await, ["work-first", "work-second"]);
    assert_eq!(names_on(&home).await, ["personal-first", "personal-second"]);

    // A create aimed at the other account's machine is refused rather than
    // routed: the work account has never paired with the personal machine.
    let misrouted = create(&mut embedded.sessions.ui, &home, "misrouted", root.path());
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while embedded
            .sessions
            .ui
            .model()
            .finished_op(misrouted)
            .is_none()
        {
            assert!(embedded.sessions.ui.next_message().await);
        }
    })
    .await
    .expect("the misrouted create never finished");
    assert!(
        embedded
            .sessions
            .ui
            .model()
            .finished_op(misrouted)
            .unwrap()
            .outcome
            .is_error(),
        "an operation reached a machine the account is not paired with"
    );
    assert_eq!(names_on(&home).await, ["personal-first", "personal-second"]);

    embedded.shutdown().await;
    net.shutdown().await;
}
