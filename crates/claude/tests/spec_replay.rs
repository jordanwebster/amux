#![cfg(specs)]

use std::collections::BTreeSet;
use std::time::Duration;

#[cfg(feature = "pty")]
use claude::specs::pty_registry;
use claude::specs::{MINIMUM_SUPPORTED, SpecSource, fixtures_root, run, sdk_registry};
use replay_support::{
    ReplayOptions, SourceKind, below_minimum, load_recording, orphan_recordings, strict_replay,
};
use semver::Version;

const SPEC_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn every_registered_sdk_specification_replays_strictly() {
    for entry in sdk_registry() {
        replay(entry.name).await;
    }
}

#[cfg(feature = "pty")]
mod pty_replays {
    use super::*;

    macro_rules! scenarios {
        ($($name:ident),+ $(,)?) => {
            const SCENARIOS: &[&str] = &[$(stringify!($name)),+];

            $(
                #[tokio::test]
                async fn $name() {
                    let entry = pty_registry().iter()
                        .find(|entry| entry.name == stringify!($name))
                        .expect("replay scenario remains registered");
                    replay_pty(entry).await;
                }
            )+
        };
    }

    scenarios!(
        prompt,
        prompt_multiline,
        tools,
        permission_allow_once,
        permission_allow_scoped,
        permission_deny_feedback,
        plan_approve,
        plan_auto,
        plan_request_changes,
        question_single,
        question_multi_other,
        question_mixed,
        question_tabs,
        question_other_single,
        interrupt,
        mode_cycle,
        compact_relink,
        clear_relink,
    );

    #[tokio::test(start_paused = true)]
    async fn recorded_preparation_and_cleanup_do_not_wait_for_live_timers() {
        let entry = pty_registry()
            .iter()
            .find(|entry| entry.name == "plan_approve")
            .unwrap();
        let started = tokio::time::Instant::now();
        replay_pty(entry).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_drains_a_recorded_tail_larger_than_the_transport_buffers() {
        let entry = pty_registry()
            .iter()
            .find(|entry| entry.name == "prompt")
            .unwrap();
        let mut recording =
            load_recording(&claude::specs::pty::fixtures_root().join(entry.recording)).unwrap();
        let us = recording.io.last().unwrap().us;
        let line = serde_json::json!({
            "row": {"type": "system", "content": "x".repeat(8192)}
        })
        .to_string();
        for index in 1..=1024 {
            recording.io.push(replay_support::IoEvent {
                us: us + index,
                direction: replay_support::IoDirection::Read,
                line: line.clone(),
                transport_id: Some("transcript".to_owned()),
                session_id: None,
            });
        }
        replay_pty_recording(entry, recording).await;
    }

    #[test]
    fn every_registered_scenario_has_a_test() {
        assert_eq!(
            SCENARIOS.iter().copied().collect::<BTreeSet<_>>(),
            pty_registry()
                .iter()
                .map(|entry| entry.name)
                .collect::<BTreeSet<_>>(),
        );
    }
}

#[cfg(feature = "pty")]
async fn replay_pty(entry: &replay_support::SpecEntry) {
    let root = claude::specs::pty::fixtures_root();
    eprintln!("replaying PTY {}", entry.name);
    let recording = load_recording(&root.join(entry.recording))
        .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
    replay_pty_recording(entry, recording).await;
}

#[cfg(feature = "pty")]
async fn replay_pty_recording(
    entry: &replay_support::SpecEntry,
    recording: replay_support::Recording,
) {
    assert!(
        entry
            .allowed_models
            .contains(&recording.manifest.recorded.model.as_str()),
        "{} was recorded with disallowed model {}",
        entry.name,
        recording.manifest.recorded.model
    );
    let replay = strict_replay(&recording, ReplayOptions::default());
    let report = tokio::time::timeout(
        SPEC_TIMEOUT,
        claude::specs::pty::run(
            entry,
            claude::specs::pty::Source::Recorded {
                replay,
                manifest: Box::new(recording.manifest),
                keymaps: claude::pty::keymap::KeymapSources::default(),
            },
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("{} stalled after {SPEC_TIMEOUT:?}", entry.name))
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        report.replay.is_some_and(|report| report.is_complete()),
        "{} did not return a complete replay report",
        entry.name
    );
}

#[tokio::test]
async fn elicited() {
    replay("tools/elicitation_accepted").await;
}

#[tokio::test]
async fn hook_lifecycle() {
    replay("tools/hook_lifecycle").await;
}

#[tokio::test]
async fn every_hook_event() {
    replay("options/every_hook_event").await;
}

async fn replay(name: &str) {
    let entry = sdk_registry()
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("missing registered specification {name}"));
    eprintln!("replaying {}", entry.name);
    let recording = load_recording(&fixtures_root().join(entry.recording))
        .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
    assert!(
        entry
            .allowed_models
            .contains(&recording.manifest.recorded.model.as_str()),
        "{} was recorded with disallowed model {}",
        entry.name,
        recording.manifest.recorded.model
    );

    let mut seen = BTreeSet::new();
    let transport_order = recording
        .io
        .iter()
        .filter_map(|event| event.transport_id.clone())
        .filter(|transport| seen.insert(transport.clone()))
        .collect::<Vec<_>>();
    let replay = strict_replay(&recording, ReplayOptions::default());
    let controller = replay.controller.clone();
    let outcome = match tokio::time::timeout(
        SPEC_TIMEOUT,
        run(
            entry,
            SpecSource::Recorded {
                replay,
                transport_order,
                session_ids: recording.manifest.session_ids.clone(),
            },
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => {
            let report = controller.finish().err().map(|error| error.report);
            panic!(
                "{} stalled after {SPEC_TIMEOUT:?}; replay={report:?}",
                entry.name
            );
        }
    };
    if let Err(error) = outcome {
        let report = controller.finish().err().map(|error| error.report);
        panic!("{error}; replay={report:?}");
    }
    let report = controller
        .finish()
        .unwrap_or_else(|error| panic!("{}: {error}: {:?}", entry.name, error.report));
    assert!(report.is_complete(), "{}: {report:?}", entry.name);
}

#[test]
fn sdk_corpus_is_inventoried_current_and_unorphaned() {
    let root = fixtures_root();
    assert_eq!(
        orphan_recordings(&root, sdk_registry()),
        Vec::<String>::new()
    );
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());

    for entry in sdk_registry() {
        let recording = load_recording(&root.join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert_eq!(recording.manifest.spec, entry.name);
        assert_eq!(recording.manifest.recorded.provider, "claude");
        assert!(recording.manifest.recorded.version >= minimum);
        assert!(
            recording
                .manifest
                .verified
                .iter()
                .all(|verification| verification.version == Version::new(2, 1, 251))
        );
        match recording.manifest.recorded.source_kind {
            SourceKind::Migrated { .. } => {
                assert_eq!(recording.manifest.recorded.version, minimum)
            }
            SourceKind::LiveCapture => {
                assert!(
                    [
                        Version::new(2, 1, 251),
                        Version::new(2, 1, 260),
                        Version::new(2, 1, 261),
                    ]
                    .contains(&recording.manifest.recorded.version),
                    "{} was captured against an unreviewed Claude version {}",
                    entry.name,
                    recording.manifest.recorded.version
                )
            }
        }
        assert!(recording.manifest.content.contains_key("io.jsonl"));
        assert!(recording.manifest.content.contains_key("spawn.jsonl"));
    }
}

#[test]
#[cfg(feature = "pty")]
fn recorded_pty_corpus_is_inventoried_current_and_unorphaned() {
    let root = claude::specs::pty::fixtures_root();
    if !root.exists() {
        return;
    }
    assert_eq!(
        orphan_recordings(&root, pty_registry()),
        Vec::<String>::new()
    );
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());
    for entry in pty_registry() {
        let recording = load_recording(&root.join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert_eq!(recording.manifest.spec, entry.name);
        assert_eq!(recording.manifest.recorded.provider, "claude");
        assert!(recording.manifest.recorded.version >= minimum);
        assert_eq!(
            recording.manifest.recorded.source_kind,
            SourceKind::LiveCapture
        );
        assert_eq!(
            recording
                .io
                .iter()
                .filter_map(|event| event.transport_id.as_deref())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["hook", "pty", "transcript"]),
            "{} does not carry all three PTY transports",
            entry.name
        );
    }
}
