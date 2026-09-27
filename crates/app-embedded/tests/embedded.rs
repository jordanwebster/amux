//! The embedded runtime against served hosts: it pairs with a desk by the
//! PIN the desk shows, holds the desk's agents as replica rows a chat reads
//! in process, switches its source policy, and signs in to an account whose
//! relay links it to the account's hosts.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use app_embedded::{EdgeOverrides, EmbeddedRuntime, PairRequest, StartConfig};
use app_runtime::values::Draft;
use app_runtime::{AppRuntime, Wake};
use client::SystemClock;
use model::AgentKey;
use node::SourcePolicy;
use provider_fakes::script::Step;
use testnet::{AgentDecl, FakeKind, HostDecl, Net, Relay, Topology};
use wire::{ProfileStartPairingRequest, StartPairingRequest, start_pairing_request};

const PATIENCE: Duration = Duration::from_secs(30);

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn worker() -> AgentDecl {
    AgentDecl::new("worker", "desk")
        .kind(FakeKind::ClaudeSdk)
        .steps(vec![
            text("turn one"),
            Step::TurnEnd,
            text("answered from the phone"),
            Step::TurnEnd,
        ])
        .prompt("go")
}

fn config(dir: &std::path::Path) -> StartConfig {
    StartConfig {
        data_dir: dir.join("data"),
        device_name: "phone".into(),
        log_path: None,
        discovery_scope: String::new(),
        lan: true,
        lan_bind: None,
        tail: 50,
    }
}

fn loopback() -> EdgeOverrides {
    EdgeOverrides {
        lan_bind: ([127, 0, 0, 1], 0).into(),
        ..EdgeOverrides::default()
    }
}

async fn app(embedded: &EmbeddedRuntime) -> AppRuntime {
    let wake = Arc::new(|_: Wake| {});
    AppRuntime::open(
        embedded.client(),
        Arc::new(SystemClock),
        embedded.host_id(),
        wake,
    )
    .await
    .unwrap()
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never saw {what}"));
}

fn worker_key(net: &Net) -> AgentKey {
    let agent = net.agent("worker").unwrap();
    AgentKey {
        host: agent.host_id.as_bytes().to_vec(),
        agent: agent.id.as_bytes().to_vec(),
    }
}

/// Reads a desk agent's chat from this device's replica rows and sends it a
/// prompt the desk's agent answers.
async fn converse(app: &AppRuntime, net: &Net) {
    let agent = worker_key(net);
    eventually("the desk's agent in the fleet", || {
        app.fleet_card(&agent).is_some()
    })
    .await;
    let chat = app.open_chat(&agent, 50).await.unwrap();
    let says = |wanted: &str| {
        let keys = chat.keys();
        chat.rows_for(&keys, &Default::default())
            .iter()
            .any(|row| format!("{:?}", row.kind).contains(wanted))
    };
    eventually("the first turn from the desk", || {
        chat.frame().caught_up && says("turn one")
    })
    .await;
    chat.send(&Draft {
        text: "hello from the phone".into(),
        attachments: Vec::new(),
    })
    .await;
    eventually("the desk's answer", || says("answered from the phone")).await;
}

/// Opens pairing mode on the desk: the PIN it shows and where it listens.
async fn pairing_pin(net: &Net) -> (String, Vec<String>) {
    let desk = net.host("desk").unwrap();
    let started = net
        .front_door("desk")
        .await
        .unwrap()
        .start_pairing(ProfileStartPairingRequest {
            operation_id: "pair".into(),
            profile_id: desk.profile.to_string(),
            pairing: Some(StartPairingRequest {
                mode: start_pairing_request::Mode::Pin as i32,
                ..StartPairingRequest::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();
    let Some(wire::start_pairing_response::Secret::Pin(pin)) = started.secret else {
        panic!("the desk shows a PIN: {started:?}");
    };
    (pin, started.addrs)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_pairs_by_pin_and_reads_the_desks_agents_from_its_own_rows() {
    let net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                lan: true,
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let embedded =
        EmbeddedRuntime::start_with(&config(dir.path()), loopback(), Arc::new(SystemClock))
            .await
            .unwrap();

    let desk = net.host("desk").unwrap();
    let (pin, addrs) = pairing_pin(&net).await;
    let wrong = embedded
        .pair(&PairRequest::Pin {
            host_id: desk.host_id.as_bytes().to_vec(),
            pin: "000000".into(),
            addrs: addrs.clone(),
        })
        .await;
    assert!(
        wrong.is_err() || pin == "000000",
        "a wrong PIN pairs: {wrong:?}"
    );
    let peer = embedded
        .pair(&PairRequest::Pin {
            host_id: desk.host_id.as_bytes().to_vec(),
            pin: pin.clone(),
            addrs: addrs.clone(),
        })
        .await
        .unwrap();
    assert_eq!(peer.name, "desk");

    let app = app(&embedded).await;
    converse(&app, &net).await;
    let hosts = app.hosts();
    assert!(
        hosts.iter().any(|host| host.name == "desk" && host.trusted),
        "{hosts:?}"
    );
    assert!(hosts.iter().any(|host| host.local && host.name == "phone"));

    // In the background only the chats asked for keep a source.
    assert_eq!(embedded.source_policy(), SourcePolicy::Listed);
    embedded.set_source_policy(SourcePolicy::OnDemand);
    assert_eq!(embedded.source_policy(), SourcePolicy::OnDemand);
    embedded.set_source_policy(SourcePolicy::Listed);

    embedded.unpair(desk.host_id.as_bytes()).await.unwrap();
    eventually("the desk to leave the trusted hosts", || {
        !app.hosts()
            .iter()
            .any(|host| host.name == "desk" && host.trusted)
    })
    .await;

    drop(app);
    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_signed_in_to_the_account_reaches_its_hosts_over_the_relay() {
    let net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(HostDecl {
                name: "desk".into(),
                account: Some("ada".into()),
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let gate = relay.gate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.lan = false;
    let embedded = EmbeddedRuntime::start_with(
        &config,
        EdgeOverrides {
            cloud: relay.cloud_options(&gate),
            ..loopback()
        },
        Arc::new(SystemClock),
    )
    .await
    .unwrap();
    let before = relay.refreshed_as().len();
    let info = embedded
        .sign_in(relay.url(), "mobile", &Relay::login("ada"))
        .await
        .unwrap();
    assert_eq!(info.account_name.is_empty(), info.email.is_empty());
    // The token refreshes as the client that obtained it.
    assert_eq!(relay.refreshed_as()[before..], ["mobile"]);
    let account = embedded.account().await.unwrap();
    assert_eq!(account.binding, app_runtime::values::Binding::SignedIn);
    assert_eq!(account.email, "ada@example.com");
    assert_eq!(embedded.access_token().await.unwrap().bearer, "access-ada");
    eventually("the relay link", || {
        matches!(
            embedded.runtime().edge().map(|edge| edge.observed()),
            Some(node::Observed::Connected { .. })
        )
    })
    .await;
    // The account lists its hosts; pairing is what makes one trusted. The
    // desk listens on no local network: the pairing goes over the relay.
    let (pin, _) = pairing_pin(&net).await;
    let desk = net.host("desk").unwrap();
    embedded
        .pair(&PairRequest::Pin {
            host_id: desk.host_id.as_bytes().to_vec(),
            pin,
            addrs: Vec::new(),
        })
        .await
        .unwrap();

    let app = app(&embedded).await;
    converse(&app, &net).await;

    drop(app);
    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}
