//! GetCatalogue: what an agent offers, read from the file its process wrote
//! under the hash its snapshot names. A paired host forwards a call for its
//! peer's agent to that peer and keeps a copy, so it answers again with the
//! peer away; a hash that changes mid-session is read anew.

mod support;

use std::path::{Path, PathBuf};

use prost::Message as _;
use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::client_service_server::ClientService as _;
use wire::{
    Catalogue, GetCatalogueRequest, HostProvider, OfferedModel, Phase, SessionEvent,
    get_catalogue_request, session_event,
};

const SEGMENTS: u64 = 1 << 20;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn offering(model: &str) -> Catalogue {
    let mut catalogue = Catalogue {
        models: vec![OfferedModel {
            value: model.to_owned(),
            display_name: model.to_owned(),
            ..OfferedModel::default()
        }],
        ..Catalogue::default()
    };
    catalogue.hash = node::catalogue_hash(&catalogue);
    catalogue
}

/// What the agent process does: the catalogue's bytes, then the snapshot
/// that names them.
fn offer(agent: &mut SyntheticAgent, catalogue: &Catalogue, at_ms: i64) {
    let unhashed = Catalogue {
        hash: Vec::new(),
        ..catalogue.clone()
    };
    let dir = agent.dir.join(agent_dir::CATALOGUES);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(hex(&catalogue.hash)), unhashed.encode_to_vec()).unwrap();
    let mut step = snapshot(Phase::Idle, &[], at_ms);
    step.snapshot.as_mut().unwrap().catalogue = Some(catalogue.hash.clone());
    agent.append(&step);
}

fn by_agent(agent: &SyntheticAgent) -> tonic::Request<GetCatalogueRequest> {
    tonic::Request::new(GetCatalogueRequest {
        of: Some(get_catalogue_request::Of::AgentId(
            agent.id.as_bytes().to_vec(),
        )),
    })
}

fn names(event: &SessionEvent, hash: &[u8]) -> bool {
    matches!(
        &event.of,
        Some(session_event::Of::Snapshot(snapshot)) if snapshot.catalogue.as_deref() == Some(hash)
    )
}

fn kept_copy(laptop: &Install, desk: uuid::Uuid, agent: uuid::Uuid, hash: &[u8]) -> PathBuf {
    laptop
        .profile_dir()
        .join(node::REPLICAS)
        .join(desk.to_string())
        .join(node::AGENTS)
        .join(agent.to_string())
        .join(agent_dir::CATALOGUES)
        .join(hex(hash))
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_host_reads_its_peers_catalogue_keeps_a_copy_and_follows_a_new_hash() {
    let desk = Install::new();
    let mut agent = SyntheticAgent::new(&desk, "coder", SEGMENTS);
    agent.register_offline(&desk);
    let first = offering("gpt-1");
    offer(&mut agent, &first, 1_000);
    agent.go_live();
    let desk_daemon = desk.start("boot-1", quiet_launch()).await;
    let desk_runtime = runtime(&desk_daemon, &desk);

    // The agent's own host reads the file its snapshot names.
    let on_desk = node::ClientApi::new(&desk_runtime, None);
    let read = on_desk
        .get_catalogue(by_agent(&agent))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, first);

    let laptop = Install::new();
    let laptop_daemon = laptop
        .start("boot-1", laptop.launch("idle", Vec::new()))
        .await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(
        laptop_edge
            .wait_for_route(desk_runtime.host(), PATIENCE)
            .await
    );

    // The laptop follows the agent as a chat on it would.
    let mut subscription = until("the laptop to list the desk's agent", || {
        laptop_runtime.subscribe(agent.id.as_bytes(), 10)
    })
    .await
    .unwrap();
    let mut seen = Vec::new();
    read_until(
        &mut subscription,
        &mut seen,
        "the first catalogue",
        |seen| seen.iter().any(|event| names(event, &first.hash)),
    )
    .await;
    let on_laptop = node::ClientApi::new(&laptop_runtime, None);
    let read = on_laptop
        .get_catalogue(by_agent(&agent))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, first, "the desk answers through the laptop");
    let desk_host = desk_runtime.host();
    assert!(
        is_file(&kept_copy(&laptop, desk_host, agent.id, &first.hash)),
        "the laptop keeps what it read"
    );

    // Mid-session the agent offers something else: the new hash is read
    // anew, and the old copy stays.
    let second = offering("gpt-2");
    offer(&mut agent, &second, 2_000);
    agent.nudge().await;
    read_until(
        &mut subscription,
        &mut seen,
        "the second catalogue",
        |seen| seen.iter().any(|event| names(event, &second.hash)),
    )
    .await;
    let read = on_laptop
        .get_catalogue(by_agent(&agent))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, second);
    assert!(is_file(&kept_copy(
        &laptop,
        desk_host,
        agent.id,
        &second.hash
    )));
    assert!(is_file(&kept_copy(
        &laptop,
        desk_host,
        agent.id,
        &first.hash
    )));

    // With the desk away the laptop still answers from its copy.
    drop(link);
    let read = on_laptop
        .get_catalogue(by_agent(&agent))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, second, "the copy answers with the desk away");

    drop(subscription);
    agent.die().await;
    drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_has_offered_nothing_is_not_found() {
    let install = Install::new();
    let agent = SyntheticAgent::new(&install, "coder", SEGMENTS);
    agent.register_offline(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let api = node::ClientApi::new(&runtime, None);

    let refused = api.get_catalogue(by_agent(&agent)).await.unwrap_err();
    assert_eq!(refused.code(), Code::NotFound, "{refused:?}");
    let unknown = api
        .get_catalogue(tonic::Request::new(GetCatalogueRequest {
            of: Some(get_catalogue_request::Of::AgentId(
                uuid::Uuid::new_v4().as_bytes().to_vec(),
            )),
        }))
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), Code::NotFound, "{unknown:?}");
    let empty = api
        .get_catalogue(tonic::Request::new(GetCatalogueRequest { of: None }))
        .await
        .unwrap_err();
    assert_eq!(empty.code(), Code::InvalidArgument, "{empty:?}");
    let host = api
        .get_catalogue(tonic::Request::new(GetCatalogueRequest {
            of: Some(get_catalogue_request::Of::Host(HostProvider {
                host_id: runtime.host().as_bytes().to_vec(),
                provider: "codex".to_owned(),
            })),
        }))
        .await
        .unwrap_err();
    assert_eq!(host.code(), Code::Unimplemented, "{host:?}");

    drop((api, runtime));
    daemon.shutdown().await.unwrap();
}
