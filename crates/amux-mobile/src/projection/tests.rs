use amux_ui::{Msg, ServerMsg, StreamEntry, StreamMsg, update};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;

const AGENT: AgentId = Uuid::from_u128(1);
const HOST: AgentId = Uuid::from_u128(2);

fn host(online: bool) -> Msg {
    Msg::Server(ServerMsg::HostUpserted {
        host: amux::HostEntry {
            id: HOST,
            name: "studio".into(),
            online,
            version: None,
            capabilities: None,
            trust_status: amux::HostTrustStatus::Trusted,
            last_dial_error: None,
        },
    })
}
fn upsert(kind: amux::AgentKind) -> Msg {
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
    })
}
fn model(kind: amux::AgentKind) -> Model {
    let mut model = Model::default();
    for msg in [
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(HOST),
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
        diff: serde_json::from_value(json!(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
        .unwrap(),
        document: amux_ui::review::parse_stored_patch(
            "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-old\n+new\n",
            amux::BaseIdentity {
                base: amux::DiffBase::WorkingTree,
                head: "abc".into(),
                merge_base: None,
                blobs: vec![],
            },
        )
        .unwrap(),
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
    let mut sdk = self::model(amux::AgentKind::Claude {
        driver: amux::ClaudeDriver::Sdk,
    });
    let facts =
        include_str!("../../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .rev()
            .find(|row| row["type"] == "amux.claude_sdk.session_facts")
            .unwrap();
    row(&mut sdk, 1, facts);
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

/// A machine that stops answering takes its folded layer with it. What it
/// last said is still the only account of that conversation there is, so the
/// projected feed keeps it until the machine itself replaces it; an agent that
/// is genuinely gone from a machine still answering loses its rows.
#[test]
fn mobile_projection_keeps_the_feed_of_an_agent_whose_host_has_gone_away() {
    let mut model = claude_model();
    let mut projection = subscribed();
    let mut phone = PhoneFeed::default();
    for id in 0..3 {
        row(&mut model, id as u64 + 1, message(id, "before the outage"));
    }
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows.len(), 3);
    let held = phone.rows.clone();

    // The machine goes away. The relay drops what it knew of that machine's
    // agents, so the layer these rows were folded from is gone.
    update(&mut model, host(false));
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentRemoved { id: AGENT }),
    );
    assert!(model.claude(AGENT).is_none());
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows, held, "the rows went when the machine did");

    // It answers again. Between coming back and replaying what it holds it
    // has an agent and an open stream but nothing folded, and a transcript
    // that emptied itself for those seconds would be reporting the
    // reconnection rather than the conversation.
    update(&mut model, host(true));
    for msg in [
        upsert(amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        }),
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Opened { truncated: false },
        },
    ] {
        update(&mut model, msg);
        phone.apply_events(&collect(&mut projection, &model));
        assert_eq!(
            phone.rows, held,
            "the rows went while the machine came back"
        );
    }

    // The replay is what replaces them: the same rows once, never both copies.
    update(
        &mut model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::ReplayComplete,
        },
    );
    for id in 0..3 {
        row(&mut model, id as u64 + 1, message(id, "before the outage"));
    }
    phone.apply_events(&collect(&mut projection, &model));
    assert_eq!(phone.rows.len(), 3, "the replay doubled the transcript");

    // An agent removed from a machine that is still answering is not stale,
    // it is gone, and its rows go with it.
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentRemoved { id: AGENT }),
    );
    phone.apply_events(&collect(&mut projection, &model));
    assert!(phone.rows.is_empty(), "a removed agent kept its rows");
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

/// A frozen diff reaches the phone once, as the review document the rest of
/// the workspace reads: files under their own paths, rows already numbered,
/// and the repository identity it was taken against.
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
            command: Command::RequestDiff {
                agent: AGENT,
                base: amux::DiffBase::WorkingTree,
            },
        },
    );
    update(
        &mut model,
        Msg::OpResult {
            op,
            outcome: OpOutcome::DiffReady {
                response: amux::DiffResponse {
                    artifact: amux::ArtifactRef {
                        id: artifact.clone(),
                        kind: amux::ArtifactKind::Diff,
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
                    identity: amux::BaseIdentity {
                        base: amux::DiffBase::WorkingTree,
                        head: "abc".into(),
                        merge_base: None,
                        blobs: vec![],
                    },
                    files: vec![amux::DiffFile {
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
            command: Command::Queue(amux_ui::QueueCommand::Hold {
                agent: AGENT,
                draft: amux_ui::Draft {
                    segments: vec![amux_ui::DraftSegment::Text {
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
    assert!(cancelled.iter().any(|event| matches!(event, Event::OpResult { outcome: OpOutcomeDto::Shared(outcome), .. } if matches!(&**outcome, OpOutcome::QueueCancelled { draft } if draft.text() == "next step"))));
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

#[test]
fn provider_commands_mobile_callback_exposes_reported_list() {
    let mut model = model(amux::AgentKind::Codex);
    let mut projection = subscribed();
    for (index, line) in include_str!("../../../amux/tests/fixtures/provider-commands/rows.jsonl")
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
    for (i, line) in include_str!("../../../amux-ui/tests/fixtures/todos/rows.jsonl")
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
const ASK_FIXTURES: &[(&str, amux::AgentKind, &str)] = &[
    (
        "permission",
        amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        include_str!("../../../amux/tests/fixtures/rows/claude-pty/permission_session.rows.jsonl"),
    ),
    (
        "question",
        amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        include_str!("../../../amux/tests/fixtures/rows/claude-pty/question_multi.rows.jsonl"),
    ),
    (
        "plan",
        amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        include_str!("../../../amux/tests/fixtures/rows/claude-pty/plan_approve.rows.jsonl"),
    ),
    (
        "codex-approval",
        amux::AgentKind::Codex,
        include_str!("../../../amux/tests/fixtures/rows/codex/approval_allow.rows.jsonl"),
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
