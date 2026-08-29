#![cfg(feature = "specs")]

use std::collections::BTreeSet;
use std::time::Duration;

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

#[tokio::test]
async fn elicited() {
    replay("tools/elicited").await;
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
            panic!("{} stalled after {SPEC_TIMEOUT:?}; replay={report:?}", entry.name);
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
fn sdk_corpus_is_migrated_inventoried_and_unorphaned() {
    let root = fixtures_root();
    assert_eq!(orphan_recordings(&root, sdk_registry()), Vec::<String>::new());
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());

    for entry in sdk_registry() {
        let recording = load_recording(&root.join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert_eq!(recording.manifest.spec, entry.name);
        assert_eq!(recording.manifest.recorded.provider, "claude");
        assert_eq!(recording.manifest.recorded.version, minimum);
        assert!(recording.manifest.verified.is_empty());
        assert!(matches!(
            recording.manifest.recorded.source_kind,
            SourceKind::Migrated { .. }
        ));
        assert!(recording.manifest.content.contains_key("io.jsonl"));
        assert!(recording.manifest.content.contains_key("spawn.jsonl"));
    }
}
