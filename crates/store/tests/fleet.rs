use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{Duration, TimeZone, Utc};
use fold::{FleetDelta, FleetSnapshot, Generations, Membership};
use model::{
    Agent, AgentKind, AgentPhase, Attention, Capabilities, ClaudeDriver, HostEntry,
    HostTrustStatus, Progress, Summary, SummaryEnvelope, SummaryField,
};
use rusqlite::Connection;
use store::{Budget, FleetChange, Store, StoreError};
use tempfile::TempDir;

const HOST: u128 = 1;
const AGENT_X: u128 = 11;
const AGENT_Y: u128 = 12;
const AGENT_Z: u128 = 13;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn database(temp: &TempDir) -> PathBuf {
    temp.path().join("store.sqlite")
}

fn id(value: u128) -> model::AgentId {
    model::AgentId::from_u128(value)
}

fn host(name: &str, online: bool) -> HostEntry {
    HostEntry {
        id: id(HOST),
        name: name.into(),
        online,
        version: Some("1.2.3".into()),
        capabilities: Some(Capabilities::default()),
        trust_status: HostTrustStatus::Trusted,
        last_dial_error: None,
        platform: Some("macos".into()),
    }
}

fn agent(agent_id: u128, name: &str, revision: u64) -> Agent {
    Agent {
        id: id(agent_id),
        host_id: id(HOST),
        name: Some(name.into()),
        command: "claude".into(),
        working_dir: PathBuf::from("/work/tree"),
        kind: AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        },
        readonly: false,
        args: vec!["--resume".into()],
        created_at: Utc.timestamp_opt(1_700_000_000, 123_000_000).unwrap(),
        parent: None,
        working_on: None,
        summary: None,
        progress: None,
        inventory_revision: revision,
    }
}

fn summary(revision: u64, through: u64, model: &str) -> SummaryEnvelope {
    SummaryEnvelope {
        through,
        producer_version: 1,
        observed_at: Utc
            .timestamp_opt(1_700_000_100 + revision as i64, 0)
            .unwrap(),
        stale: false,
        revision,
        summary: Summary {
            attention: Attention::Working,
            phase: AgentPhase::Running,
            last_activity: None,
            todo: None,
            context: None,
            model: Some(model.into()),
            unknown: vec![SummaryField::Todo],
        },
    }
}

fn progress(revision: u64, through: u64) -> Progress {
    Progress {
        through,
        at: Utc
            .timestamp_opt(1_700_000_200 + revision as i64, 0)
            .unwrap(),
        revision,
    }
}

fn store_generations(store: &Store) -> Generations {
    store
        .generations()
        .for_provider("codex")
        .expect("provider generations")
}

fn apply(store: &Store, generations: Generations, delta: FleetDelta) -> FleetChange {
    runtime()
        .block_on(store.apply_fleet(generations, delta))
        .expect("apply fleet")
}

#[test]
fn fleet_derived_decode_failure_preserves_the_store_and_durable_view() {
    runtime().block_on(async {
        let temp = TempDir::new().expect("tempdir");
        let path = database(&temp);
        let store = Store::open(&path).await.expect("open");
        let generations = store_generations(&store);
        store
            .view_set("workspace", "selected", "durable")
            .await
            .expect("write durable view");
        store
            .apply_fleet(
                generations,
                FleetDelta::AgentUp {
                    agent: agent(AGENT_X, "x", 1),
                    revision: 1,
                },
            )
            .await
            .expect("store agent");
        store
            .apply_fleet(
                generations,
                FleetDelta::Summary {
                    host_id: id(HOST),
                    agent_id: id(AGENT_X),
                    envelope: summary(2, 1, "model"),
                },
            )
            .await
            .expect("store summary");

        let connection = Connection::open(&path).expect("raw connection");
        connection
            .execute(
                "UPDATE host_summary SET summary=X'00' WHERE agent_id=?1",
                [id(AGENT_X).to_string()],
            )
            .expect("damage derived summary");
        drop(connection);

        assert_eq!(
            store.fleet(generations).await,
            Err(StoreError::UnsupportedFormat)
        );
        assert_eq!(
            store.view_get("workspace", "selected").await,
            Ok(Some("durable".into()))
        );
        assert!(!temp.path().join("quarantine/request.json").exists());
        store.close().await;

        let reopened = Store::open(&path).await.expect("reopen without quarantine");
        assert_eq!(
            reopened.view_get("workspace", "selected").await,
            Ok(Some("durable".into()))
        );
        reopened.close().await;
    });
}

#[test]
fn fleet_orders_facts_summaries_progress_and_reachability_independently() {
    let temp = TempDir::new().expect("tempdir");
    let store = runtime()
        .block_on(Store::open(&database(&temp)))
        .expect("open");
    let generations = store_generations(&store);

    assert_eq!(
        apply(
            &store,
            generations,
            FleetDelta::Host {
                host: host("host-new", true),
                revision: 10,
            },
        ),
        FleetChange::Changed
    );
    let host_only = runtime()
        .block_on(store.fleet(generations))
        .expect("load host-only fleet");
    assert_eq!(host_only.hosts.len(), 1);
    apply(
        &store,
        generations,
        FleetDelta::Host {
            host: host("host-old", false),
            revision: 9,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "x-new", 20),
            revision: 20,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentUpdated {
            agent: agent(AGENT_X, "x-old", 19),
            revision: 19,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Summary {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            envelope: summary(30, 50, "new-model"),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Summary {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            envelope: summary(29, 80, "old-model"),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Progress {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            progress: progress(31, 60),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Progress {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            progress: progress(32, 40),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentDown {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            revision: 40,
            reason: Some("gone".into()),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "resurrected-old", 39),
            revision: 39,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Reachability {
            host_id: id(HOST),
            online: false,
        },
    );

    let fleet = runtime()
        .block_on(store.fleet(generations))
        .expect("load fleet");
    assert_eq!(fleet.hosts.len(), 1);
    assert_eq!(fleet.hosts[0].host.name, "host-new");
    assert!(!fleet.hosts[0].host.online);
    assert_eq!(fleet.hosts[0].revision, 10);
    assert_eq!(fleet.agents.len(), 1);
    let row = &fleet.agents[0];
    assert_eq!(row.agent.name.as_deref(), Some("x-new"));
    assert_eq!(row.membership, Membership::Absent);
    assert!(row.absent_since.is_some());
    assert_eq!(
        row.agent.summary.as_ref().unwrap().summary.model.as_deref(),
        Some("new-model")
    );
    assert_eq!(row.agent.summary.as_ref().unwrap().through, 50);
    assert_eq!(row.agent.progress.as_ref().unwrap().through, 60);
    assert_eq!(row.agent.progress.as_ref().unwrap().revision, 32);

    runtime().block_on(store.close());
}

#[test]
fn fleet_snapshot_confirms_membership_without_regressing_newer_values() {
    let temp = TempDir::new().expect("tempdir");
    let store = runtime()
        .block_on(Store::open(&database(&temp)))
        .expect("open");
    let generations = store_generations(&store);

    let mut current = agent(AGENT_Z, "current", 25);
    current.summary = Some(summary(26, 100, "current-model"));
    current.progress = Some(progress(27, 100));
    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: current,
            revision: 25,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentDown {
            host_id: id(HOST),
            agent_id: id(AGENT_Z),
            revision: 28,
            reason: None,
        },
    );

    let mut stale_facts = agent(AGENT_Z, "stale", 10);
    stale_facts.summary = Some(summary(24, 50, "stale-model"));
    stale_facts.progress = Some(progress(29, 90));
    apply(
        &store,
        generations,
        FleetDelta::Snapshot(FleetSnapshot {
            host_id: id(HOST),
            through_revision: 30,
            agents: vec![(stale_facts, 10)],
        }),
    );

    let fleet = runtime()
        .block_on(store.fleet(generations))
        .expect("load fleet");
    let row = fleet
        .agents
        .iter()
        .find(|row| row.agent.id == id(AGENT_Z))
        .expect("agent z");
    assert_eq!(row.membership, Membership::Cached);
    assert_eq!(row.agent.name.as_deref(), Some("current"));
    assert_eq!(row.agent.inventory_revision, 25);
    assert_eq!(
        row.agent.summary.as_ref().unwrap().summary.model.as_deref(),
        Some("current-model")
    );
    assert_eq!(row.agent.progress.as_ref().unwrap().through, 100);
    assert_eq!(row.agent.progress.as_ref().unwrap().revision, 29);

    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "cut-off", 30),
            revision: 30,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Summary {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            envelope: summary(30, 500, "cut-off"),
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::Progress {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            progress: progress(30, 500),
        },
    );
    let fleet = runtime()
        .block_on(store.fleet(generations))
        .expect("load after cutoff");
    assert!(fleet.agents.iter().all(|row| row.agent.id != id(AGENT_X)));

    runtime().block_on(store.close());
    let raw = Connection::open(database(&temp)).expect("inspect cutoff");
    let summary_count: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM host_summary WHERE agent_id=?1",
            [id(AGENT_X).to_string()],
            |row| row.get(0),
        )
        .expect("summary count");
    let progress_count: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM progress WHERE agent_id=?1",
            [id(AGENT_X).to_string()],
            |row| row.get(0),
        )
        .expect("progress count");
    assert_eq!((summary_count, progress_count), (0, 0));
}

#[test]
fn fleet_newer_snapshot_wins_in_both_cross_process_orders() {
    for order in ["old-first", "new-first"] {
        let temp = TempDir::new().expect("tempdir");
        let path = database(&temp);
        let store = runtime().block_on(Store::open(&path)).expect("open");
        let generations = store_generations(&store);
        for (id, name, revision) in [(AGENT_X, "x", 10), (AGENT_Y, "y", 11)] {
            apply(
                &store,
                generations,
                FleetDelta::AgentUp {
                    agent: agent(id, name, revision),
                    revision,
                },
            );
        }

        if order == "old-first" {
            run_snapshot_child(&path, "old");
            apply_new_snapshot(&store, generations);
        } else {
            apply_new_snapshot(&store, generations);
            run_snapshot_child(&path, "old");
        }

        let fleet = runtime()
            .block_on(store.fleet(generations))
            .expect("load fleet");
        let memberships = fleet
            .agents
            .iter()
            .map(|row| (row.agent.id, row.membership))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(memberships[&id(AGENT_X)], Membership::Absent, "{order}");
        assert_eq!(memberships[&id(AGENT_Y)], Membership::Cached, "{order}");
        runtime().block_on(store.close());
    }
}

fn apply_new_snapshot(store: &Store, generations: Generations) {
    apply(
        store,
        generations,
        FleetDelta::Snapshot(FleetSnapshot {
            host_id: id(HOST),
            through_revision: 30,
            agents: vec![(agent(AGENT_Y, "y", 11), 11)],
        }),
    );
}

fn run_snapshot_child(path: &Path, snapshot: &str) {
    let status = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--ignored",
            "--exact",
            "fleet_helper_applies_snapshot",
            "--nocapture",
        ])
        .env("AMUX_FLEET_HELPER_DB", path)
        .env("AMUX_FLEET_HELPER_SNAPSHOT", snapshot)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("snapshot child");
    assert!(status.success(), "snapshot child failed");
}

#[test]
#[ignore = "spawned by fleet_newer_snapshot_wins_in_both_cross_process_orders"]
fn fleet_helper_applies_snapshot() {
    let path = PathBuf::from(std::env::var_os("AMUX_FLEET_HELPER_DB").expect("database path"));
    let snapshot = std::env::var("AMUX_FLEET_HELPER_SNAPSHOT").expect("snapshot kind");
    let store = runtime().block_on(Store::open(&path)).expect("open helper");
    let generations = store_generations(&store);
    let (through_revision, member) = match snapshot.as_str() {
        "old" => (20, agent(AGENT_X, "x", 10)),
        _ => panic!("unknown snapshot {snapshot}"),
    };
    apply(
        &store,
        generations,
        FleetDelta::Snapshot(FleetSnapshot {
            host_id: id(HOST),
            through_revision,
            agents: vec![(member, 10)],
        }),
    );
    runtime().block_on(store.close());
}

#[test]
fn fleet_sweep_removes_absent_rows_but_keeps_the_removal_fence() {
    let temp = TempDir::new().expect("tempdir");
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).expect("open");
    let generations = store_generations(&store);
    apply(
        &store,
        generations,
        FleetDelta::Host {
            host: host("host", true),
            revision: 1,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "x", 399),
            revision: 399,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentDown {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            revision: 400,
            reason: None,
        },
    );
    runtime().block_on(store.close());

    let raw = Connection::open(&path).expect("raw database");
    raw.execute(
        "UPDATE agent SET absent_since=?2 WHERE id=?1",
        rusqlite::params![
            model::AgentId::from_u128(AGENT_X).to_string(),
            (Utc::now() - Duration::days(8)).timestamp_millis(),
        ],
    )
    .expect("age absence");
    drop(raw);

    let store = runtime().block_on(Store::open(&path)).expect("reopen");
    let generations = store_generations(&store);
    let report = runtime()
        .block_on(store.maintain(Budget::default(), std::time::Duration::from_secs(5)))
        .expect("sweep maintenance");
    assert_eq!(report.absent_agents_deleted, 1);
    assert!(
        runtime()
            .block_on(store.fleet(generations))
            .expect("fleet after sweep")
            .agents
            .is_empty()
    );

    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "old", 399),
            revision: 399,
        },
    );
    assert!(
        runtime()
            .block_on(store.fleet(generations))
            .expect("fleet after old up")
            .agents
            .is_empty()
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentUp {
            agent: agent(AGENT_X, "new", 401),
            revision: 401,
        },
    );
    apply(
        &store,
        generations,
        FleetDelta::AgentDown {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            revision: 402,
            reason: None,
        },
    );
    let first_absence = runtime()
        .block_on(store.fleet(generations))
        .expect("first absence")
        .agents[0]
        .absent_since;
    apply(
        &store,
        generations,
        FleetDelta::AgentDown {
            host_id: id(HOST),
            agent_id: id(AGENT_X),
            revision: 403,
            reason: None,
        },
    );
    let second_absence = runtime()
        .block_on(store.fleet(generations))
        .expect("second absence")
        .agents[0]
        .absent_since;
    assert_eq!(first_absence, second_absence);
    runtime().block_on(store.close());
}

#[test]
fn fleet_refuses_a_moved_generation() {
    let temp = TempDir::new().expect("tempdir");
    let store = runtime()
        .block_on(Store::open(&database(&temp)))
        .expect("open");
    let mut stale = store_generations(&store);
    stale.fleet += 1;
    assert_eq!(
        runtime().block_on(store.apply_fleet(
            stale,
            FleetDelta::Reachability {
                host_id: id(HOST),
                online: false,
            },
        )),
        Err(StoreError::GenerationMoved)
    );
    assert_eq!(
        runtime().block_on(store.fleet(stale)),
        Err(StoreError::GenerationMoved)
    );
    runtime().block_on(store.close());
}
