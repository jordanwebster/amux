//! A dump bundle a real daemon wrote for a real agent on the fake Claude
//! SDK provider (two turns, thinking and streamed text), replayed through
//! its three pure stages.

use std::path::Path;

use replay_support::bundle::{AgentDump, Bundle, final_texts};
use ui_view::RowKind;
use wire::Kind;

fn bundle() -> Bundle {
    Bundle::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle"))
        .expect("the fixture bundle opens")
}

#[test]
fn the_facts_replay_through_the_interpreter_to_what_the_journal_holds() {
    let bundle = bundle();
    let [agent] = bundle.agents.as_slice() else {
        panic!("one agent in the fixture");
    };
    assert_eq!(agent.row.kind(), Kind::ClaudeSdk);
    let replayed = agent.replay_facts().expect("the facts replay");
    assert!(!replayed.is_empty());
    // The ring starts after the interpreter's first frame, which it wrote
    // before any fact: everything after it comes back from the facts alone.
    let journaled = final_texts(&agent.journal[1..]);
    let from_facts = final_texts(&replayed);
    assert_eq!(from_facts, journaled);
    assert!(
        from_facts.values().any(|text| text == "second answer more"),
        "{from_facts:#?}"
    );
    let snapshot = |steps: &[wire::Step]| {
        steps
            .iter()
            .rev()
            .find_map(|step| step.snapshot.clone())
            .map(|snapshot| (snapshot.phase, snapshot.body))
    };
    assert_eq!(
        snapshot(&replayed),
        snapshot(&agent.journal),
        "the replay ends in the journal's snapshot"
    );
}

#[test]
fn the_store_slice_replays_through_the_session_model_and_the_views() {
    let bundle = bundle();
    let agent = &bundle.agents[0];
    let state = agent.session();
    assert!(state.caught_up());
    assert!(state.has_snapshot());
    let held: Vec<&str> = state
        .transcript()
        .iter()
        .map(|held| held.item.key.as_str())
        .collect();
    let stored: Vec<&str> = agent
        .store
        .items
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(held, stored, "every stored row is held, in order");

    let rows = AgentDump::rows(&state);
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, held, "one row per item, keyed by it");
    let prose: Vec<&str> = rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Prose { .. }))
        .map(|row| row.id.as_str())
        .collect();
    assert!(!prose.is_empty());
    for key in prose {
        assert!(
            state
                .transcript()
                .get(key)
                .unwrap()
                .item
                .text
                .ends_with("more"),
            "{key}"
        );
    }
    for row in &rows {
        println!("{:>3} {:<44} {:?}", row.order, row.id, row.kind);
    }
}
