use serde_json::{Value, json};
use ui_state::{Msg, ServerMsg, StreamEntry, StreamMsg, update};
use uuid::Uuid;

use super::*;

const AGENT: AgentId = Uuid::from_u128(1);
const HOST: AgentId = Uuid::from_u128(2);
/// This phone. It is a device on the account like any machine and so has a
/// host id, but nothing runs on it and it is never the machine an agent is on.
const PHONE: AgentId = Uuid::from_u128(9);

fn host(online: bool) -> Msg {
    Msg::Server(ServerMsg::HostUpserted {
        host: model::HostEntry {
            id: HOST,
            name: "studio".into(),
            online,
            version: None,
            capabilities: None,
            trust_status: model::HostTrustStatus::Trusted,
            last_dial_error: None,
            via: model::HostVia::Direct,
            signed_in: None,
            platform: None,
        },
    })
}
fn upsert(kind: model::AgentKind) -> Msg {
    Msg::Server(ServerMsg::AgentUpserted {
        agent: Agent {
            id: AGENT,
            host_id: HOST,
            name: Some("Fix login".into()),
            command: "provider".into(),
            working_dir: "/work".into(),
            kind,
            readonly: false,
            args: vec![],
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            last_activity: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: 0,
        },
    })
}
fn model(kind: model::AgentKind) -> Model {
    let mut model = Model::default();
    for msg in [
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(PHONE),
        }),
        host(true),
        upsert(kind),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::ReplayComplete,
        },
    ] {
        update(&mut model, msg);
    }
    model
}
fn claude_model() -> Model {
    model(model::AgentKind::Claude {
        driver: model::ClaudeDriver::Pty,
    })
}
fn row(model: &mut Model, seq: u64, payload: Value) {
    let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    update(
        model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Batch {
                at,
                entries: vec![StreamEntry::observed(seq, at, payload)],
            },
        },
    );
}
fn message(id: usize, text: &str) -> Value {
    json!({"type":"assistant", "uuid":format!("row-{id}-{text}"), "sessionId":"session",
        "message":{"id":format!("message-{id}"), "role":"assistant", "stop_reason":"end_turn",
            "content":[{"type":"text", "text":text}]}})
}
fn collect(projection: &mut Projection, model: &Model) -> Vec<Event> {
    let mut events = vec![];
    projection.collect(model, &RelayConnection::Connected, &mut events);
    events
}
fn subscribed() -> Projection {
    let mut projection = Projection::default();
    projection.subscribe(AGENT);
    projection
}

// A consumer reconstructs only from public JSON indices, never reducer internals.
#[derive(Default)]
struct PhoneFeed {
    rows: BTreeMap<u64, Value>,
}
impl PhoneFeed {
    fn apply(&mut self, batch: &[Value]) -> usize {
        let mut payload_rows = 0;
        for event in batch {
            let Some(feed) = event.get("Feed") else {
                continue;
            };
            let mut keys: Vec<_> = feed
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort();
            assert_eq!(keys, ["agent", "append", "base", "evicted", "replace"]);
            let evicted = feed["evicted"].as_u64().unwrap();
            self.rows.retain(|id, _| *id >= evicted);
            for replacement in feed["replace"].as_array().unwrap() {
                let id = replacement[0].as_u64().unwrap();
                assert!(self.rows.insert(id, replacement[1].clone()).is_some());
                payload_rows += 1;
            }
            for (i, entry) in feed["append"].as_array().unwrap().iter().enumerate() {
                let id = feed["base"].as_u64().unwrap() + i as u64;
                assert!(self.rows.insert(id, entry.clone()).is_none());
                payload_rows += 1;
            }
        }
        payload_rows
    }
    fn apply_events(&mut self, events: &[Event]) -> usize {
        self.apply(
            &serde_json::from_str::<Vec<Value>>(&serde_json::to_string(events).unwrap()).unwrap(),
        )
    }
}

#[test]
fn mobile_projection_schema_snapshot() {
    let (mut model, stream) = stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        vec![
            message(0, "Hello"),
            json!({"type":"hook.stop","hook_event_name":"Stop","stop_id":2}),
        ],
    );
    update(
        &mut model,
        Msg::Tick {
            now: DateTime::from_timestamp(1_700_000_061, 0).unwrap(),
        },
    );
    let mut projection = subscribed();
    let mut events = vec![Event::connection(&RelayConnection::Connecting)];
    events.extend(collect(&mut projection, &model));
    events.push(Event::TokenRequest {
        request_id: 7,
        account: "personal".into(),
    });
    events.push(Event::OpResult {
        op: OpId(Uuid::from_u128(3)),
        outcome: OpOutcomeDto::Shared(Box::new(OpOutcome::InputSent)),
    });
    events.push(Event::OpResult {
        op: OpId(Uuid::from_u128(4)),
        outcome: OpOutcomeDto::Shared(Box::new(OpOutcome::Error {
            error: ui_state::OpError::general("send refused"),
        })),
    });
    events.push(Event::Diff {
        agent: AGENT,
        diff: serde_json::from_value(json!(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
        .unwrap(),
        document: ui_state::review::parse_stored_patch(
            "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-old\n+new\n",
            model::BaseIdentity {
                base: model::DiffBase::WorkingTree,
                head: "abc".into(),
                merge_base: None,
                blobs: vec![],
            },
        )
        .unwrap(),
    });
    chat_row(&mut model, stream, 3, message(0, "Updated"));
    chat_row(
        &mut model,
        stream,
        4,
        json!({"type":"hook.stop","hook_event_name":"Stop","stop_id":4}),
    );
    events.extend(collect(&mut projection, &model));
    let (mut codex, codex_stream) = stored_model(
        model::AgentKind::Codex,
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"item/started", "item":{"id":"m", "type":"agentMessage", "text":"Hello", "phase":"final_answer"}}),
        ],
    );
    events.extend(collect(&mut subscribed(), &codex));
    chat_row(
        &mut codex,
        codex_stream,
        3,
        json!({"type":"item/completed", "item":{"id":"m", "type":"agentMessage", "text":"Done", "phase":"final_answer"}}),
    );
    let facts = include_str!("../../../claude-specs/fixtures/claude-sdk/streamed_turn.rows.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .rev()
        .find(|row| row["type"] == "amux.claude_sdk.session_facts")
        .unwrap();
    let (sdk, _) = stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Sdk,
        },
        vec![facts],
    );
    events.extend(collect(&mut subscribed(), &sdk));
    events.push(Event::connection(&RelayConnection::Disconnected {
        reason: model::DisconnectReason::Unreachable,
    }));
    events.push(Event::Invariant {
        detail: "example diagnostic".into(),
    });
    events.push(Event::StoreFailure {
        message: "store /cache/personal.sqlite: the disk is full; free space and relaunch".into(),
    });
    events.push(Event::Attention {
        account: "work".into(),
        waiting: 2,
    });
    events.push(Event::Feed {
        agent: AGENT,
        base: 2,
        append: vec![FeedEntryDto::History(HistoryRow {
            id: 2,
            seq: 0,
            boundary: HistoryBoundary::Missing,
        })],
        replace: vec![],
        evicted: 0,
    });
    events.push(Event::Devices {
        identity: DeviceIdentityDto {
            host: PHONE,
            name: "iPhone".into(),
            fingerprint: "4f2a91c05b7e8d3a6c14f0928be5d7a3419c60fe2d8b7a05c31e94f2ab7d69c1".into(),
        },
        devices: vec![PairedDeviceDto {
            host: uuid::Uuid::from_u128(1),
            name: "studio".into(),
            fingerprint: "e04a7b12c98d3f5601ae72b4d8c05913f6a2e7dbc4051829f3b6ad70e91c58d2".into(),
            paired_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        }],
    });
    // The link this device is on and what the account on it buys, in the one
    // shape a paying subscriber over QUIC produces.
    events.push(Event::CloudState(ui_state::CloudState::Connected {
        tier: model::Tier::Pro,
        carrier: model::RelayCarrier::Quic,
    }));
    // What a start reports of the removed accounts it really did get rid of,
    // which is the only word the phone may drop a pending removal on.
    events.push(Event::Forgotten {
        accounts: vec!["work".into()],
    });
    let actual = format!("{}\n", serde_json::to_string_pretty(&events).unwrap());
    if std::env::var_os("UPDATE_MOBILE_PROJECTION").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/projection/schema.json"),
            &actual,
        )
        .unwrap();
    } else {
        assert_eq!(actual, include_str!("schema.json"));
    }
    assert_eq!(serde_json::from_str::<Vec<Event>>(&actual).unwrap(), events);
    println!("mobile projection schema:\n{actual}");
}

#[test]
fn mobile_projection_replaces_a_stored_codex_message_as_its_deltas_arrive() {
    let (mut model, stream) = stored_model(
        model::AgentKind::Codex,
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"item/started", "item":{"id":"m", "type":"agentMessage", "text":"", "phase":"final_answer"}}),
            json!({"type":"item/agentMessage/delta", "itemId":"m", "delta":"Hello"}),
        ],
    );
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    assert_eq!(phone.apply_events(&collect(&mut projection, &model)), 1);
    assert_eq!(phone.rows.len(), 1);
    assert!(
        phone
            .rows
            .values()
            .next()
            .unwrap()
            .to_string()
            .contains("Hello")
    );

    chat_row(
        &mut model,
        stream,
        4,
        json!({"type":"item/agentMessage/delta", "itemId":"m", "delta":"!"}),
    );
    let events = collect(&mut projection, &model);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Feed {
            append,
            replace,
            ..
        } if append.is_empty() && replace.len() == 1
    )));
    assert_eq!(phone.apply_events(&events), 1);
    assert_eq!(phone.rows.len(), 1);
    assert!(
        phone
            .rows
            .values()
            .next()
            .unwrap()
            .to_string()
            .contains("Hello!")
    );
    assert!(collect(&mut projection, &model).is_empty());
}

/// A stored conversation stays readable while its machine is away and through
/// that machine's replay. Authoritative agent removal drops the fleet member,
/// but an already-open stored conversation remains readable until the reader
/// closes it.
#[test]
fn mobile_projection_keeps_the_feed_of_an_agent_whose_host_has_gone_away() {
    let (mut model, stream) = stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        (0..3).map(|id| message(id, "before the outage")).collect(),
    );
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    assert_eq!(phone.apply_events(&collect(&mut projection, &model)), 3);
    let held = phone.rows.clone();

    // The machine goes away. The relay drops its live agent, while the store
    // window remains this device's account of the conversation.
    update(&mut model, host(false));
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentRemoved { id: AGENT }),
    );
    update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::Closed {
                at: DateTime::from_timestamp(1_700_000_001, 0).unwrap(),
                reason: ui_state::StreamCloseReason::HostUnreachable,
            },
        },
    );
    assert!(model.claude(AGENT).is_some());
    assert!(!model.agent(AGENT).unwrap().live);
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows, held, "the rows went when the machine did");

    // Its inventory answers again without reconnecting the client's daemon
    // link or changing the agent record. Cursor replay appends only the new row.
    update(&mut model, host(true));
    let effects = update(
        &mut model,
        Msg::Server(ServerMsg::HostInventory {
            host_id: HOST,
            agent_ids: vec![AGENT],
        }),
    );
    let replay_stream = effects
        .into_iter()
        .find_map(|effect| match effect {
            ui_state::Effect::OpenStoreStream { attempt, .. } => Some(attempt),
            _ => None,
        })
        .expect("reconnect reopens the stored chat stream");
    assert_ne!(replay_stream, stream);
    assert!(
        update(
            &mut model,
            Msg::Server(ServerMsg::HostInventory {
                host_id: HOST,
                agent_ids: vec![AGENT],
            }),
        )
        .is_empty()
    );
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(
        phone.rows, held,
        "the rows went while the machine came back"
    );
    update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: replay_stream,
            event: ui_state::ChatStreamMsg::Opened {
                facts: ui_state::ReplayFactsDto {
                    retained_from: 1,
                    through: 4,
                    selected_from: 4,
                    reset_at: 0,
                    outcome: ui_state::ReplayOutcomeDto::Continuous,
                },
                at: DateTime::from_timestamp(1_700_000_002, 0).unwrap(),
            },
        },
    );
    chat_row(&mut model, replay_stream, 4, message(3, "after the outage"));
    update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: replay_stream,
            event: ui_state::ChatStreamMsg::ReplayComplete {
                at: DateTime::from_timestamp(1_700_000_003, 0).unwrap(),
            },
        },
    );
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows.len(), held.len() + 1);
    assert!(
        held.iter().all(|(id, row)| phone.rows.get(id) == Some(row)),
        "replay replaced or duplicated stored rows"
    );
    assert!(
        phone
            .rows
            .values()
            .last()
            .unwrap()
            .to_string()
            .contains("after the outage")
    );

    // Authoritative removal drops the live agent, not the already-open store
    // window. The phone keeps showing the history it has until the reader
    // closes that conversation.
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentRemoved { id: AGENT }),
    );
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(
        phone.rows.len(),
        held.len() + 1,
        "agent removal discarded stored history"
    );
}

/// Exited agents leave the global inventory, while an already-open
/// conversation retains the terminal fact it needs to explain why it cannot
/// be written to.
#[test]
fn mobile_projection_retains_exit_status_for_an_open_conversation() {
    let (mut model, stream) = stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        vec![message(0, "finished")],
    );
    update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::Closed {
                at: DateTime::from_timestamp(1_700_000_001, 0).unwrap(),
                reason: ui_state::StreamCloseReason::AgentExited { exit_code: Some(7) },
            },
        },
    );
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentRemoved { id: AGENT }),
    );

    let events = collect(&mut subscribed(), &model);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Fleet { agents, .. } if agents.iter().all(|card| card.agent.id != AGENT)
    )));
    assert_eq!(
        session(&model, AGENT).terminal_phase,
        Some(model::AgentPhase::Exited { exit_code: Some(7) })
    );
}

#[tokio::test(start_paused = true)]
async fn mobile_projection_streaming_bench_1000_rows_at_50_per_second() {
    // Virtual time pins the rate and cadence without conflating the bridge
    // payload contract with simulator frame-performance measurements.
    for interval in [
        Duration::from_nanos(8_333_333),
        Duration::from_nanos(16_666_667),
        Duration::from_millis(100),
    ] {
        let (mut model, stream) = stored_model(
            model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            vec![message(0, "row 0000")],
        );
        let mut projection = subscribed();
        let mut cadence = Cadence::new(interval);
        let mut phone = PhoneFeed::default();
        let initial = collect(&mut projection, &model);
        let mut bytes = serde_json::to_vec(&initial).unwrap().len();
        let mut sent_rows = phone.apply_events(&initial);
        assert_eq!(sent_rows, 1);
        let mut times = Vec::new();
        let start = Instant::now();
        let mut dirty = false;
        let mut id = 1;
        while id < 1000 || dirty {
            let next_row = start + Duration::from_millis(id * 20);
            let next_frame = cadence.deadline();
            if dirty && (id == 1000 || next_frame <= next_row) {
                tokio::time::sleep_until(next_frame).await;
                let events = collect(&mut projection, &model);
                if !events.is_empty() {
                    let batch = serde_json::to_vec(&events).unwrap();
                    let values = serde_json::from_slice::<Vec<Value>>(&batch).unwrap();
                    let frame_rows = phone.apply(&values);
                    assert!(frame_rows > 0, "a dirty frame carried no chat delta");
                    assert!(
                        batch.len() < 1000 + frame_rows * 1500,
                        "one frame included retained history: {} bytes for {frame_rows} rows",
                        batch.len()
                    );
                    bytes += batch.len();
                    sent_rows += frame_rows;
                    times.push(Instant::now());
                    cadence.emitted();
                }
                dirty = false;
            } else {
                tokio::time::sleep_until(next_row).await;
                chat_row(
                    &mut model,
                    stream,
                    id + 1,
                    message(id as usize, &format!("row {id:04}")),
                );
                assert!(
                    model
                        .chat(AGENT)
                        .expect("store-backed chat stays open")
                        .entries
                        .last()
                        .is_some_and(|entry| serde_json::to_string(entry)
                            .unwrap()
                            .contains(&format!("row {id:04}"))),
                    "row {id} did not commit into the store window"
                );
                dirty = true;
                id += 1;
            }
        }
        assert_eq!(sent_rows, 1000, "a stored row was missed or sent twice");
        assert_eq!(
            phone.rows.len(),
            ui_state::store::PHONE_WINDOW_MAX_ENTRIES,
            "the phone did not retain its configured visible-history window"
        );
        assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= interval));
        assert!(
            bytes < 1000 * 1500,
            "payload must grow with delta bytes: {bytes}"
        );
        println!(
            "mobile projection bench: interval_ns={} rows=1000 rate=50/s batches={} rows_serialized={sent_rows} total_bytes={bytes} virtual_duration_ms={}",
            interval.as_nanos(),
            times.len(),
            (Instant::now() - start).as_millis()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn mobile_projection_cadence_adapts_without_catchup() {
    let mut cadence = Cadence::new(Duration::from_millis(17));
    cadence.emitted();
    let last = Instant::now();
    cadence.set_interval(Duration::from_millis(8));
    assert_eq!(cadence.deadline(), last + Duration::from_millis(8));
    tokio::time::advance(Duration::from_millis(100)).await;
    cadence.emitted();
    assert_eq!(
        cadence.deadline(),
        Instant::now() + Duration::from_millis(8)
    );
    cadence.set_interval(Duration::from_millis(33));
    assert_eq!(
        cadence.deadline(),
        Instant::now() + Duration::from_millis(33)
    );
}

/// A frozen diff reaches the phone once, as the review document the rest of
/// the workspace reads: files under their own paths, rows already numbered,
/// and the repository identity it was taken against.
#[test]
fn mobile_projection_op_results_and_diff_are_delivered_once() {
    let mut model = claude_model();
    let mut projection = subscribed();
    let op = OpId(Uuid::from_u128(9));
    let artifact: ui_state::ArtifactId = serde_json::from_value(json!(
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ))
    .unwrap();
    update(
        &mut model,
        Msg::Command {
            op,
            command: Command::RequestDiff {
                agent: AGENT,
                base: model::DiffBase::WorkingTree,
            },
        },
    );
    update(
        &mut model,
        Msg::OpResult {
            op,
            outcome: OpOutcome::DiffReady {
                response: model::DiffResponse {
                    artifact: model::ArtifactRef {
                        id: artifact.clone(),
                        kind: model::ArtifactKind::Diff,
                        name: "changes.patch".into(),
                        mime: "text/x-diff".into(),
                        size: 0,
                    },
                    patch: "diff --git a/one.rs b/one.rs\n\
                            --- a/one.rs\n\
                            +++ b/one.rs\n\
                            @@ -1 +1 @@\n\
                            -old\n\
                            +new\n"
                        .into(),
                    identity: model::BaseIdentity {
                        base: model::DiffBase::WorkingTree,
                        head: "abc".into(),
                        merge_base: None,
                        blobs: vec![],
                    },
                    files: vec![model::DiffFile {
                        path: "one.rs".into(),
                        added: 1,
                        removed: 1,
                    }],
                },
            },
        },
    );
    let mut events = vec![];
    projection.outcomes(&model, &mut events);
    let [Event::OpResult { .. }, Event::Diff { diff, document, .. }] = events.as_slice() else {
        panic!("expected one op result and one diff, got {events:?}");
    };
    assert_eq!(diff, &artifact);
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].path, "one.rs");
    assert_eq!(document.files[0].rows.len(), 2);
    assert_eq!(document.identity.head, "abc");
    projection.outcomes(&model, &mut events);
    assert_eq!(events.len(), 2);
}

/// Queue changes cross the same session callback boundary as provider facts.
#[test]
fn queue_mobile_projection_exposes_hold_and_cancellation() {
    let mut model = claude_model();
    row(&mut model, 1, json!({"type":"amux.transcript_ready"}));
    row(
        &mut model,
        2,
        json!({"type":"user", "uuid":"00000000-0000-0000-0000-000000000001", "origin":{"kind":"human"}, "message":{"role":"user", "content":"work"}}),
    );
    let mut projection = subscribed();
    collect(&mut projection, &model);
    update(
        &mut model,
        Msg::Command {
            op: OpId(Uuid::from_u128(1)),
            command: Command::Queue(ui_state::QueueCommand::Hold {
                agent: AGENT,
                draft: ui_state::Draft {
                    segments: vec![ui_state::DraftSegment::Text {
                        text: "next step".into(),
                    }],
                    attachments: vec![],
                },
            }),
        },
    );
    let held = collect(&mut projection, &model);
    assert!(held.iter().any(|event| matches!(event, Event::Session(session) if session.queue.as_ref().is_some_and(|queue| queue.draft.text() == "next step"))));
    println!(
        "mobile held queue callback:\n{}",
        serde_json::to_string_pretty(&held).unwrap()
    );
    // Pinned to a file rather than only asserted, because a reader on the
    // other side of the bridge has to know how a held draft is spelled: its
    // segments, its delivery state and the moment it was held. The iOS test
    // bundle reads this same file.
    let pinned = format!(
        "{}\n",
        serde_json::to_string_pretty(
            held.iter()
                .filter(|event| matches!(event, Event::Session(_)))
                .collect::<Vec<_>>()
                .last()
                .expect("a session carrying the held queue")
        )
        .unwrap()
    );
    if std::env::var_os("UPDATE_MOBILE_PROJECTION").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/projection/queue.json"),
            &pinned,
        )
        .unwrap();
    } else {
        assert_eq!(pinned, include_str!("queue.json"));
    }
    update(
        &mut model,
        Msg::Command {
            op: OpId(Uuid::from_u128(2)),
            command: Command::Queue(ui_state::QueueCommand::Cancel { agent: AGENT }),
        },
    );
    let mut cancelled = Vec::new();
    projection.outcomes(&model, &mut cancelled);
    cancelled.extend(collect(&mut projection, &model));
    assert!(
        cancelled
            .iter()
            .any(|event| matches!(event, Event::Session(session) if session.queue.is_none()))
    );
    assert!(cancelled.iter().any(|event| matches!(event, Event::OpResult { outcome: OpOutcomeDto::Shared(outcome), .. } if matches!(&**outcome, OpOutcome::QueueCancelled { draft } if draft.text() == "next step"))));
}

#[test]
fn model_effort_mobile_session_projects_recorded_choices_and_pty_gate() {
    let mut model = model(model::AgentKind::Codex);
    let mut projection = subscribed();
    let rows: Vec<Value> =
        include_str!("../../../testnet/tests/fixtures/codex/model_effort.rows.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let mut sessions = Vec::new();
    for (index, payload) in rows.into_iter().enumerate() {
        row(&mut model, index as u64 + 1, payload);
        for event in collect(&mut projection, &model) {
            if let Event::Session(session) = event {
                sessions.push(session);
            }
        }
    }
    assert_eq!(
        sessions.len(),
        4,
        "each changed host selection reaches the callback"
    );
    assert_eq!(sessions[0].provider.model.as_deref(), Some("model-a"));
    let selected = sessions.last().unwrap();
    assert_eq!(selected.provider.model.as_deref(), Some("model-b"));
    assert_eq!(selected.provider.effort.as_deref(), Some("high"));
    assert_eq!(selected.provider.efforts, ["medium", "high"]);
    assert_eq!(
        selected.settings_gate,
        ui_state::provider::SettingsGate::Ready
    );
    println!(
        "Mobile session callback: {}",
        serde_json::to_string_pretty(&Event::Session(selected.clone())).unwrap()
    );
    let pty = session(&claude_model(), AGENT);
    assert_eq!(
        pty.settings_gate,
        ui_state::provider::SettingsGate::PtySettingsUnavailable
    );
    assert!(pty.provider.models.is_empty());
}

#[test]
fn provider_commands_mobile_callback_exposes_reported_list() {
    let mut model = model(model::AgentKind::Codex);
    let mut projection = subscribed();
    for (index, line) in
        include_str!("../../../testnet/tests/fixtures/codex/provider_commands.rows.jsonl")
            .lines()
            .enumerate()
    {
        row(
            &mut model,
            index as u64 + 1,
            serde_json::from_str(line).unwrap(),
        );
    }
    let events = collect(&mut projection, &model);
    let selected = events
        .iter()
        .find_map(|event| match event {
            Event::Session(session) => Some(session),
            _ => None,
        })
        .unwrap();
    assert_eq!(selected.provider.commands.len(), 1);
    assert_eq!(selected.provider.commands[0].name, "review");
    assert!(!selected.provider.commands[0].terminal_only);
    assert!(session(&claude_model(), AGENT).provider.commands.is_empty());
    println!(
        "Mobile command session callback: {}",
        serde_json::to_string_pretty(&events).unwrap()
    );
}

#[test]
fn todos_mobile_callback_replaces_session_facts_without_appending_feed_rows() {
    let mut model = claude_model();
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    phone.apply_events(&collect(&mut projection, &model));
    let mut lists = vec![];
    for (i, line) in include_str!("../../../ui-state/tests/fixtures/todos/rows.jsonl")
        .lines()
        .enumerate()
    {
        row(
            &mut model,
            i as u64 + 1,
            serde_json::from_str(line).unwrap(),
        );
        let events = collect(&mut projection, &model);
        assert_eq!(phone.apply_events(&events), 0);
        for event in &events {
            if let Event::Session(session) = event
                && let Some(list) = &session.provider.todos
                && lists.last() != Some(list)
            {
                lists.push(list.clone());
                println!(
                    "Mobile todo session callback: {}",
                    serde_json::to_string(event).unwrap()
                );
            }
        }
    }
    assert_eq!(
        lists
            .iter()
            .map(|list| (list.done, list.total))
            .collect::<Vec<_>>(),
        [(1, 3), (1, 2), (1, 1), (0, 0)]
    );
    assert!(phone.rows.is_empty());
}

/// Every ask shape the phone draws a panel for, taken from recorded sessions
/// rather than written by hand.
///
/// The panels read a provider's own words — a command, a question's options, a
/// plan's markdown, Codex's decisions — and a hand-written example of those
/// words agrees with itself and with nothing else. So each shape here is the
/// projection of a real recording replayed up to the moment it asked, pinned
/// beside the app that reads it.
const ASK_FIXTURES: &[(&str, model::AgentKind, &str)] = &[
    (
        "permission",
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        include_str!("../../../claude-specs/fixtures/claude-pty/permission_session.rows.jsonl"),
    ),
    (
        "question",
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        include_str!("../../../claude-specs/fixtures/claude-pty/question_multi.rows.jsonl"),
    ),
    (
        "plan",
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        include_str!("../../../claude-specs/fixtures/claude-pty/plan_approve.rows.jsonl"),
    ),
    (
        "codex-approval",
        model::AgentKind::Codex,
        include_str!("../../../codex-specs/fixtures/codex/approval_allow.rows.jsonl"),
    ),
];

fn pending_asks(model: &Model) -> Vec<AskDto> {
    let mut asks: Vec<AskDto> = model
        .claude(AGENT)
        .map(|l| l.asks().cloned().map(AskDto::ClaudePty).collect())
        .unwrap_or_default();
    asks.extend(
        model
            .codex(AGENT)
            .map(|l| l.asks().cloned().map(AskDto::Codex).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    asks
}

#[test]
fn mobile_projection_ask_snapshot() {
    let mut pinned = serde_json::Map::new();
    for (name, kind, fixture) in ASK_FIXTURES {
        let mut model = self::model(*kind);
        let mut asks = vec![];
        for (index, line) in fixture.lines().enumerate() {
            row(
                &mut model,
                index as u64 + 1,
                serde_json::from_str(line).unwrap(),
            );
            asks = pending_asks(&model);
            if !asks.is_empty() {
                break;
            }
        }
        assert!(!asks.is_empty(), "{name} never reached an unanswered ask",);
        pinned.insert((*name).to_string(), serde_json::to_value(&asks).unwrap());
    }
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(pinned)).unwrap()
    );
    if std::env::var_os("UPDATE_MOBILE_PROJECTION").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/projection/asks.json"),
            &actual,
        )
        .unwrap();
    } else {
        assert_eq!(actual, include_str!("asks.json"));
    }
}

fn fleet_card(projection: &mut Projection, model: &Model) -> AgentCardDto {
    collect(projection, model)
        .into_iter()
        .find_map(|event| match event {
            Event::Fleet { agents, .. } => agents.into_iter().find(|card| card.agent.id == AGENT),
            _ => None,
        })
        .expect("the fleet names the agent")
}

/// A row that needs you says what is wanted, so the fleet card carries the
/// ask at the head of the queue — and a calm agent's card carries none.
#[test]
fn mobile_projection_fleet_cards_carry_the_waiting_ask() {
    for (name, kind, fixture) in ASK_FIXTURES {
        let mut model = self::model(*kind);
        let mut projection = Projection::default();
        assert_eq!(fleet_card(&mut projection, &model).ask, None, "{name}");
        for (index, line) in fixture.lines().enumerate() {
            row(
                &mut model,
                index as u64 + 1,
                serde_json::from_str(line).unwrap(),
            );
            if !pending_asks(&model).is_empty() {
                break;
            }
        }
        let card = fleet_card(&mut projection, &model);
        assert!(
            matches!(
                card.attention,
                Attention::NeedsYou {
                    why: Why::Permission | Why::Question
                }
            ),
            "{name}: {:?}",
            card.attention
        );
        assert_eq!(card.ask.as_ref(), pending_asks(&model).first(), "{name}");
        let json = serde_json::to_value(&card).unwrap();
        assert!(json["ask"]["layer"].is_string(), "{name}: {json}");
    }

    let calm = claude_model();
    let card = fleet_card(&mut Projection::default(), &calm);
    assert_eq!(card.ask, None);
    assert!(serde_json::to_value(&card).unwrap().get("ask").is_none());
}

#[test]
fn mobile_projection_sdk_keeps_native_rows_gates_asks_and_reconnect_history() {
    let (mut model, stream) = stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Sdk,
        },
        vec![
            json!({"type":"amux.claude_sdk.ready"}),
            message(1, "SDK reply"),
            json!({"type":"result", "subtype":"success", "is_error":false}),
        ],
    );
    let mut projection = subscribed();
    let events = collect(&mut projection, &model);
    let mut phone = PhoneFeed::default();
    phone.apply_events(&events);
    assert!(!phone.rows.is_empty());
    assert!(phone.rows.values().all(|row| row["layer"] == "claude_sdk"));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Session(session)
        if session.agent == AGENT && session.gate == GateDto::ClaudeSdk(claude_sdk::SendGate::Ready)
        && session.phase == PhaseDto::ClaudeSdk(claude_sdk::SdkPhase::Finished)))
    );
    chat_row(
        &mut model,
        stream,
        4,
        json!({"type":"amux.claude_sdk.permission_required",
        "request_id":"permission-1", "tool_name":"Bash", "input":{"command":"pwd"}, "suggestions":[]}),
    );
    let events = collect(&mut projection, &model);
    assert!(events.iter().any(|event| matches!(event, Event::Session(session)
        if session.gate == GateDto::ClaudeSdk(claude_sdk::SendGate::NeedsYou)
        && matches!(session.asks.first(), Some(AskDto::ClaudeSdk(ask)) if ask.request_id == "permission-1"))));
    phone.apply_events(&events);
    let before = phone.rows.clone();
    update(&mut model, host(false));
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows, before);
    update(&mut model, host(true));
    update(
        &mut model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Opened { truncated: false },
        },
    );
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows, before);
    chat_row(&mut model, stream, 10, message(2, "Replayed SDK reply"));
    phone.apply_events(&collect(&mut projection, &model));
    assert!(phone.rows.values().all(|row| row["layer"] == "claude_sdk"));
    assert_ne!(phone.rows, before);
    assert!(
        phone
            .rows
            .values()
            .any(|row| row.to_string().contains("Replayed SDK reply"))
    );
}

/// A conversation opened through the store, holding the rows the chat stream
/// has folded so far.
fn stored_claude_model(rows: usize) -> (Model, ui_state::StreamAttempt) {
    let rows = (0..rows).map(|id| message(id, "stored")).collect();
    stored_model(
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        },
        rows,
    )
}

#[test]
fn stored_exploration_runs_group_and_break_at_history_gaps() {
    let tool = |id: usize, name: &str, input: Value| {
        json!({"type":"assistant", "uuid":format!("row-{id}"),
            "message":{"id":format!("message-{id}"), "role":"assistant",
                "content":[{"type":"tool_use", "id":format!("tool-{id}"),
                    "name":name, "input":input}]}})
    };
    for driver in [model::ClaudeDriver::Pty, model::ClaudeDriver::Sdk] {
        let (model, _) = stored_model(
            model::AgentKind::Claude { driver },
            vec![
                tool(0, "Read", json!({"file_path":"parser.rs"})),
                tool(1, "Grep", json!({"pattern":"split"})),
                message(2, "An explanation separates the runs."),
                tool(3, "Read", json!({"file_path":"wire.rs"})),
                tool(4, "Grep", json!({"pattern":"token"})),
            ],
        );
        let mut projection = subscribed();
        let mut phone = PhoneFeed::default();
        let groups = |phone: &PhoneFeed| -> Vec<bool> {
            phone
                .rows
                .values()
                .filter_map(|row| {
                    let kind = &row["row"]["kind"];
                    kind["group_with_previous"]
                        .as_bool()
                        .or_else(|| kind["entry"]["group_with_previous"].as_bool())
                })
                .collect()
        };
        phone.apply_events(&collect(&mut projection, &model));
        assert_eq!(groups(&phone), [false, true, false, true], "{driver:?}");

        let broken = after_a_gap(&model, 1);
        phone.apply_events(&collect(&mut projection, &broken));
        assert_eq!(groups(&phone), [false, false, false, true], "{driver:?}");
        let mut fresh = PhoneFeed::default();
        fresh.apply_events(&collect(&mut subscribed(), &broken));
        assert_eq!(
            phone.rows.values().collect::<Vec<_>>(),
            fresh.rows.values().collect::<Vec<_>>()
        );
    }
}
/// A conversation with any provider opened through the store, holding the
/// entries its chat stream folded from `rows`.
fn stored_model(kind: model::AgentKind, rows: Vec<Value>) -> (Model, ui_state::StreamAttempt) {
    use fold::{
        CommitResult, ExpectedHead, Generations, HeadState, JsonBytes, Loaded, MutationOracle,
        Stored,
    };
    use ui_state::{
        ChatCommand, Effect, LoadedDto, MutationBatchDto, ProfileGeneration, StoreMsg, StoreOp,
    };
    const GENERATIONS: Generations = Generations {
        fleet: 1,
        chat: 1,
        provider: 1,
    };
    let protocol = match kind {
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        } => ui_state::StructuredProtocol::ClaudePtyTranscript,
        model::AgentKind::Claude {
            driver: model::ClaudeDriver::Sdk,
        } => ui_state::StructuredProtocol::ClaudeSdk,
        model::AgentKind::Codex => ui_state::StructuredProtocol::Codex,
        model::AgentKind::TestAgent => panic!("test agent has no stored structured chat"),
    };
    let mut model = model(kind);
    update(
        &mut model,
        Msg::StoreStartup {
            profile: ProfileGeneration(0),
            generations: GENERATIONS,
            window_max_entries: ui_state::store::PHONE_WINDOW_MAX_ENTRIES,
        },
    );
    let effects = update(&mut model, Msg::Chat(ChatCommand::Open { agent: AGENT }));
    let (attempt, op) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Load { attempt, op, .. }) => Some((*attempt, *op)),
            _ => None,
        })
        .expect("opening a chat loads it from the store");
    macro_rules! empty_loaded {
        ($fold:ty, $variant:ident) => {
            LoadedDto::$variant(Loaded::<$fold> {
                generations: GENERATIONS,
                fence: 0,
                content_revision: 0,
                segment_high_water: 0,
                head: HeadState::None,
                window: Vec::new(),
                boundaries: Vec::new(),
                first_page: None,
                aliases: Vec::new(),
                host: None,
                progress: None,
            })
        };
    }
    let loaded = match protocol {
        ui_state::StructuredProtocol::ClaudePtyTranscript => {
            empty_loaded!(fold::claude_pty::ClaudeFold, Claude)
        }
        ui_state::StructuredProtocol::ClaudeSdk => {
            empty_loaded!(fold::claude_sdk::ClaudeSdkFold, ClaudeSdk)
        }
        ui_state::StructuredProtocol::Codex => {
            empty_loaded!(fold::codex::CodexFold, Codex)
        }
    };
    let effects = update(
        &mut model,
        Msg::Store(StoreMsg::Loaded {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: AGENT,
            loaded: Box::new(loaded),
        }),
    );
    let stream = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::OpenStoreStream { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .expect("the chat opens its stream");
    update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::Opened {
                facts: ui_state::ReplayFactsDto {
                    retained_from: 1,
                    through: rows.len() as u64,
                    selected_from: 1,
                    reset_at: 0,
                    outcome: ui_state::ReplayOutcomeDto::Continuous,
                },
                at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            },
        },
    );
    let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::Batch {
                at,
                entries: rows
                    .into_iter()
                    .enumerate()
                    .map(|(seq, row)| StreamEntry::observed(seq as u64 + 1, at, row))
                    .collect(),
            },
        },
    );
    let (op, mutations) = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Store(StoreOp::Commit { op, mutations, .. }) => Some((op, mutations)),
            _ => None,
        })
        .expect("stored rows commit");
    macro_rules! canonical {
        ($mutations:expr) => {{
            let mut oracle = MutationOracle::default();
            let placed = oracle.apply(&$mutations).expect("valid fixture mutations");
            let bodies = oracle
                .entries()
                .into_iter()
                .map(|entry| Stored {
                    key: entry.key,
                    segment: entry.segment,
                    order: entry.order,
                    revision: entry.revision,
                    entry: JsonBytes(postcard::to_allocvec(&entry.entry).unwrap()),
                })
                .collect();
            (placed, bodies)
        }};
    }
    let (placed, bodies) = match mutations {
        MutationBatchDto::Claude(mutations) => canonical!(mutations),
        MutationBatchDto::ClaudeSdk(mutations) => canonical!(mutations),
        MutationBatchDto::Codex(mutations) => canonical!(mutations),
    };
    update(
        &mut model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: AGENT,
            result: CommitResult {
                expected: ExpectedHead::Present {
                    fence: 1,
                    version: 1,
                },
                content_revision: 1,
                placed,
                bodies,
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        }),
    );
    let effects = update(
        &mut model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::ReplayComplete { at },
        },
    );
    if let Some((op, mutations)) = effects.into_iter().find_map(|effect| match effect {
        Effect::Store(StoreOp::Commit { op, mutations, .. }) => Some((op, mutations)),
        _ => None,
    }) {
        let (placed, bodies) = match mutations {
            MutationBatchDto::Claude(mutations) => canonical!(mutations),
            MutationBatchDto::ClaudeSdk(mutations) => canonical!(mutations),
            MutationBatchDto::Codex(mutations) => canonical!(mutations),
        };
        update(
            &mut model,
            Msg::Store(StoreMsg::Committed {
                profile: ProfileGeneration(0),
                attempt,
                op,
                agent: AGENT,
                result: CommitResult {
                    expected: ExpectedHead::Present {
                        fence: 2,
                        version: 2,
                    },
                    content_revision: 2,
                    placed,
                    bodies,
                    deleted: Vec::new(),
                    redirected: Vec::new(),
                    boundaries: Vec::new(),
                },
            }),
        );
    }
    (model, stream)
}
/// The same window as a healthy store holds it before any live fold exists:
/// what a phone has to paint a remembered conversation from. The failed load
/// only avoids building a durable load by hand.
fn persisted(model: &Model) -> Model {
    let mut value = serde_json::to_value(model).unwrap();
    value["agents"][AGENT.to_string()]["layer"] = Value::Null;
    serde_json::from_value(value).unwrap()
}
fn chat_row(model: &mut Model, stream: ui_state::StreamAttempt, seq: u64, payload: Value) {
    use fold::{
        CommitResult, ExpectedHead, JsonBytes, MutationOracle, RedirectState, Revision, Stored,
    };
    use ui_state::{Effect, MutationBatchDto, ProfileGeneration, StoreMsg, StoreOp, StoredDto};

    let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let effects = update(
        model,
        Msg::ChatStream {
            agent: AGENT,
            attempt: stream,
            event: ui_state::ChatStreamMsg::Batch {
                at,
                entries: vec![StreamEntry::observed(seq, at, payload)],
            },
        },
    );
    let Some((attempt, op, mutations)) = effects.into_iter().find_map(|effect| match effect {
        Effect::Store(StoreOp::Commit {
            attempt,
            op,
            mutations,
            ..
        }) => Some((attempt, op, mutations)),
        _ => None,
    }) else {
        return;
    };
    macro_rules! canonical {
        ($mutations:expr, $variant:ident) => {{
            let chat = model.chat(AGENT).expect("chat window");
            let entries = chat
                .entries
                .iter()
                .filter_map(|stored| match stored {
                    StoredDto::$variant(stored) => Some((**stored).clone()),
                    _ => None,
                })
                .collect();
            let redirects = chat
                .aliases
                .iter()
                .cloned()
                .map(|(from, to)| RedirectState {
                    from,
                    to,
                    revision: Revision {
                        seq: 0,
                        fence: 0,
                        ordinal: 0,
                    },
                    promote: None,
                })
                .collect();
            let mut oracle = MutationOracle::from_state(
                chat.segment_high_water.max(1),
                fold::DESKTOP_ENTRY_MAX_BYTES,
                entries,
                Vec::new(),
                redirects,
            )
            .expect("valid fixture window");
            let placed = oracle.apply(&$mutations).expect("valid fixture mutations");
            let placed_keys = placed
                .iter()
                .map(|placement| &placement.key)
                .collect::<std::collections::BTreeSet<_>>();
            let bodies = oracle
                .entries()
                .into_iter()
                .filter(|entry| placed_keys.contains(&entry.key))
                .map(|entry| Stored {
                    key: entry.key,
                    segment: entry.segment,
                    order: entry.order,
                    revision: entry.revision,
                    entry: JsonBytes(postcard::to_allocvec(&entry.entry).unwrap()),
                })
                .collect();
            (placed, bodies)
        }};
    }
    let (placed, bodies) = match mutations {
        MutationBatchDto::Claude(mutations) => canonical!(mutations, Claude),
        MutationBatchDto::ClaudeSdk(mutations) => canonical!(mutations, ClaudeSdk),
        MutationBatchDto::Codex(mutations) => canonical!(mutations, Codex),
    };
    update(
        model,
        Msg::Store(StoreMsg::Committed {
            profile: ProfileGeneration(0),
            attempt,
            op,
            agent: AGENT,
            result: CommitResult {
                expected: ExpectedHead::Present {
                    fence: 2,
                    version: seq + 1,
                },
                content_revision: seq + 1,
                placed,
                bodies,
                deleted: Vec::new(),
                redirected: Vec::new(),
                boundaries: Vec::new(),
            },
        }),
    );
}

/// A chat opened through the store is read from its window: every stored
/// entry reaches the phone once, in order, and a new entry joining the window
/// appends after them instead of rewriting what the phone already drew.
#[test]
fn mobile_projection_paints_a_stored_chat_window_and_appends_to_it() {
    let (mut live, stream) = stored_claude_model(3);
    let model = persisted(&live);
    let chat = model.chat(AGENT).expect("the chat is open");
    assert_eq!(chat.entries.len(), 3);
    assert!(model.claude(AGENT).is_none());
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    assert_eq!(phone.apply_events(&collect(&mut projection, &model)), 3);
    let texts = |phone: &PhoneFeed| -> Vec<Value> {
        phone
            .rows
            .values()
            .map(|row| row["row"]["kind"]["segments"][0].clone())
            .collect()
    };
    assert_eq!(texts(&phone), vec![json!("stored"); 3]);

    chat_row(&mut live, stream, 4, message(3, "fresh"));
    let model = persisted(&live);
    let events = collect(&mut projection, &model);
    let feed = events
        .iter()
        .find_map(|event| match event {
            Event::Feed {
                base,
                append,
                replace,
                ..
            } => Some((*base, append.len(), replace.len())),
            _ => None,
        })
        .expect("the new entry reaches the phone");
    assert_eq!(feed, (3, 1, 0));
    phone.apply_events(&events);
    assert_eq!(texts(&phone).last(), Some(&json!("fresh")));
    assert!(collect(&mut projection, &model).is_empty());
}

/// Moves the stored entries from `from` onward into the next segment and marks
/// that segment as starting after missing history: the window a store holds
/// once a reconnect found the machine no longer had the rows in between.
fn after_a_gap(model: &Model, from: usize) -> Model {
    let mut value = serde_json::to_value(model).unwrap();
    let chat = &mut value["store"]["chats"][AGENT.to_string()];
    let entries = chat["entries"].as_array_mut().unwrap();
    let segment = entries[0].as_object().unwrap().values().next().unwrap()["segment"]
        .as_u64()
        .unwrap();
    for entry in &mut entries[from..] {
        for stored in entry.as_object_mut().unwrap().values_mut() {
            stored["segment"] = json!(segment + 1);
        }
    }
    chat["boundaries"] = serde_json::to_value([ui_state::BoundaryAt {
        segment: segment as u32 + 1,
        before: None,
        boundary: ui_state::Boundary::Gap,
    }])
    .unwrap();
    serde_json::from_value(value).unwrap()
}

/// A stored conversation whose history is broken shows the break where it is,
/// for every provider: the entries before it, a missing-history row, then the
/// entries after it. New entries joining the window append after them without
/// renumbering anything the phone already drew.
#[test]
fn mobile_projection_draws_a_stored_history_gap_between_the_entries_it_separates() {
    let sdk_result = || json!({"type":"result", "subtype":"success", "is_error":false});
    let codex_message = |id: &str, text: &str| {
        json!({"type":"item/completed", "item":{"id":id, "type":"agentMessage", "text":text,
            "phase":"final_answer"}})
    };
    let providers = [
        (
            "claude_pty",
            model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            vec![
                message(0, "before gap"),
                message(1, "after gap"),
                message(2, "fresh"),
            ],
        ),
        (
            "claude_sdk",
            model::AgentKind::Claude {
                driver: model::ClaudeDriver::Sdk,
            },
            vec![
                message(0, "before gap"),
                sdk_result(),
                message(1, "after gap"),
                sdk_result(),
                message(2, "fresh"),
            ],
        ),
        (
            "codex",
            model::AgentKind::Codex,
            vec![
                codex_message("a", "before gap"),
                codex_message("b", "after gap"),
                codex_message("c", "fresh"),
            ],
        ),
    ];
    for (layer, kind, mut rows) in providers {
        let fresh = rows.pop().unwrap();
        let (mut live, stream) = stored_model(kind, rows.clone());
        let entries = live.chat(AGENT).unwrap().entries.clone();
        let split = entries
            .iter()
            .position(|entry| serde_json::to_string(entry).unwrap().contains("after gap"))
            .unwrap_or_else(|| panic!("{layer}: no stored entry after the gap: {entries:?}"));
        assert!(split > 0, "{layer}: {entries:?}");
        let model = after_a_gap(&persisted(&live), split);

        let mut projection = subscribed();
        let mut phone = PhoneFeed::default();
        phone.apply_events(&collect(&mut projection, &model));
        let drawn: Vec<_> = phone.rows.values().cloned().collect();
        let marker = drawn
            .iter()
            .position(|row| row["layer"] == "history")
            .unwrap_or_else(|| panic!("{layer}: no history break reached the phone: {drawn:?}"));
        assert_eq!(drawn[marker]["row"]["boundary"], "missing", "{layer}");
        assert_eq!(
            drawn.iter().filter(|row| row["layer"] == "history").count(),
            1,
            "{layer}: {drawn:?}"
        );
        assert!(
            drawn[..marker]
                .iter()
                .all(|row| row["layer"] == layer && !row.to_string().contains("after gap"))
                && drawn[..marker]
                    .iter()
                    .any(|row| row.to_string().contains("before gap")),
            "{layer}: {drawn:?}"
        );
        assert!(
            drawn[marker + 1..]
                .iter()
                .all(|row| row["layer"] == layer && !row.to_string().contains("before gap"))
                && drawn[marker + 1..]
                    .iter()
                    .any(|row| row.to_string().contains("after gap")),
            "{layer}: {drawn:?}"
        );

        let next = rows.len() as u64 + 1;
        chat_row(&mut live, stream, next, fresh);
        let model = after_a_gap(&persisted(&live), split);
        let events = collect(&mut projection, &model);
        let before = phone.rows.clone();
        phone.apply_events(&events);
        let (base, replaced) = events
            .iter()
            .find_map(|event| match event {
                Event::Feed { base, replace, .. } => Some((*base, replace.len())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{layer}: the fresh entry never reached the phone"));
        assert_eq!(
            (base, replaced),
            (drawn.len() as u64, 0),
            "{layer}: the window was renumbered"
        );
        assert!(
            before
                .iter()
                .all(|(id, row)| phone.rows.get(id) == Some(row)),
            "{layer}: rows already drawn changed"
        );
        assert!(
            phone
                .rows
                .values()
                .last()
                .unwrap()
                .to_string()
                .contains("fresh"),
            "{layer}"
        );
    }
}
