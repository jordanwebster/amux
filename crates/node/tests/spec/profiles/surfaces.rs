//! Profile clients have independent selections and no installation administration.
#![cfg(unix)]

use amux::installation::{FrontDoor, rpc};
use amux::testnet::{TestNet, Via};

/// A paired peer can use agent calls, but lifecycle and trust administration is absent
/// from its gRPC service. The installation owner still administers trust.
#[tokio::test]
async fn administration_is_absent_from_profile_sockets_and_peer_tunnels() {
    let net = TestNet::builder()
        .installation("desktop")
        .persistent()
        .profile("work")
        .daemon("laptop")
        .paired("laptop", "desktop/work", Via::Tcp)
        .start()
        .await;
    let desktop = net.installation("desktop").profile("work");
    let laptop = net.daemon("laptop");

    desktop.start_pairing().await;
    let agent = desktop.spawn_echo_agent("worker").await;
    laptop.can_call(&desktop).await;
    let mut session = laptop.attach(&desktop, "worker").await;
    session.send("before-admin-probes").await;
    session.expect_output("before-admin-probes").await;
    desktop.rejects_remote_admin_from(&laptop).await;
    desktop
        .rejects_admin_on_socket(desktop.paths().socket_path)
        .await;
    desktop.pair_mode_active().await;
    desktop.trusts(&laptop).await;
    desktop.allows_owner_trust_admin().await;
    assert!(
        desktop
            .socket_client()
            .await
            .list_agents()
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == agent.id)
    );
    session.send("still-serving").await;
    session.expect_output("still-serving").await;
}

/// Discovering another profile on a second connection never retargets an existing client.
#[tokio::test]
async fn two_clients_select_different_profiles_independently() {
    let net = TestNet::builder()
        .installation("desktop")
        .persistent()
        .profile("personal")
        .profile("work")
        .start()
        .await;
    let desktop = net.installation("desktop");
    let personal = desktop.profile("personal");
    let work = desktop.profile("work");
    let personal_agent = personal.spawn_echo_agent("personal-worker").await;
    let work_agent = work.spawn_echo_agent("work-worker").await;
    let front = FrontDoor::new(desktop.front_door(), None);
    let mut first = rpc::profile_service_client::ProfileServiceClient::new(front.channel());
    let mut second = rpc::profile_service_client::ProfileServiceClient::new(front.channel());
    let (a, b) = tokio::join!(
        first.list_profiles(rpc::ListProfilesRequest {}),
        second.list_profiles(rpc::ListProfilesRequest {})
    );
    let selected_a = a
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|p| p.id == personal.id.to_string())
        .unwrap();
    let selected_b = b
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|p| p.id == work.id.to_string())
        .unwrap();
    assert_ne!(selected_a.socket_path, selected_b.socket_path);
    async fn connect(socket: &str) -> amux::Client {
        amux::Server::builder()
            .config(amux::Config {
                socket_path: socket.into(),
                ..Default::default()
            })
            .daemon()
            .open()
            .await
            .unwrap()
    }
    let a = connect(&selected_a.socket_path).await;
    let b = connect(&selected_b.socket_path).await;
    for _ in 0..3 {
        let (a_rows, b_rows) = tokio::join!(a.list_agents(), b.list_agents());
        assert_eq!(
            a_rows.unwrap().iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![personal_agent.id]
        );
        assert_eq!(
            b_rows.unwrap().iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![work_agent.id]
        );
    }
    drop(b);
    assert_eq!(a.list_agents().await.unwrap()[0].id, personal_agent.id);
    println!(
        "Independent gRPC selections: {} -> personal-worker; {} -> work-worker. Closing work leaves personal serving.",
        selected_a.socket_path, selected_b.socket_path
    );
}
