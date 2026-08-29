use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use replay_support::{
    IoDirection, IoEvent, MANIFEST_SCHEMA_VERSION, Manifest, Observed, Recorded, Recording,
    RedactionSummary, ReplayAdvance, ReplayClock, ReplayOptions, SourceKind,
    replay_transport_with_controller, strict_replay,
};
use semver::Version;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn event(us: u64, direction: IoDirection, line: &str, transport_id: &str) -> IoEvent {
    IoEvent {
        us,
        direction,
        line: line.to_string(),
        transport_id: Some(transport_id.to_string()),
        session_id: None,
    }
}

fn recording(io: Vec<IoEvent>) -> Recording {
    Recording {
        dir: PathBuf::from("synthetic"),
        manifest: Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            spec: "strict_replay".to_string(),
            recorded: Recorded {
                provider: "synthetic".to_string(),
                version: Version::new(1, 0, 0),
                model: "none".to_string(),
                at: DateTime::UNIX_EPOCH,
                source_kind: SourceKind::LiveCapture,
            },
            verified: Vec::new(),
            content: BTreeMap::new(),
            observed: Observed::default(),
            redaction: RedactionSummary::default(),
            session_ids: Vec::new(),
            provider_extra: serde_json::Map::new(),
        },
        io,
    }
}

#[tokio::test]
async fn strict_replay_reports_complete_consumption() {
    let script = vec![
        event(10, IoDirection::Read, r#"{"ready":true}"#, "session-a"),
        event(
            20,
            IoDirection::Write,
            r#"{"request":"turn","id":7}"#,
            "session-a",
        ),
        event(30, IoDirection::Read, r#"{"done":true}"#, "session-a"),
    ];
    let (mut reader, mut writer, controller) =
        replay_transport_with_controller(script, ReplayOptions::default());

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"ready\":true}\n");

    writer
        .write_all(b"{\"id\":7,\"request\":\"turn\"}\n")
        .await
        .unwrap();
    writer.flush().await.unwrap();

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 30 }
    );
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"done\":true}\n");

    let report = controller.finish().unwrap();
    assert!(report.is_complete());
    assert_eq!(report.validated_writes, 1);
    assert_eq!(report.delivered_reads, 2);
}

#[test]
fn strict_replay_rejects_leftover_reads() {
    let (_, _, controller) = replay_transport_with_controller(
        vec![event(10, IoDirection::Read, "unread", "session-a")],
        ReplayOptions::default(),
    );

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.remaining_reads, 1);
}

#[test]
fn strict_replay_rejects_unwritten_expectations() {
    let (_, _, controller) = replay_transport_with_controller(
        vec![event(10, IoDirection::Write, "expected", "session-a")],
        ReplayOptions::default(),
    );

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.remaining_writes, 1);
}

#[tokio::test]
async fn strict_replay_rejects_trailing_writes() {
    let (_, mut writer, controller) =
        replay_transport_with_controller(Vec::new(), ReplayOptions::default());

    writer.write_all(b"unexpected\n").await.unwrap();
    assert!(writer.flush().await.is_err());

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.trailing_writes, vec!["unexpected"]);
}

#[tokio::test]
async fn strict_replay_rejects_partial_trailing_output() {
    let (_, mut writer, controller) =
        replay_transport_with_controller(Vec::new(), ReplayOptions::default());

    writer.write_all(b"partial").await.unwrap();

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.trailing_output.as_deref(), Some("partial"));
}

#[test]
fn strict_replay_rejects_unused_transports() {
    let (_, _, controller) = replay_transport_with_controller(
        vec![event(10, IoDirection::Read, "unread", "unused-session")],
        ReplayOptions::default(),
    );

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.unused_transports, vec!["unused-session"]);
}

#[test]
fn strict_replay_rejects_undeclared_skipped_notifications() {
    let (_, _, controller) = replay_transport_with_controller(Vec::new(), ReplayOptions::default());
    controller.record_skipped_notification(serde_json::json!({"kind": "progress"}));

    let error = controller.finish().unwrap_err();
    assert_eq!(
        error.report.skipped_notifications,
        vec![serde_json::json!({"kind": "progress"})]
    );
}

#[test]
fn strict_replay_allows_explicit_notification_ignores() {
    let (_, _, controller) = replay_transport_with_controller(Vec::new(), ReplayOptions::default());
    controller.ignore_notification(
        serde_json::json!({"kind": "heartbeat"}),
        "heartbeat is outside this journey",
    );

    let report = controller.finish().unwrap();
    assert_eq!(report.explicit_ignores.len(), 1);
    assert_eq!(
        report.explicit_ignores[0].reason,
        "heartbeat is outside this journey"
    );
}

#[tokio::test]
async fn strict_replay_scheduling_is_deterministic_across_transports() {
    let (mut slow_reader, _, slow) = replay_transport_with_controller(
        vec![event(20, IoDirection::Read, "slow", "slow")],
        ReplayOptions::default(),
    );
    let (mut fast_reader, _, fast) = replay_transport_with_controller(
        vec![event(10, IoDirection::Read, "fast", "fast")],
        ReplayOptions::default(),
    );
    let clock = ReplayClock::new(None);
    clock.register(slow.clone());
    clock.register(fast.clone());

    assert_eq!(
        clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    fast_reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "fast\n");

    assert_eq!(
        clock.advance_for(Duration::from_micros(10)).await,
        ReplayAdvance::Advanced { event_us: 20 }
    );
    line.clear();
    slow_reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "slow\n");

    assert_eq!(clock.current_us(), Some(20));
    slow.finish().unwrap();
    fast.finish().unwrap();
}

#[tokio::test]
async fn strict_replay_accepts_concurrent_writes_in_either_order() {
    let script = vec![
        event(10, IoDirection::Read, r#"{"ask":"handshake"}"#, "session-a"),
        event(
            20,
            IoDirection::Write,
            r#"{"type":"user","message":"hello"}"#,
            "session-a",
        ),
        event(
            30,
            IoDirection::Write,
            r#"{"type":"control_response","response":{"request_id":"r1"}}"#,
            "session-a",
        ),
        event(40, IoDirection::Read, r#"{"done":true}"#, "session-a"),
    ];
    let (mut reader, mut writer, controller) =
        replay_transport_with_controller(script, ReplayOptions::default());

    controller.advance_one().await;
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    writer
        .write_all(b"{\"type\":\"control_response\",\"response\":{\"request_id\":\"r1\"}}\n")
        .await
        .unwrap();
    writer
        .write_all(b"{\"type\":\"user\",\"message\":\"hello\"}\n")
        .await
        .unwrap();
    writer.flush().await.unwrap();

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 40 }
    );
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"done\":true}\n");
    controller.finish().unwrap();
}

#[tokio::test]
async fn strict_replay_rejects_a_write_that_crosses_a_read() {
    let script = vec![
        event(10, IoDirection::Write, r#"{"first":true}"#, "session-a"),
        event(20, IoDirection::Read, r#"{"between":true}"#, "session-a"),
        event(30, IoDirection::Write, r#"{"second":true}"#, "session-a"),
    ];
    let (_, mut writer, controller) =
        replay_transport_with_controller(script, ReplayOptions::default());

    writer.write_all(b"{\"second\":true}\n").await.unwrap();
    assert!(writer.flush().await.is_err());
    assert!(controller.finish().is_err());
}

#[tokio::test]
async fn strict_replay_rejects_reordering_writes_from_one_origin() {
    let script = vec![
        event(
            10,
            IoDirection::Write,
            r#"{"type":"user","message":"go"}"#,
            "session-a",
        ),
        event(
            20,
            IoDirection::Write,
            r#"{"type":"control_request","request":{"subtype":"reload_skills"}}"#,
            "session-a",
        ),
    ];
    let (_, mut writer, controller) =
        replay_transport_with_controller(script, ReplayOptions::default());

    writer
        .write_all(b"{\"type\":\"control_request\",\"request\":{\"subtype\":\"reload_skills\"}}\n")
        .await
        .unwrap();
    assert!(writer.flush().await.is_err());
    assert!(controller.finish().is_err());
}

#[tokio::test]
async fn strict_replay_reports_write_mismatch_without_panicking() {
    let (_, mut writer, controller) = replay_transport_with_controller(
        vec![event(10, IoDirection::Write, "expected", "session-a")],
        ReplayOptions::default(),
    );

    writer.write_all(b"actual\n").await.unwrap();
    assert!(writer.flush().await.is_err());

    let mismatch = &controller.finish().unwrap_err().report.write_mismatches[0];
    assert_eq!(mismatch.index, 0);
    assert_eq!(mismatch.expected, "expected");
    assert_eq!(mismatch.actual, "actual");
}

#[tokio::test]
async fn strict_replay_builds_named_transports_over_a_recording() {
    let mut replay = strict_replay(
        &recording(vec![
            event(10, IoDirection::Read, "alpha-ready", "alpha"),
            event(20, IoDirection::Read, "beta-ready", "beta"),
        ]),
        ReplayOptions::default(),
    );
    assert_eq!(
        replay.transports.keys().cloned().collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    let mut alpha = replay.transports.remove("alpha").unwrap();
    let mut beta = replay.transports.remove("beta").unwrap();

    assert_eq!(
        replay.clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    alpha.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "alpha-ready\n");

    assert_eq!(
        replay.clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 20 }
    );
    line.clear();
    beta.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "beta-ready\n");

    replay.controller.finish().unwrap();
}
