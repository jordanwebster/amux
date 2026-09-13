//! A client attached to a daemon it did not start owns its views and nothing
//! else: closing every one of them leaves the daemon serving whoever comes
//! next.

use std::sync::Arc;

use app_embedded::AdminSeat;
use app_runtime::{Link, Places, Session, Sessions};
use model::{AgentType, ProfileId, RelayCarrier, RelayConnection, Tier};
use testnet::TestNet;
use tokio::sync::watch;
use ui_state::{CloudState, Command, OpId};

/// A link nobody dials: the daemon is reached in process, so there is no
/// relay to retry.
struct NoLink;

impl Link for NoLink {
    fn retry_now(&self) {}
    fn set_active(&self, _active: bool) {}
    fn attempts(&self) -> u64 {
        0
    }
    fn shortened(&self) -> u64 {
        0
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_every_view_leaves_an_attached_daemon_serving_others() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let workstation = net.daemon("workstation");
    let seat = Arc::new(AdminSeat::new(workstation.pairing_admin().await));
    let (_connected, relay) = watch::channel(RelayConnection::Connected);
    let (_cloud, cloud) = watch::channel(CloudState::Connected {
        tier: Tier::Free,
        carrier: RelayCarrier::Tcp,
    });
    let root = tempfile::tempdir().unwrap();
    let session = Session {
        account: Some("owner".into()),
        profile: ProfileId::new(),
        host: workstation.host_id(),
        relay,
        cloud,
        link: Arc::new(NoLink),
        client: workstation.admin_client().await,
        admin: seat.clone(),
        inventory: seat,
    };
    let places = Places {
        cache_dir: root.path().join("cache"),
        report_dir: root.path().join("reports"),
        log_path: root.path().join("attach.log"),
    };
    let mut sessions = Sessions::open(vec![session], Some("owner"), places).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !sessions.ui.model().is_connected() {
            assert!(sessions.ui.next_message().await, "the view's runtime ended");
        }
    })
    .await
    .expect("the attached view never connected");

    // One view does real work against the daemon: it starts an agent there.
    let op = OpId(uuid::Uuid::new_v4());
    sessions.ui.dispatch_with_id(
        op,
        Command::CreateAgent {
            host: Some(workstation.host_id()),
            name: "attached-view-agent".into(),
            agent_type: AgentType::TestAgent {
                command: "cat".into(),
            },
            working_dir: root.path().to_owned(),
        },
    );
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while sessions.ui.model().finished_op(op).is_none() {
            assert!(sessions.ui.next_message().await, "the view's runtime ended");
        }
    })
    .await
    .expect("the attached view never heard its agent was created");
    assert!(
        !sessions
            .ui
            .model()
            .finished_op(op)
            .unwrap()
            .outcome
            .is_error(),
        "{:?}",
        sessions.ui.model().finished_op(op).unwrap().outcome
    );

    // Every view this client owns closes. Nothing here stops a daemon: only
    // an embedded owner stops an installation, and only the one it created.
    drop(sessions);

    // The daemon serves the next client exactly as before, agent included.
    let other = workstation.admin_client().await;
    let agents = other
        .list_agents()
        .await
        .expect("the daemon stopped serving");
    assert!(
        agents
            .iter()
            .any(|agent| agent.name.as_deref() == Some("attached-view-agent")),
        "the daemon lost the agent the closed view created: {agents:?}"
    );
    net.shutdown().await;
}
