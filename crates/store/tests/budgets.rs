use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use fold::claude_sdk::ClaudeSdkFold;
use fold::{
    Baseline, Boundary, CommitOutcome, EntryKey, ExpectedHead, FleetDelta, Head, HeadState,
    Mutation, ProviderFold, Revision, SegmentTransition, StoreError, WindowBudget, WindowInterest,
};
use model::{Agent, AgentKind, ClaudeDriver};
use rusqlite::{Connection, params};
use store::{Budget, Store};
use tempfile::TempDir;

const AGENT: u128 = 901;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn database(temp: &TempDir) -> PathBuf {
    temp.path().join("store.sqlite")
}

fn agent_id() -> model::AgentId {
    model::AgentId::from_u128(AGENT)
}

fn now() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).unwrap()
}

fn generations(store: &Store) -> fold::Generations {
    store
        .generations()
        .for_provider("claude_sdk")
        .expect("provider generations")
}

fn head(through: u64) -> Head<ClaudeSdkFold> {
    let mut fold = ClaudeSdkFold::default();
    fold.begin(1, Baseline::Start);
    let summary = fold.summary();
    Head {
        segment: 1,
        baseline: Baseline::Start,
        through,
        tip_version: ClaudeSdkFold::TIP_VERSION,
        entry_version: ClaudeSdkFold::ENTRY_VERSION,
        tip: fold,
        summary,
        observed_at: now(),
    }
}

fn transition(through: u64) -> SegmentTransition {
    SegmentTransition {
        predecessor: None,
        successor: 1,
        baseline: Baseline::Start,
        previous_through: 0,
        selected_from: (through > 0).then_some(1),
        replay_through: through,
        opened_at: now(),
    }
}

fn resumed_head(through: u64) -> Head<ClaudeSdkFold> {
    let baseline = Baseline::Gap { after: 1 };
    let mut fold = ClaudeSdkFold::default();
    fold.begin(2, baseline);
    let summary = fold.summary();
    Head {
        segment: 2,
        baseline,
        through,
        tip_version: ClaudeSdkFold::TIP_VERSION,
        entry_version: ClaudeSdkFold::ENTRY_VERSION,
        tip: fold,
        summary,
        observed_at: now(),
    }
}

fn resumed_transition(through: u64) -> SegmentTransition {
    SegmentTransition {
        predecessor: Some(1),
        successor: 2,
        baseline: Baseline::Gap { after: 1 },
        previous_through: 1,
        selected_from: Some(2),
        replay_through: through,
        opened_at: now(),
    }
}

fn interest() -> WindowInterest {
    WindowInterest::all(0, 8 * 1024 * 1024)
}

#[test]
fn durable_view_survives_derived_rebuilds_and_reports_external_changes() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        store
            .view_set("chat", "remembered", "agent-901")
            .await
            .unwrap();
        assert_eq!(
            store
                .view_get("chat", "remembered")
                .await
                .unwrap()
                .as_deref(),
            Some("agent-901")
        );
        let before = store.data_version().await.unwrap();
        let second = Store::open(&path).await.unwrap();
        second.view_set("ui", "theme", "dark").await.unwrap();
        assert!(store.data_version().await.unwrap() > before);
        second.close().await;
        store.close().await;

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE family_shape SET shape=0 WHERE family IN
                 ('fleet','chat','claude_pty','claude_sdk','codex')",
                [],
            )
            .unwrap();
        drop(connection);
        let rebuilt = Store::open(&path).await.unwrap();
        assert_eq!(
            rebuilt
                .view_get("chat", "remembered")
                .await
                .unwrap()
                .as_deref(),
            Some("agent-901")
        );

        let host_id = model::HostId::from_u128(902);
        rebuilt
            .apply_fleet(
                generations(&rebuilt),
                FleetDelta::AgentUp {
                    agent: Agent {
                        id: agent_id(),
                        host_id,
                        name: Some("removed agent".to_owned()),
                        command: "claude".to_owned(),
                        working_dir: PathBuf::from("/tmp/removed"),
                        kind: AgentKind::Claude {
                            driver: ClaudeDriver::Sdk,
                        },
                        readonly: false,
                        args: Vec::new(),
                        created_at: now(),
                        parent: None,
                        working_on: None,
                        summary: None,
                        progress: None,
                        inventory_revision: 1,
                    },
                    revision: 1,
                },
            )
            .await
            .unwrap();
        rebuilt
            .apply_fleet(
                generations(&rebuilt),
                FleetDelta::AgentDown {
                    host_id,
                    agent_id: agent_id(),
                    revision: 2,
                    reason: Some("removed".to_owned()),
                },
            )
            .await
            .unwrap();
        let raw = Connection::open(&path).unwrap();
        raw.execute(
            "UPDATE agent SET absent_since=0 WHERE id=?1",
            [agent_id().to_string()],
        )
        .unwrap();
        drop(raw);
        let report = rebuilt
            .maintain(Budget::default(), Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(report.absent_agents_deleted, 1);
        assert_eq!(
            rebuilt
                .view_get("chat", "remembered")
                .await
                .unwrap()
                .as_deref(),
            Some("agent-901")
        );

        let raw = Connection::open(&path).unwrap();
        raw.execute(
            "INSERT INTO quarantine(id,manifest,durable_unresolved) VALUES ('lost','{}',1)",
            [],
        )
        .unwrap();
        assert_eq!(
            rebuilt.view_get("chat", "remembered").await,
            Err(StoreError::RecoveryRequired)
        );
        assert_eq!(
            rebuilt.view_set("chat", "remembered", "other").await,
            Err(StoreError::RecoveryRequired)
        );
        rebuilt.close().await;
    });
}

#[test]
fn metadata_budget_retires_aliases_and_tombstones_with_an_evicted_boundary() {
    for tombstones in [false, true] {
        runtime().block_on(async {
            let temp = TempDir::new().unwrap();
            let store = Store::open(&database(&temp)).await.unwrap();
            let count = if tombstones { 8_000 } else { 3_000 };
            let mutations = (0..count)
                .map(|index| {
                    let from =
                        EntryKey::new(format!("budget-source-{index:05}-{}", "x".repeat(32)))
                            .unwrap();
                    if tombstones {
                        Mutation::Delete {
                            key: from,
                            revision: Revision::row(1),
                        }
                    } else {
                        Mutation::Alias {
                            from,
                            to: EntryKey::new(format!(
                                "budget-target-{index:05}-{}",
                                "y".repeat(32)
                            ))
                            .unwrap(),
                            revision: Revision::row(1),
                            promote: None,
                        }
                    }
                })
                .collect();
            let outcome = store
                .commit(
                    agent_id(),
                    generations(&store),
                    ExpectedHead::Absent { fence: 0 },
                    head(1),
                    Some(transition(1)),
                    mutations,
                    interest(),
                )
                .await;
            assert!(matches!(
                outcome,
                CommitOutcome::Refused(StoreError::OverBudget)
            ));
            let loaded = store
                .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
                .await
                .unwrap();
            assert_eq!(loaded.fence, 1);
            assert!(loaded.window.is_empty());
            assert!(matches!(
                loaded.head,
                HeadState::NeedsBaseline {
                    previous_through: 1,
                    ..
                }
            ));
            assert!(
                loaded
                    .boundaries
                    .iter()
                    .any(|boundary| boundary.boundary == Boundary::Evicted)
            );
            let refused_while_reclaiming = store
                .commit(
                    agent_id(),
                    loaded.generations,
                    ExpectedHead::Absent {
                        fence: loaded.fence,
                    },
                    resumed_head(2),
                    Some(resumed_transition(2)),
                    Vec::new(),
                    interest(),
                )
                .await;
            assert!(matches!(
                refused_while_reclaiming,
                CommitOutcome::Refused(StoreError::OverBudget)
            ));
            let mut completed = false;
            for _ in 0..10 {
                let report = store
                    .maintain(Budget::default(), Duration::from_secs(10))
                    .await
                    .unwrap();
                assert!(report.retirement_rows_deleted <= 1_000);
                if report.retirements_completed == 1 {
                    completed = true;
                    break;
                }
            }
            assert!(completed, "bounded retirement did not complete");
            let resumed_mutations = [("resumed-2", 2, "two"), ("resumed-3", 3, "three")]
                .into_iter()
                .map(|(key, seq, text)| Mutation::Upsert {
                    key: EntryKey::new(key).unwrap(),
                    order: fold::Order::new(seq, 0).unwrap(),
                    revision: Revision::row(seq),
                    entry: fold::claude_sdk::ClaudeSdkPartial {
                        kind: fold::Patch::set(
                            fold::claude_sdk::ClaudeSdkEntryKind::Prompt,
                            Revision::row(seq),
                        ),
                        text: fold::Patch::set(text.to_owned(), Revision::row(seq)),
                        ..Default::default()
                    },
                })
                .collect();
            let resumed = store
                .commit(
                    agent_id(),
                    loaded.generations,
                    ExpectedHead::Absent {
                        fence: loaded.fence,
                    },
                    resumed_head(3),
                    Some(resumed_transition(3)),
                    resumed_mutations,
                    interest(),
                )
                .await;
            assert!(matches!(resumed, CommitOutcome::Committed(_)));
            let resumed_load = store
                .load::<ClaudeSdkFold>(
                    agent_id(),
                    WindowBudget {
                        max_entries: 1,
                        max_bytes: 1024 * 1024,
                        view_epoch: 0,
                    },
                )
                .await
                .unwrap();
            assert!(resumed_load.boundaries.iter().any(|boundary| {
                boundary.segment == 1 && boundary.boundary == Boundary::Evicted
            }));
            assert!(
                resumed_load.boundaries.iter().any(|boundary| {
                    boundary.segment == 2 && boundary.boundary == Boundary::Gap
                })
            );
            let page = store
                .page::<ClaudeSdkFold>(
                    agent_id(),
                    resumed_load.first_page.expect("older successor entry"),
                    10,
                )
                .await
                .unwrap();
            assert!(page.next.is_none());
            assert!(page.boundaries.iter().any(|boundary| {
                boundary.segment == 1 && boundary.boundary == Boundary::Evicted
            }));
            store.close().await;
        });
    }
}

#[test]
fn maintenance_evicts_the_oldest_page_inside_a_live_segment() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let key = EntryKey::new("seed").unwrap();
        let partial = fold::claude_sdk::ClaudeSdkPartial {
            kind: fold::Patch::set(
                fold::claude_sdk::ClaudeSdkEntryKind::Prompt,
                Revision::row(1),
            ),
            text: fold::Patch::set("seed".to_owned(), Revision::row(1)),
            ..Default::default()
        };
        let outcome = store
            .commit(
                agent_id(),
                generations(&store),
                ExpectedHead::Absent { fence: 0 },
                head(50_001),
                Some(transition(50_001)),
                vec![Mutation::Upsert {
                    key,
                    order: fold::Order::new(1, 0).unwrap(),
                    revision: Revision::row(1),
                    entry: partial,
                }],
                interest(),
            )
            .await;
        assert!(matches!(outcome, CommitOutcome::Committed(_)));

        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<=50001)
             INSERT INTO claude_sdk_entry(
                agent_id,key,segment,order_seq,order_slot,revision_seq,revision_fence,
                revision_ordinal,kind,text,bytes,body)
             SELECT agent_id,printf('seed-%05d',x),segment,x,order_slot,x,revision_fence,
                    revision_ordinal,kind,printf('seed %d',x),bytes,body
             FROM claude_sdk_entry,n WHERE key='seed';",
        )
        .unwrap();
        drop(raw);
        let report = store
            .maintain(Budget::default(), Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!(report.entries_evicted, 2);
        let loaded = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 2,
                    max_bytes: 1024 * 1024,
                    view_epoch: 0,
                },
            )
            .await
            .unwrap();
        assert!(loaded.boundaries.iter().any(|boundary| {
            boundary.boundary == Boundary::Evicted
                && boundary
                    .before
                    .as_ref()
                    .is_some_and(|(order, _)| order.seq() == 3)
        }));
        store.close().await;
    });
}

#[test]
fn dump_renders_segments_boundaries_keys_revisions_and_text() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let partial = fold::claude_sdk::ClaudeSdkPartial {
            kind: fold::Patch::set(
                fold::claude_sdk::ClaudeSdkEntryKind::Prompt,
                Revision::row(1),
            ),
            text: fold::Patch::set("readable text".to_owned(), Revision::row(1)),
            ..Default::default()
        };
        let outcome = store
            .commit(
                agent_id(),
                generations(&store),
                ExpectedHead::Absent { fence: 0 },
                head(1),
                Some(transition(1)),
                vec![Mutation::Upsert {
                    key: EntryKey::new("visible-key").unwrap(),
                    order: fold::Order::new(1, 0).unwrap(),
                    revision: Revision::row(1),
                    entry: partial,
                }],
                interest(),
            )
            .await;
        assert!(matches!(outcome, CommitOutcome::Committed(_)));
        let raw = Connection::open(&path).unwrap();
        raw.execute(
            "INSERT INTO eviction_frontier(agent_id,segment,order_seq,order_slot,key)
             VALUES (?1,1,0,0,'evicted-key')",
            [agent_id().to_string()],
        )
        .unwrap();
        drop(raw);
        let dump = store.dump(agent_id()).await.unwrap();
        println!("{dump}");
        assert!(dump.contains("segment 1 predecessor=none boundary=Start"));
        assert!(dump.contains("boundary Evicted frontier=1:0:0:evicted-key"));
        assert!(dump.contains("key=visible-key revision=1:0:0"));
        assert!(dump.contains("text=\"readable text\""));
        store.close().await;
    });
}

#[test]
fn maintenance_collapses_empty_segments_after_the_cap() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let partial = fold::claude_sdk::ClaudeSdkPartial {
            kind: fold::Patch::set(
                fold::claude_sdk::ClaudeSdkEntryKind::Prompt,
                Revision::row(1),
            ),
            text: fold::Patch::set("live entry".to_owned(), Revision::row(1)),
            ..Default::default()
        };
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations(&store),
                    ExpectedHead::Absent { fence: 0 },
                    head(1),
                    Some(transition(1)),
                    vec![Mutation::Upsert {
                        key: EntryKey::new("live-entry").unwrap(),
                        order: fold::Order::new(1, 0).unwrap(),
                        revision: Revision::row(1),
                        entry: partial,
                    }],
                    interest(),
                )
                .await,
            CommitOutcome::Committed(_)
        ));
        let raw = Connection::open(&path).unwrap();
        raw.execute(
            "UPDATE chat_state SET segment_high_water=66 WHERE agent_id=?1",
            [agent_id().to_string()],
        )
        .unwrap();
        raw.execute(
            "UPDATE segment SET first_seq=NULL,last_seq=1,closed_by=2
             WHERE agent_id=?1 AND id=1",
            [agent_id().to_string()],
        )
        .unwrap();
        for id in 2..=66i64 {
            raw.execute(
                "INSERT INTO segment(agent_id,id,predecessor,baseline_kind,baseline_seq,
                    first_seq,last_seq,closed_by,opened_at)
                 VALUES (?1,?2,?3,2,?4,NULL,?4,2,0)",
                params![agent_id().to_string(), id, id - 1, id],
            )
            .unwrap();
        }
        drop(raw);
        let report = store
            .maintain(Budget::default(), Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(report.empty_segments_collapsed, 1);
        let raw = Connection::open(&path).unwrap();
        let count: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM segment WHERE agent_id=?1",
                [agent_id().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let entry_count: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM claude_sdk_entry WHERE agent_id=?1",
                [agent_id().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let live_segment_exists: bool = raw
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM segment WHERE agent_id=?1 AND id=1)",
                [agent_id().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let oldest_kind: i64 = raw
            .query_row(
                "SELECT baseline_kind FROM segment WHERE agent_id=?1 ORDER BY id LIMIT 1",
                [agent_id().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let marker_kind: i64 = raw
            .query_row(
                "SELECT baseline_kind FROM segment WHERE agent_id=?1 AND id=3",
                [agent_id().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 65);
        assert_eq!(entry_count, 1);
        assert!(live_segment_exists);
        assert_eq!(oldest_kind, 0);
        assert_eq!(marker_kind, 4);
        drop(raw);
        let loaded = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert_eq!(loaded.window.len(), 1);
        assert_eq!(loaded.boundaries[0].segment, 3);
        assert_eq!(loaded.boundaries[0].boundary, Boundary::Evicted);
        assert!(
            loaded
                .boundaries
                .windows(2)
                .all(|pair| pair[0].segment <= pair[1].segment)
        );
        store.close().await;
    });
}

#[test]
fn large_store_opens_without_data_proportional_work_and_refuses_writes_at_reserve() {
    let temp = TempDir::new().unwrap();
    let path = database(&temp);
    let store = runtime().block_on(Store::open(&path)).unwrap();
    runtime().block_on(store.close());
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE ballast(bytes BLOB NOT NULL)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO ballast VALUES (zeroblob(?1))",
            [568 * 1024 * 1024i64],
        )
        .unwrap();
    drop(connection);

    let started = Instant::now();
    let store = runtime().block_on(Store::open(&path)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        runtime().block_on(store.view_set("ui", "selected", "agent")),
        Err(StoreError::DiskFull)
    );
    assert_eq!(
        runtime().block_on(store.maintain(Budget::default(), Duration::from_secs(30))),
        Err(StoreError::OverBudget)
    );
    runtime().block_on(store.close());
}
