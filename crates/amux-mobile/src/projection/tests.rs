use amux_ui::{Msg, ServerMsg, StreamEntry, StreamMsg, update};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;

const AGENT: AgentId = Uuid::from_u128(1);
const HOST: AgentId = Uuid::from_u128(2);

fn model(kind: amux::AgentKind) -> Model {
    let mut model = Model::default();
    for msg in [
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(HOST),
        }),
        Msg::Server(ServerMsg::HostUpserted {
            host: amux::HostEntry {
                id: HOST,
                name: "studio".into(),
                online: true,
                version: None,
                capabilities: None,
                trust_status: amux::HostTrustStatus::Trusted,
                last_dial_error: None,
            },
        }),
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
                parent: None,
                working_on: None,
            },
        }),
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
    model(amux::AgentKind::Claude {
        driver: amux::ClaudeDriver::Pty,
    })
}
fn row(model: &mut Model, seq: u64, payload: Value) {
    update(
        model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Batch {
                at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                entries: vec![StreamEntry { seq, payload }],
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
    let mut model = claude_model();
    row(&mut model, 1, message(0, "Hello"));
    let mut projection = subscribed();
    let mut events = vec![Event::connection(&RelayConnection::Connecting)];
    events.extend(collect(&mut projection, &model));
    events.push(Event::TokenRequest { request_id: 7 });
    events.push(Event::OpResult {
        op: OpId(Uuid::from_u128(3)),
        outcome: OpOutcomeDto::Shared(Box::new(OpOutcome::InputSent)),
    });
    events.push(Event::OpResult {
        op: OpId(Uuid::from_u128(4)),
        outcome: OpOutcomeDto::Shared(Box::new(OpOutcome::Error {
            error: amux_ui::OpError::general("send refused"),
        })),
    });
    events.push(Event::Diff {
        agent: AGENT,
        document: diff::parse_unified_patch("@@ -1 +1 @@\n-old\n+new\n", false),
    });
    row(&mut model, 2, message(0, "Updated"));
    events.extend(collect(&mut projection, &model));
    let mut codex = model_with_codex_row();
    events.extend(collect(&mut subscribed(), &codex));
    row(
        &mut codex,
        3,
        json!({"type":"item/completed", "item":{"id":"m", "type":"agentMessage", "text":"Done", "phase":"final_answer"}}),
    );
    let sdk = self::model(amux::AgentKind::Claude {
        driver: amux::ClaudeDriver::Sdk,
    });
    events.extend(collect(&mut subscribed(), &sdk));
    events.push(Event::connection(&RelayConnection::Disconnected {
        reason: "relay unavailable".into(),
    }));
    events.push(Event::Invariant {
        detail: "example diagnostic".into(),
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
    // An unsupported SDK must never accept a PTY row by structural coincidence.
    let mut wrong = serde_json::to_value(FeedEntryDto::ClaudePty(
        model
            .claude(AGENT)
            .unwrap()
            .entries()
            .next()
            .unwrap()
            .clone(),
    ))
    .unwrap();
    wrong["layer"] = json!("claude_sdk");
    assert!(serde_json::from_value::<FeedEntryDto>(wrong).is_err());
    println!("mobile projection schema:\n{actual}");
}

fn model_with_codex_row() -> Model {
    let mut model = model(amux::AgentKind::Codex);
    row(&mut model, 1, json!({"type":"amux.codex_ready"}));
    row(
        &mut model,
        2,
        json!({"type":"item/started", "item":{"id":"m", "type":"agentMessage", "text":"Hello", "phase":"final_answer"}}),
    );
    model
}

#[test]
fn mobile_projection_replaces_codex_deltas_without_appending_duplicate_rows() {
    let mut model = model_with_codex_row();
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    assert_eq!(phone.apply_events(&collect(&mut projection, &model)), 1);
    for seq in 3..20 {
        row(
            &mut model,
            seq,
            json!({"type":"item/agentMessage/delta", "itemId":"m", "delta":"!"}),
        );
    }
    let events = collect(&mut projection, &model);
    let feed = events
        .iter()
        .find(|e| matches!(e, Event::Feed { .. }))
        .unwrap();
    assert!(
        matches!(feed, Event::Feed { append, replace, .. } if append.is_empty() && replace.len() == 1)
    );
    phone.apply_events(&events);
    assert_eq!(phone.rows.len(), 1);
    assert_eq!(
        phone.rows[&0]["row"]["kind"]["text"],
        format!("Hello{}", "!".repeat(17))
    );
    assert!(collect(&mut projection, &model).is_empty());
}

#[test]
fn mobile_projection_eviction_replay_and_unsubscribe_reconstruct_exactly() {
    let mut model = claude_model();
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    for id in 0..1050 {
        row(&mut model, id as u64 + 1, message(id, "row"));
        phone.apply_events(&collect(&mut projection, &model));
    }
    let layer = model.claude(AGENT).unwrap();
    assert!(layer.evicted_entries() > 0);
    assert_eq!(phone.rows.len(), layer.entry_count());
    let before_end = *phone.rows.last_key_value().unwrap().0 + 1;
    update(
        &mut model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Opened { truncated: false },
        },
    );
    row(&mut model, 1, message(0, "new window"));
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows.len(), 1);
    assert_eq!(*phone.rows.first_key_value().unwrap().0, before_end);
    assert_eq!(
        phone.rows[&before_end]["row"]["kind"]["segments"],
        json!(["new window"])
    );
    projection.unsubscribe(AGENT);
    row(&mut model, 2, message(1, "hidden"));
    assert!(
        !collect(&mut projection, &model)
            .iter()
            .any(|e| matches!(e, Event::Feed { .. } | Event::Session(_)))
    );
    projection.subscribe(AGENT);
    let mut fresh = PhoneFeed::default();
    assert_eq!(fresh.apply_events(&collect(&mut projection, &model)), 2);
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
        let mut model = claude_model();
        let mut projection = subscribed();
        let mut cadence = Cadence::new(interval);
        let mut phone = PhoneFeed::default();
        let mut bytes = 0;
        let mut sent_rows = 0;
        let mut times = Vec::new();
        let start = Instant::now();
        let mut dirty = false;
        let mut id = 0;
        while id < 1000 || dirty {
            let next_row = start + Duration::from_millis(id * 20);
            let next_frame = cadence.deadline();
            if dirty && (id == 1000 || next_frame <= next_row) {
                tokio::time::sleep_until(next_frame).await;
                let events = collect(&mut projection, &model);
                if !events.is_empty() {
                    let batch = serde_json::to_vec(&events).unwrap();
                    assert!(batch.len() < 3000, "one frame included retained history");
                    bytes += batch.len();
                    sent_rows +=
                        phone.apply(&serde_json::from_slice::<Vec<Value>>(&batch).unwrap());
                    times.push(Instant::now());
                    cadence.emitted();
                }
                dirty = false;
            } else {
                tokio::time::sleep_until(next_row).await;
                update(
                    &mut model,
                    Msg::Stream {
                        agent: AGENT,
                        event: StreamMsg::Batch {
                            at: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
                                + chrono::TimeDelta::milliseconds(id as i64 * 20),
                            entries: vec![StreamEntry {
                                seq: id + 1,
                                payload: message(id as usize, &format!("row {id:04}")),
                            }],
                        },
                    },
                );
                dirty = true;
                id += 1;
            }
        }
        assert_eq!(phone.rows.len(), 1000);
        assert_eq!(sent_rows, 1000, "unchanged rows were serialized again");
        assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= interval));
        assert!(
            bytes < 1000 * 1500,
            "payload must grow with delta bytes: {bytes}"
        );
        let native: Vec<_> = model
            .claude(AGENT)
            .unwrap()
            .entries()
            .map(|e| serde_json::to_value(FeedEntryDto::ClaudePty(e.clone())).unwrap())
            .collect();
        assert_eq!(phone.rows.values().cloned().collect::<Vec<_>>(), native);
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

#[test]
fn mobile_projection_op_results_and_diff_are_delivered_once() {
    let mut model = claude_model();
    let mut projection = subscribed();
    let op = OpId(Uuid::from_u128(9));
    let artifact: amux_ui::ArtifactId = serde_json::from_value(json!(
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ))
    .unwrap();
    update(
        &mut model,
        Msg::Command {
            op,
            command: Command::FetchDiff {
                agent: AGENT,
                id: artifact.clone(),
            },
        },
    );
    update(
        &mut model,
        Msg::OpResult {
            op,
            outcome: OpOutcome::DiffFetched {
                id: artifact,
                patch: "@@ -1 +1 @@\n-old\n+new\n".into(),
            },
        },
    );
    let mut events = vec![];
    projection.outcomes(&model, &mut events);
    assert!(
        matches!(events.as_slice(), [Event::OpResult { .. }, Event::Diff { document, .. }] if document.line_count() == 2)
    );
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
            command: Command::Queue(amux_ui::QueueCommand::Hold {
                agent: AGENT,
                draft: amux_ui::Draft {
                    text: "next step".into(),
                    attachments: vec![],
                },
            }),
        },
    );
    let held = collect(&mut projection, &model);
    assert!(held.iter().any(|event| matches!(event, Event::Session(session) if session.queue.as_ref().is_some_and(|queue| queue.draft.text == "next step"))));
    println!(
        "mobile held queue callback:\n{}",
        serde_json::to_string_pretty(&held).unwrap()
    );
    update(
        &mut model,
        Msg::Command {
            op: OpId(Uuid::from_u128(2)),
            command: Command::Queue(amux_ui::QueueCommand::Cancel { agent: AGENT }),
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
    assert!(cancelled.iter().any(|event| matches!(event, Event::OpResult { outcome: OpOutcomeDto::Shared(outcome), .. } if matches!(&**outcome, OpOutcome::QueueCancelled { draft } if draft.text == "next step"))));
}

#[test]
fn model_effort_mobile_session_projects_recorded_choices_and_pty_gate() {
    let mut model = model(amux::AgentKind::Codex);
    let mut projection = subscribed();
    let rows: Vec<Value> = include_str!("../../../amux/tests/fixtures/model-effort/rows.jsonl")
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
        amux_ui::provider::SettingsGate::Ready
    );
    println!(
        "Mobile session callback: {}",
        serde_json::to_string_pretty(&Event::Session(selected.clone())).unwrap()
    );
    let pty = session(&claude_model(), AGENT);
    assert_eq!(
        pty.settings_gate,
        amux_ui::provider::SettingsGate::PtySettingsUnavailable
    );
    assert!(pty.provider.models.is_empty());
}
