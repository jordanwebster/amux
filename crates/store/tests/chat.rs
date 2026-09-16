use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use fold::claude_sdk::{ClaudeSdkEntryKind, ClaudeSdkFold, ClaudeSdkPartial};
use fold::{
    Baseline, BaselineReason, CommitOutcome, EntryKey, ExpectedHead, Head, HeadState, Input,
    Mutation, MutationOracle, Order, Patch, ProviderFold, Revision, SegmentTransition, StoreError,
    WindowBudget, WindowInterest,
};
use model::{
    Agent, AgentKind, AgentPhase, Attention, ClaudeDriver, Progress, Summary, SummaryEnvelope,
};
use rusqlite::Connection;
use store::Store;
use tempfile::TempDir;

const AGENT: u128 = 101;

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

fn now(second: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + second, 0).unwrap()
}

fn summary() -> Summary {
    Summary {
        attention: Attention::Working,
        phase: AgentPhase::Running,
        last_activity: Some(now(0)),
        todo: None,
        context: None,
        model: Some("claude-test".into()),
        unknown: Vec::new(),
    }
}

fn head(
    fold: ClaudeSdkFold,
    segment: u32,
    baseline: Baseline,
    through: u64,
) -> Head<ClaudeSdkFold> {
    let summary = fold.summary();
    Head {
        segment,
        baseline,
        through,
        tip_version: ClaudeSdkFold::TIP_VERSION,
        entry_version: ClaudeSdkFold::ENTRY_VERSION,
        tip: fold,
        summary,
        observed_at: now(through as i64),
    }
}

fn transition(
    predecessor: Option<u32>,
    successor: u32,
    baseline: Baseline,
    previous_through: u64,
    selected_from: Option<u64>,
    replay_through: u64,
) -> SegmentTransition {
    SegmentTransition {
        predecessor,
        successor,
        baseline,
        previous_through,
        selected_from,
        replay_through,
        opened_at: now(replay_through as i64),
    }
}

fn prompt(id: &str, text: &str) -> Vec<u8> {
    format!(r#"{{"type":"user","uuid":"{id}","message":{{"content":"{text}"}}}}"#).into_bytes()
}

fn fold_rows(
    fold: &mut ClaudeSdkFold,
    rows: &[(u64, &str, &str)],
) -> Vec<Mutation<fold::claude_sdk::ClaudeSdkEntry>> {
    let mut mutations = Vec::new();
    for (seq, id, text) in rows {
        let payload = prompt(id, text);
        let changes = fold.apply(Input::Row {
            seq: *seq,
            published_at: now(*seq as i64),
            activity_at: Some(now(*seq as i64)),
            historical: false,
            payload: &payload,
        });
        mutations.extend(changes.mutations);
    }
    mutations
}

fn store_generations(store: &Store) -> fold::Generations {
    store
        .generations()
        .for_provider("claude_sdk")
        .expect("provider generation")
}

fn all_interest() -> WindowInterest {
    WindowInterest::all(7, 8 * 1024 * 1024)
}

fn wait_for(path: &Path, timeout: Duration) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn seed_standing(store: &Store, generations: fold::Generations) {
    let host_id = model::HostId::from_u128(1);
    let agent = Agent {
        id: agent_id(),
        host_id,
        name: Some("chat agent".into()),
        command: "claude".into(),
        working_dir: PathBuf::from("/work/tree"),
        kind: AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        },
        readonly: false,
        args: Vec::new(),
        created_at: now(0),
        parent: None,
        working_on: None,
        summary: None,
        progress: None,
        inventory_revision: 1,
    };
    store
        .apply_fleet(
            generations,
            fold::FleetDelta::AgentUp { agent, revision: 1 },
        )
        .await
        .unwrap();
    store
        .apply_fleet(
            generations,
            fold::FleetDelta::Summary {
                host_id,
                agent_id: agent_id(),
                envelope: SummaryEnvelope {
                    through: 1,
                    producer_version: 1,
                    observed_at: now(1),
                    stale: false,
                    revision: 2,
                    summary: summary(),
                },
            },
        )
        .await
        .unwrap();
    store
        .apply_fleet(
            generations,
            fold::FleetDelta::Progress {
                host_id,
                agent_id: agent_id(),
                progress: Progress {
                    through: 1,
                    at: now(1),
                    revision: 3,
                },
            },
        )
        .await
        .unwrap();
}

#[test]
fn chat_writer_conflict_reload_and_invalidation_are_fenced() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let second = Store::open(&path).await.unwrap();
        let generations = store_generations(&store);
        let second_generations = store_generations(&second);
        seed_standing(&store, generations).await;
        let empty = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(7))
            .await
            .unwrap();
        assert!(matches!(empty.head, HeadState::None));
        assert_eq!(empty.fence, 0);
        assert_eq!(empty.host.as_ref().map(|value| value.through), Some(1));
        assert_eq!(empty.progress.as_ref().map(|value| value.through), Some(1));
        let second_empty = second
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(7))
            .await
            .unwrap();
        assert!(matches!(second_empty.head, HeadState::None));

        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let first = fold_rows(&mut fold, &[(1, "u1", "one")]);
        let committed = store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 1),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 1)),
                first,
                all_interest(),
            )
            .await;
        let first_expected = match committed {
            CommitOutcome::Committed(result) => {
                assert_eq!(result.placed.len(), 1);
                assert_eq!(result.bodies.len(), 1);
                result.expected
            }
            _ => panic!("first commit was not accepted"),
        };

        let stale = second
            .commit(
                agent_id(),
                second_generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 1),
                None,
                Vec::new(),
                all_interest(),
            )
            .await;
        match stale {
            CommitOutcome::Conflict(loaded) => {
                assert!(matches!(loaded.head, HeadState::Usable(1, _)));
                assert_eq!(loaded.window.len(), 1);
            }
            _ => panic!("stale writer did not receive a fresh load"),
        }

        let invalidated = store
            .invalidate::<ClaudeSdkFold>(
                agent_id(),
                generations,
                first_expected,
                BaselineReason::TipVersion,
            )
            .await;
        let (absent, invalidated_revision) = match invalidated {
            CommitOutcome::Committed(result) => {
                assert_eq!(result.boundaries.len(), 1);
                assert_eq!(result.boundaries[0].segment, 2);
                assert_eq!(result.boundaries[0].before, None);
                (result.expected, result.content_revision)
            }
            _ => panic!("invalidation was not accepted"),
        };
        assert!(matches!(absent, ExpectedHead::Absent { .. }));
        let pending_boundary = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(7))
            .await
            .unwrap();
        assert!(pending_boundary.boundaries.iter().any(|boundary| {
            boundary.segment == 2
                && boundary.before.is_none()
                && boundary.boundary == fold::Boundary::VersionGap
        }));

        let repeated = store
            .invalidate::<ClaudeSdkFold>(
                agent_id(),
                generations,
                absent,
                BaselineReason::TipVersion,
            )
            .await;
        match repeated {
            CommitOutcome::Committed(result) => {
                assert_eq!(result.expected, absent);
                assert_eq!(result.content_revision, invalidated_revision);
                assert!(result.boundaries.is_empty());
            }
            _ => panic!("repeated invalidation was not idempotent"),
        }

        let stale_invalidation = second
            .invalidate::<ClaudeSdkFold>(
                agent_id(),
                second_generations,
                first_expected,
                BaselineReason::Corrupt,
            )
            .await;
        assert!(matches!(stale_invalidation, CommitOutcome::Conflict(_)));

        let mut resumed = ClaudeSdkFold::default();
        resumed.begin(2, Baseline::VersionGap { after: 1 });
        let resumed_mutations = fold_rows(&mut resumed, &[(2, "u2", "two")]);
        let resumed = store
            .commit(
                agent_id(),
                generations,
                absent,
                head(resumed, 2, Baseline::VersionGap { after: 1 }, 2),
                Some(transition(
                    Some(1),
                    2,
                    Baseline::VersionGap { after: 1 },
                    1,
                    Some(2),
                    2,
                )),
                resumed_mutations,
                all_interest(),
            )
            .await;
        let resumed_expected = match resumed {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("successor commit failed"),
        };
        assert!(matches!(
            (first_expected, resumed_expected),
            (
                ExpectedHead::Present { version: 1, .. },
                ExpectedHead::Present { version: 1, .. }
            )
        ));

        let delayed = second
            .commit(
                agent_id(),
                second_generations,
                first_expected,
                head(fold, 1, Baseline::Start, 1),
                None,
                Vec::new(),
                all_interest(),
            )
            .await;
        match delayed {
            CommitOutcome::Conflict(loaded) => {
                assert!(matches!(loaded.head, HeadState::Usable(1, _)));
                assert_ne!(
                    first_expected,
                    ExpectedHead::Present {
                        fence: loaded.fence,
                        version: 1,
                    },
                    "the equal head version must be rejected by the invalidation fence"
                );
            }
            _ => panic!("delayed pre-invalidation writer replaced the successor"),
        }
        second.close().await;
        store.close().await;
    });
}

#[test]
fn chat_rejects_stale_derivations_and_invalid_mutations_without_corrupting_store() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let store = Store::open(&database(&temp)).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let initial = fold_rows(&mut fold, &[(1, "u1", "one")]);
        let expected = match store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 1),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 1)),
                initial,
                all_interest(),
            )
            .await
        {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("seed commit failed"),
        };

        macro_rules! assert_conflict {
            ($head:expr, $transition:expr) => {
                match store
                    .commit(
                        agent_id(),
                        generations,
                        expected,
                        $head,
                        $transition,
                        Vec::new(),
                        all_interest(),
                    )
                    .await
                {
                    CommitOutcome::Conflict(loaded) => {
                        assert!(matches!(loaded.head, HeadState::Usable(1, _)));
                        assert_eq!(loaded.window.len(), 1);
                    }
                    _ => panic!("stale derivation did not return a fresh load"),
                }
            };
        }

        assert_conflict!(head(fold.clone(), 2, Baseline::Start, 1), None);
        assert_conflict!(head(fold.clone(), 1, Baseline::Gap { after: 1 }, 1), None);
        assert_conflict!(head(fold.clone(), 1, Baseline::Start, 0), None);
        assert_conflict!(
            head(fold.clone(), 3, Baseline::Gap { after: 1 }, 1),
            Some(transition(
                Some(1),
                3,
                Baseline::Gap { after: 1 },
                1,
                None,
                1,
            ))
        );
        assert_conflict!(
            head(fold.clone(), 2, Baseline::Gap { after: 1 }, 1),
            Some(transition(
                Some(2),
                2,
                Baseline::Gap { after: 1 },
                1,
                None,
                1,
            ))
        );

        let loaded = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        let stored = loaded.window.first().expect("stored entry");
        let past_head = Mutation::Delete {
            key: stored.key.clone(),
            revision: Revision::row(2),
        };
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations,
                    expected,
                    head(fold.clone(), 1, Baseline::Start, 1),
                    None,
                    vec![past_head],
                    all_interest(),
                )
                .await,
            CommitOutcome::Refused(StoreError::Invalid)
        ));

        let alias_a = EntryKey::new("alias:a").unwrap();
        let alias_b = EntryKey::new("alias:b").unwrap();
        let alias_cycle = vec![
            Mutation::Alias {
                from: alias_a.clone(),
                to: alias_b.clone(),
                revision: Revision::row(1),
                promote: None,
            },
            Mutation::Alias {
                from: alias_b,
                to: alias_a,
                revision: Revision::row(1),
                promote: None,
            },
        ];
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations,
                    expected,
                    head(fold.clone(), 1, Baseline::Start, 1),
                    None,
                    alias_cycle,
                    all_interest(),
                )
                .await,
            CommitOutcome::Refused(StoreError::Invalid)
        ));

        let disagreement = Mutation::Upsert {
            key: stored.key.clone(),
            order: stored.order,
            revision: stored.revision,
            entry: ClaudeSdkPartial {
                text: Patch::set("different".into(), Revision::row(1)),
                ..ClaudeSdkPartial::default()
            },
        };
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations,
                    expected,
                    head(fold, 1, Baseline::Start, 1),
                    None,
                    vec![disagreement],
                    all_interest(),
                )
                .await,
            CommitOutcome::Refused(StoreError::Invalid)
        ));
        assert!(!temp.path().join("quarantine/request.json").exists());
        store.close().await;
    });
}

#[test]
fn chat_unreadable_heads_invalidate_and_open_a_successor() {
    runtime().block_on(async {
        for damage in ["missing-tip", "bad-tip", "bad-summary"] {
            let temp = TempDir::new().unwrap();
            let path = database(&temp);
            let store = Store::open(&path).await.unwrap();
            let generations = store_generations(&store);
            let mut fold = ClaudeSdkFold::default();
            fold.begin(1, Baseline::Start);
            let mutations = fold_rows(&mut fold, &[(1, "u1", "one")]);
            assert!(matches!(
                store
                    .commit(
                        agent_id(),
                        generations,
                        ExpectedHead::Absent { fence: 0 },
                        head(fold, 1, Baseline::Start, 1),
                        Some(transition(None, 1, Baseline::Start, 0, Some(1), 1)),
                        mutations,
                        all_interest(),
                    )
                    .await,
                CommitOutcome::Committed(_)
            ));

            let connection = Connection::open(&path).unwrap();
            match damage {
                "missing-tip" => {
                    connection
                        .execute(
                            "DELETE FROM claude_sdk_tip WHERE agent_id=?1",
                            [agent_id().to_string()],
                        )
                        .unwrap();
                }
                "bad-tip" => {
                    connection
                        .execute(
                            "UPDATE claude_sdk_tip SET tip=X'00' WHERE agent_id=?1",
                            [agent_id().to_string()],
                        )
                        .unwrap();
                }
                "bad-summary" => {
                    connection
                        .execute(
                            "UPDATE chat_head SET summary=X'00' WHERE agent_id=?1",
                            [agent_id().to_string()],
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            drop(connection);

            let damaged = store
                .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
                .await
                .unwrap();
            assert!(matches!(
                damaged.head,
                HeadState::NeedsBaseline {
                    previous_through: 1,
                    reason: BaselineReason::Corrupt
                }
            ));
            let absent = match store
                .invalidate::<ClaudeSdkFold>(
                    agent_id(),
                    generations,
                    ExpectedHead::Absent {
                        fence: damaged.fence,
                    },
                    BaselineReason::Corrupt,
                )
                .await
            {
                CommitOutcome::Committed(result) => result.expected,
                _ => panic!("{damage} head did not invalidate"),
            };

            let mut successor = ClaudeSdkFold::default();
            successor.begin(2, Baseline::Gap { after: 1 });
            let mutations = fold_rows(&mut successor, &[(2, "u2", "two")]);
            assert!(matches!(
                store
                    .commit(
                        agent_id(),
                        generations,
                        absent,
                        head(successor, 2, Baseline::Gap { after: 1 }, 2),
                        Some(transition(
                            Some(1),
                            2,
                            Baseline::Gap { after: 1 },
                            1,
                            Some(2),
                            2,
                        )),
                        mutations,
                        all_interest(),
                    )
                    .await,
                CommitOutcome::Committed(_)
            ));
            store.close().await;
        }
    });
}

#[test]
fn chat_sqlite_corruption_during_writes_stops_worker_and_requests_quarantine() {
    runtime().block_on(async {
        for operation in ["commit", "invalidate"] {
            let temp = TempDir::new().unwrap();
            let path = database(&temp);
            let store = Store::open(&path).await.unwrap();
            let generations = store_generations(&store);
            let mut fold = ClaudeSdkFold::default();
            fold.begin(1, Baseline::Start);
            let mutations = fold_rows(&mut fold, &[(1, "u1", "one")]);
            let expected = match store
                .commit(
                    agent_id(),
                    generations,
                    ExpectedHead::Absent { fence: 0 },
                    head(fold.clone(), 1, Baseline::Start, 1),
                    Some(transition(None, 1, Baseline::Start, 0, Some(1), 1)),
                    mutations,
                    all_interest(),
                )
                .await
            {
                CommitOutcome::Committed(result) => result.expected,
                _ => panic!("seed commit failed"),
            };

            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA writable_schema=ON;
                     UPDATE sqlite_schema SET rootpage=2147483647 WHERE name='chat_state';
                     PRAGMA schema_version=999;",
                )
                .unwrap();
            drop(connection);

            let outcome = match operation {
                "commit" => {
                    store
                        .commit(
                            agent_id(),
                            generations,
                            expected,
                            head(fold, 1, Baseline::Start, 1),
                            None,
                            Vec::new(),
                            all_interest(),
                        )
                        .await
                }
                "invalidate" => {
                    store
                        .invalidate::<ClaudeSdkFold>(
                            agent_id(),
                            generations,
                            expected,
                            BaselineReason::Corrupt,
                        )
                        .await
                }
                _ => unreachable!(),
            };
            assert!(matches!(
                outcome,
                CommitOutcome::Refused(StoreError::Corrupt)
            ));
            wait_for(
                &temp.path().join("quarantine/request.json"),
                Duration::from_secs(2),
            );
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(temp.path().join("store.lock"))
                .unwrap();
            lock.try_lock().expect("worker released its store lease");
            lock.unlock().unwrap();
            store.close().await;
        }
    });
}

#[test]
fn chat_load_pages_boundaries_and_matches_the_fold_oracle() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let store = Store::open(&database(&temp)).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mutations = fold_rows(
            &mut fold,
            &[
                (1, "u1", "one"),
                (2, "u2", "two"),
                (3, "u3", "three"),
                (4, "u4", "four"),
                (5, "u5", "five"),
            ],
        );
        let mut oracle = MutationOracle::new(1, fold::DESKTOP_ENTRY_MAX_BYTES);
        let changes = fold::Changes {
            summary: None,
            through: 5,
            mutations: mutations.clone(),
        };
        oracle.apply_changes(&changes).unwrap();
        let outcome = store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 5),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 5)),
                mutations,
                all_interest(),
            )
            .await;
        let expected = match outcome {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("initial window commit failed"),
        };
        let loaded = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 2,
                    max_bytes: 16 * 1024 * 1024,
                    view_epoch: 22,
                },
            )
            .await
            .unwrap();
        assert_eq!(loaded.window.len(), 2);
        assert_eq!(loaded.window[0].order.seq(), 4);
        let first_token = loaded.first_page.clone().expect("older page");
        assert_eq!(first_token.view_epoch, 22);
        let page = store
            .page::<ClaudeSdkFold>(agent_id(), first_token.clone(), 2)
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].order.seq(), 2);
        assert!(page.next.is_some());

        let stored_all = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 20,
                    max_bytes: 16 * 1024 * 1024,
                    view_epoch: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(stored_all.window, oracle.entries());

        let stale_token = first_token;
        fold.begin(2, Baseline::Gap { after: 5 });
        let next = fold_rows(&mut fold, &[(6, "u6", "six")]);
        let moved = store
            .commit(
                agent_id(),
                generations,
                expected,
                head(fold, 2, Baseline::Gap { after: 5 }, 6),
                Some(transition(
                    Some(1),
                    2,
                    Baseline::Gap { after: 5 },
                    5,
                    Some(6),
                    6,
                )),
                next,
                all_interest(),
            )
            .await;
        let moved_expected = match moved {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("segment transition failed"),
        };
        assert_eq!(
            store
                .page::<ClaudeSdkFold>(agent_id(), stale_token, 2)
                .await,
            Err(StoreError::GenerationMoved)
        );
        let loaded = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 1,
                    max_bytes: 16 * 1024 * 1024,
                    view_epoch: 30,
                },
            )
            .await
            .unwrap();
        assert_eq!(loaded.segment_high_water, 2);
        assert!(
            loaded.boundaries.iter().any(|boundary| {
                boundary.segment == 2 && boundary.boundary == fold::Boundary::Gap
            })
        );
        let cross_boundary = store
            .page::<ClaudeSdkFold>(
                agent_id(),
                loaded.first_page.clone().expect("segment-one page"),
                5,
            )
            .await
            .unwrap();
        assert!(
            cross_boundary.boundaries.iter().any(|boundary| {
                boundary.segment == 2 && boundary.boundary == fold::Boundary::Gap
            })
        );

        let token_before_invalidate = loaded.first_page.unwrap();
        let invalidated = store
            .invalidate::<ClaudeSdkFold>(
                agent_id(),
                generations,
                moved_expected,
                BaselineReason::Corrupt,
            )
            .await;
        assert!(matches!(invalidated, CommitOutcome::Committed(_)));
        assert_eq!(
            store
                .page::<ClaudeSdkFold>(agent_id(), token_before_invalidate, 2)
                .await,
            Err(StoreError::GenerationMoved)
        );
        let invalidated_load = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert!(matches!(
            invalidated_load.head,
            HeadState::NeedsBaseline {
                previous_through: 6,
                reason: BaselineReason::Corrupt
            }
        ));
        assert!(
            invalidated_load.boundaries.iter().any(|boundary| {
                boundary.segment == 3 && boundary.boundary == fold::Boundary::Gap
            })
        );
        store.close().await;
    });
}

#[test]
fn chat_pages_to_exhaustion_without_overlapping_entries() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let store = Store::open(&database(&temp)).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mutations = fold_rows(
            &mut fold,
            &[
                (1, "u1", "one"),
                (2, "u2", "two"),
                (3, "u3", "three"),
                (4, "u4", "four"),
                (5, "u5", "five"),
                (6, "u6", "six"),
            ],
        );
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations,
                    ExpectedHead::Absent { fence: 0 },
                    head(fold, 1, Baseline::Start, 6),
                    Some(transition(None, 1, Baseline::Start, 0, Some(1), 6)),
                    mutations,
                    all_interest(),
                )
                .await,
            CommitOutcome::Committed(_)
        ));
        let loaded = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 1,
                    max_bytes: 1024 * 1024,
                    view_epoch: 9,
                },
            )
            .await
            .unwrap();
        assert_eq!(loaded.window[0].order.seq(), 6);
        let first = store
            .page::<ClaudeSdkFold>(agent_id(), loaded.first_page.unwrap(), 3)
            .await
            .unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.order.seq())
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        let second = store
            .page::<ClaudeSdkFold>(agent_id(), first.next.unwrap(), 3)
            .await
            .unwrap();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.order.seq())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(second.next.is_none());
        let mut all = loaded
            .window
            .iter()
            .chain(first.entries.iter())
            .chain(second.entries.iter())
            .map(|entry| entry.order.seq())
            .collect::<Vec<_>>();
        all.sort_unstable();
        assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);
        store.close().await;
    });
}

#[test]
fn chat_page_spanning_segments_carries_every_crossed_boundary() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let store = Store::open(&database(&temp)).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Truncated { from: 1 });
        let first = fold_rows(&mut fold, &[(1, "u1", "one"), (2, "u2", "two")]);
        let expected = match store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Truncated { from: 1 }, 2),
                Some(transition(
                    None,
                    1,
                    Baseline::Truncated { from: 1 },
                    0,
                    Some(1),
                    2,
                )),
                first,
                all_interest(),
            )
            .await
        {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("first segment failed"),
        };
        fold.begin(2, Baseline::Gap { after: 2 });
        let second = fold_rows(&mut fold, &[(3, "u3", "three"), (4, "u4", "four")]);
        assert!(matches!(
            store
                .commit(
                    agent_id(),
                    generations,
                    expected,
                    head(fold, 2, Baseline::Gap { after: 2 }, 4),
                    Some(transition(
                        Some(1),
                        2,
                        Baseline::Gap { after: 2 },
                        2,
                        Some(3),
                        4,
                    )),
                    second,
                    all_interest(),
                )
                .await,
            CommitOutcome::Committed(_)
        ));
        let loaded = store
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
        let page = store
            .page::<ClaudeSdkFold>(agent_id(), loaded.first_page.unwrap(), 10)
            .await
            .unwrap();
        assert!(page.boundaries.iter().any(|boundary| {
            boundary.segment == 1 && boundary.boundary == fold::Boundary::Truncated
        }));
        assert!(
            page.boundaries.iter().any(|boundary| {
                boundary.segment == 2 && boundary.boundary == fold::Boundary::Gap
            })
        );
        store.close().await;
    });
}

#[test]
fn chat_alias_delete_and_canonical_results_match_oracle() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let store = Store::open(&database(&temp)).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let _ = fold_rows(&mut fold, &[(1, "tip", "tip")]);
        let source = EntryKey::new("user:source").unwrap();
        let target = EntryKey::new("user:target").unwrap();
        let survivor = EntryKey::new("user:survivor").unwrap();
        let partial = |text: &str, seq| ClaudeSdkPartial {
            kind: Patch::set(ClaudeSdkEntryKind::Prompt, Revision::row(seq)),
            text: Patch::set(text.to_owned(), Revision::row(seq)),
            ..ClaudeSdkPartial::default()
        };
        let mutations = vec![
            Mutation::Upsert {
                key: source.clone(),
                order: Order::new(1, 0).unwrap(),
                revision: Revision::row(1),
                entry: partial("source", 1),
            },
            Mutation::Upsert {
                key: target.clone(),
                order: Order::new(2, 0).unwrap(),
                revision: Revision::row(2),
                entry: partial("target", 2),
            },
            Mutation::Upsert {
                key: survivor.clone(),
                order: Order::new(3, 0).unwrap(),
                revision: Revision::row(3),
                entry: partial("survivor", 3),
            },
            Mutation::Alias {
                from: source.clone(),
                to: target.clone(),
                revision: Revision::row(4),
                promote: None,
            },
            Mutation::Delete {
                key: survivor.clone(),
                revision: Revision::row(5),
            },
        ];
        let mut oracle = MutationOracle::new(1, fold::DESKTOP_ENTRY_MAX_BYTES);
        oracle
            .apply_changes(&fold::Changes {
                summary: None,
                through: 5,
                mutations: mutations.clone(),
            })
            .unwrap();
        let oversized = store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 5),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 5)),
                mutations.clone(),
                WindowInterest::all(7, 0),
            )
            .await;
        assert!(matches!(
            oversized,
            CommitOutcome::Refused(StoreError::OverBudget)
        ));
        let after_rollback = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert!(matches!(after_rollback.head, HeadState::None));
        assert!(after_rollback.window.is_empty());
        let result = store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 5),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 5)),
                mutations.clone(),
                all_interest(),
            )
            .await;
        let expected = match result {
            CommitOutcome::Committed(result) => {
                assert!(result.redirected.contains(&(source, target.clone())));
                assert!(result.deleted.contains(&survivor));
                assert_eq!(result.placed.len(), 1);
                assert_eq!(result.bodies.len(), 1);
                assert_eq!(result.placed[0].key, target);
                result.expected
            }
            _ => panic!("canonical mutation commit failed"),
        };
        let repeated = store
            .commit(
                agent_id(),
                generations,
                expected,
                head(fold, 1, Baseline::Start, 5),
                None,
                mutations,
                all_interest(),
            )
            .await;
        match repeated {
            CommitOutcome::Committed(result) => {
                assert_eq!(result.placed.len(), 1);
                assert_eq!(result.bodies.len(), 1);
            }
            _ => panic!("idempotent mutation commit failed"),
        }
        let loaded = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert_eq!(loaded.window, oracle.entries());
        store.close().await;
    });
}

#[test]
fn chat_family_recreation_by_another_process_moves_generation_and_refuses_newer_shape() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mutations = fold_rows(
            &mut fold,
            &[(1, "u1", "one"), (2, "u2", "two"), (3, "u3", "three")],
        );
        let expected = match store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 3),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 3)),
                mutations,
                all_interest(),
            )
            .await
        {
            CommitOutcome::Committed(result) => result.expected,
            _ => panic!("seed commit failed"),
        };
        let before = store
            .load::<ClaudeSdkFold>(
                agent_id(),
                WindowBudget {
                    max_entries: 1,
                    max_bytes: 8 * 1024 * 1024,
                    view_epoch: 7,
                },
            )
            .await
            .unwrap();
        let stale_page = before.first_page.expect("older stored entries");

        let connection = Connection::open(&path).unwrap();
        connection
            .execute("UPDATE family_shape SET shape=0 WHERE family='chat'", [])
            .unwrap();
        drop(connection);
        run_open_helper(&path, "success");

        let moved = store
            .commit(
                agent_id(),
                generations,
                expected,
                head(fold, 1, Baseline::Start, 3),
                None,
                Vec::new(),
                all_interest(),
            )
            .await;
        assert!(matches!(
            moved,
            CommitOutcome::Refused(StoreError::GenerationMoved)
        ));
        let reloaded = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(7))
            .await
            .unwrap();
        assert!(reloaded.generations.chat > generations.chat);
        assert!(reloaded.generations.provider > generations.provider);
        assert!(matches!(reloaded.head, HeadState::None));
        assert_eq!(
            store
                .page::<ClaudeSdkFold>(agent_id(), stale_page, 2)
                .await,
            Err(StoreError::GenerationMoved)
        );

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE family_shape SET shape=?1 WHERE family='chat'",
                [i64::from(store::CHAT_SHAPE + 1)],
            )
            .unwrap();
        let generation_before_refusal: i64 = connection
            .query_row(
                "SELECT generation FROM family_shape WHERE family='chat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        run_open_helper(&path, "unsupported");
        assert!(matches!(
            store
                .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(7))
                .await,
            Err(StoreError::UnsupportedFormat)
        ));
        let connection = Connection::open(&path).unwrap();
        let generation_after_refusal: i64 = connection
            .query_row(
                "SELECT generation FROM family_shape WHERE family='chat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let first_retirement_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='chat_state_old_1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let later_retirements: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name LIKE '%_old_2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation_after_refusal, generation_before_refusal);
        assert!(first_retirement_exists);
        assert_eq!(later_retirements, 0);
        store.close().await;
    });
}

fn run_open_helper(path: &Path, expected: &str) {
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--ignored",
            "--exact",
            "chat_helper_opens_store",
            "--nocapture",
        ])
        .env("AMUX_STORE_HELPER_DB", path)
        .env("AMUX_STORE_HELPER_EXPECT", expected)
        .output()
        .expect("spawn store opener");
    assert!(
        output.status.success(),
        "store opener failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "spawned by chat_family_recreation_by_another_process_moves_generation_and_refuses_newer_shape"]
fn chat_helper_opens_store() {
    let path = PathBuf::from(std::env::var_os("AMUX_STORE_HELPER_DB").expect("database path"));
    let expected = std::env::var("AMUX_STORE_HELPER_EXPECT").expect("expected outcome");
    match (expected.as_str(), runtime().block_on(Store::open(&path))) {
        ("success", Ok(store)) => runtime().block_on(store.close()),
        ("unsupported", Err(StoreError::UnsupportedFormat)) => {}
        (expected, Ok(store)) => {
            runtime().block_on(store.close());
            panic!("store unexpectedly opened while expecting {expected}");
        }
        (expected, Err(error)) => panic!("expected {expected}, got {error:?}"),
    }
}

#[test]
fn chat_newer_tip_is_refused() {
    runtime().block_on(async {
        let temp = TempDir::new().unwrap();
        let path = database(&temp);
        let store = Store::open(&path).await.unwrap();
        let generations = store_generations(&store);
        let mut fold = ClaudeSdkFold::default();
        fold.begin(1, Baseline::Start);
        let mutations = fold_rows(&mut fold, &[(1, "u1", "one")]);
        let committed = store
            .commit(
                agent_id(),
                generations,
                ExpectedHead::Absent { fence: 0 },
                head(fold.clone(), 1, Baseline::Start, 1),
                Some(transition(None, 1, Baseline::Start, 0, Some(1), 1)),
                mutations,
                all_interest(),
            )
            .await;
        match committed {
            CommitOutcome::Committed(_) => {}
            _ => panic!("seed commit failed"),
        }

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE chat_head SET tip_version=?1 WHERE agent_id=?2",
                rusqlite::params![
                    i64::from(ClaudeSdkFold::TIP_VERSION + 1),
                    agent_id().to_string()
                ],
            )
            .unwrap();
        assert!(matches!(
            store
                .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
                .await,
            Err(StoreError::UnsupportedFormat)
        ));
        connection
            .execute(
                "UPDATE chat_head SET tip_version=0 WHERE agent_id=?1",
                [agent_id().to_string()],
            )
            .unwrap();
        let older = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert!(matches!(
            older.head,
            HeadState::NeedsBaseline {
                previous_through: 1,
                reason: BaselineReason::TipVersion
            }
        ));
        let cannot_replace_without_invalidation = store
            .commit(
                agent_id(),
                older.generations,
                ExpectedHead::Absent { fence: older.fence },
                head(fold.clone(), 1, Baseline::Start, 1),
                None,
                Vec::new(),
                all_interest(),
            )
            .await;
        assert!(matches!(
            cannot_replace_without_invalidation,
            CommitOutcome::Conflict(_)
        ));
        connection
            .execute(
                "UPDATE chat_head SET tip_version=?1 WHERE agent_id=?2",
                rusqlite::params![
                    i64::from(ClaudeSdkFold::TIP_VERSION),
                    agent_id().to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE claude_sdk_tip SET tip=X'00' WHERE agent_id=?1",
                [agent_id().to_string()],
            )
            .unwrap();
        let corrupt = store
            .load::<ClaudeSdkFold>(agent_id(), WindowBudget::desktop(0))
            .await
            .unwrap();
        assert!(matches!(
            corrupt.head,
            HeadState::NeedsBaseline {
                previous_through: 1,
                reason: BaselineReason::Corrupt
            }
        ));
        store.close().await;
    });
}
