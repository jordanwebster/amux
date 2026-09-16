//! A recorder report is a self-contained replay bundle: replaying it twice
//! yields byte-identical Model JSON, and both equal the live Model.

use chrono::{TimeZone, Utc};
use model::{Agent, Capabilities, HostEntry, HostTrustStatus};
use ui_runtime::report::{
    ReplayVerdict, ReportDraft, ReportKind, ReportParts, ReportWriter, TraceKind,
};
use ui_runtime::{BUILD, Recorder, replay_msgs};
use ui_state::{DisconnectReason, Model, Msg, ServerMsg, StreamEntry, StreamMsg, update};
use uuid::Uuid;

fn sequence() -> Vec<Msg> {
    let host = Uuid::from_u128(1);
    let agent = Uuid::from_u128(2);
    vec![
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(host),
        }),
        Msg::Server(ServerMsg::HostUpserted {
            host: HostEntry {
                id: host,
                name: "mbp".into(),
                online: true,
                version: Some("0.4.0".into()),
                capabilities: Some(Capabilities::default()),
                trust_status: HostTrustStatus::Trusted,
                last_dial_error: None,
                platform: None,
            },
        }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: Agent {
                id: agent,
                host_id: host,
                name: Some("worker".into()),
                command: "claude".into(),
                working_dir: "/work".into(),
                kind: model::AgentKind::Claude {
                    driver: model::ClaudeDriver::Pty,
                },
                readonly: false,
                args: Vec::new(),
                created_at: Utc.timestamp_opt(0, 0).unwrap(),
                parent: None,
                working_on: None,
                summary: None,
                progress: None,
                inventory_revision: 0,
            },
        }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent,
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent,
            event: StreamMsg::Batch {
                at: Utc.timestamp_opt(1, 0).unwrap(),
                entries: vec![StreamEntry::observed(
                    1,
                    Utc.timestamp_opt(1, 0).unwrap(),
                    serde_json::json!({"type": "user", "text": "hello"}),
                )],
            },
        },
        Msg::Server(ServerMsg::Disconnected {
            reason: DisconnectReason::TransportError {
                message: "gone".into(),
            },
        }),
    ]
}

/// The recorder capacity is deliberately tiny so eviction folds Msgs into the
/// checkpoint on the way; the replay must still land on the live Model.
#[test]
fn replaying_a_recorded_log_twice_yields_identical_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut live = Model::default();
    let mut recorder = Recorder::new(4, &live);

    for msg in sequence() {
        recorder.record(&msg);
        update(&mut live, msg);
    }
    let report = ReportWriter::new(dir.path().to_path_buf(), BUILD, "test")
        .write(
            ReportDraft {
                kind: ReportKind::Bug,
                detail: None,
                note: String::new(),
                marks: Vec::new(),
                viewport: None,
                replay: ReplayVerdict::Unchecked,
            },
            ReportParts {
                frame: None,
                trace: None,
                trace_kind: TraceKind::TerminalChrome,
                msgs: Some(recorder.snapshot()),
                daemon: None,
                log: None,
                absent_reason: "test".to_string(),
                log_absent_reason: None,
                daemon_absent_reason: None,
            },
        )
        .expect("report written");
    let path = report.join("msgs.jsonl");

    let first = replay_msgs(&path).expect("first replay");
    let second = replay_msgs(&path).expect("second replay");
    let first_json = serde_json::to_value(&first).unwrap();
    assert_eq!(first_json, serde_json::to_value(&second).unwrap());
    assert_eq!(first_json, serde_json::to_value(&live).unwrap());
    assert_eq!(first, live);
}
