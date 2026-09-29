//! Run membership: two or more consecutive exploration calls, kept current
//! on every message and equal to a rebuild from the transcript.

use ui_state::{Run, SessionState};
use wire::{Kind, Phase, ToolState};

use crate::harness::*;

fn open(kind: Kind) -> SessionState {
    let mut state = SessionState::new(agent(kind), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
    );
    state
}

fn run_of(state: &SessionState, order: u64) -> Option<Run> {
    state.transcript().run_at(order)
}

#[test]
fn a_run_needs_two_consecutive_exploration_calls() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(&mut state, ev_item(text(kind, 10, 1, "looking")));
        apply_checked(&mut state, ev_item(read(kind, 11, 2)));
        assert_eq!(run_of(&state, 11), None, "one call is not a run");
        let outcome = apply_checked(&mut state, ev_item(grep(kind, 12, 3)));
        assert_eq!(outcome.changed, vec!["k12", "k11"]);
        let run = run_of(&state, 11).unwrap();
        assert_eq!(
            (run.oldest, run.newest, run.len, run.reads, run.searches),
            (11, 12, 2, 1, 1)
        );
        assert_eq!(run.newest_key, "k12");
        apply_checked(
            &mut state,
            ev_item(command(kind, 13, 4, ToolState::Succeeded)),
        );
        assert_eq!(
            run_of(&state, 13),
            None,
            "a consequential call ends the run"
        );
        apply_checked(&mut state, ev_item(read(kind, 14, 5)));
        assert_eq!(run_of(&state, 14), None);
    }
}

#[test]
fn the_summary_moves_to_the_newest_member_while_every_row_id_stays() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 1..=3 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        let keys: Vec<String> = state.transcript().keys().cloned().collect();
        let outcome = apply_checked(&mut state, ev_item(read(kind, 4, 4)));
        assert_eq!(
            outcome.changed,
            vec!["k4", "k1", "k2", "k3"],
            "every member's run attributes moved"
        );
        assert_eq!(run_of(&state, 1).unwrap().newest_key, "k4");
        let after: Vec<String> = state.transcript().keys().cloned().collect();
        assert_eq!(&after[..3], &keys[..], "row ids never move");
    }
}

#[test]
fn a_page_merging_into_a_run_at_the_edge_keeps_ids_and_grows_the_count() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 20..=21 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        apply_checked(&mut state, ev_item(text(kind, 22, 22, "found it")));
        apply_checked(&mut state, caught_up(22));
        let run = run_of(&state, 20).unwrap();
        assert!(
            run.open_below,
            "the run starts at the oldest held row with older history"
        );
        let items = (12..=19)
            .rev()
            .map(|order| read(kind, order, order))
            .collect();
        apply_page(&mut state, items, false);
        let run = run_of(&state, 21).unwrap();
        assert_eq!((run.oldest, run.newest, run.len), (12, 21, 10));
        assert_eq!(
            run.newest_key, "k21",
            "the summary the reader looks at keeps its id"
        );
        assert!(run.open_below);
        apply_page(&mut state, vec![text(kind, 11, 11, "earlier")], false);
        assert!(
            !run_of(&state, 21).unwrap().open_below,
            "a non-member below closes it"
        );
    }
}

#[test]
fn open_below_is_set_only_at_the_oldest_held_item_with_older_history() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 5..=6 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        apply_checked(&mut state, ev_item(text(kind, 7, 7, "a")));
        for order in 8..=9 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        assert!(run_of(&state, 5).unwrap().open_below);
        assert!(!run_of(&state, 8).unwrap().open_below);
        let outcome = apply_page(&mut state, vec![], true);
        assert_eq!(
            outcome.changed,
            vec!["k5", "k6"],
            "exhausted history closes the edge run"
        );
        assert!(!run_of(&state, 5).unwrap().open_below);
    }
}

#[test]
fn a_revision_that_changes_an_items_class_splits_or_merges_runs() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(&mut state, ev_item(read(kind, 1, 1)));
        apply_checked(&mut state, ev_item(command(kind, 2, 2, ToolState::Running)));
        apply_checked(&mut state, ev_item(read(kind, 3, 3)));
        assert_eq!(run_of(&state, 1), None);
        // The middle call is revised into an exploration call: one run of three.
        let outcome = apply_checked(&mut state, ev_item(grep(kind, 2, 4)));
        assert_eq!(outcome.changed.len(), 3);
        assert_eq!(run_of(&state, 3).unwrap().len, 3);
        // And back: split.
        apply_checked(
            &mut state,
            ev_item(command(kind, 2, 5, ToolState::Succeeded)),
        );
        assert_eq!(run_of(&state, 1), None);
        assert_eq!(run_of(&state, 3), None);
    }
}

#[test]
fn the_run_index_equals_a_rebuild_after_every_message() {
    // apply_checked asserts the rebuild and changed-key invariants on every
    // message; this drives it through random windows, pages and revisions.
    for kind in KINDS {
        for seed in 0..40 {
            let mut rng = Rng::new(seed + 1);
            let mut state = open(kind);
            let start = 30 + rng.below(20);
            let mut head = start;
            apply_checked(&mut state, ev_item(read(kind, head, 1)));
            let mut revision = 2;
            for _ in 0..80 {
                revision += 1;
                let order = match rng.below(4) {
                    0 | 1 => {
                        head += 1;
                        head
                    }
                    2 => start + rng.below(head - start + 1),
                    _ => {
                        let oldest = state.oldest_order().unwrap();
                        if oldest > 1 {
                            let items = (oldest.saturating_sub(1 + rng.below(3)).max(1)..oldest)
                                .map(|o| random_item(kind, &mut rng, o, revision))
                                .collect();
                            apply_page(&mut state, items, rng.below(10) == 0);
                        }
                        continue;
                    }
                };
                apply_checked(
                    &mut state,
                    ev_item(random_item(kind, &mut rng, order, revision)),
                );
            }
        }
    }
}

fn random_item(kind: Kind, rng: &mut Rng, order: u64, revision: u64) -> wire::Item {
    match rng.below(4) {
        0 => read(kind, order, revision),
        1 => grep(kind, order, revision),
        2 => command(kind, order, revision, ToolState::Succeeded),
        _ => text(kind, order, revision, "prose"),
    }
}
