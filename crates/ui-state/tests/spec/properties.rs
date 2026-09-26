//! Properties: replay and checkpoint equality, arrival-order independence,
//! and catch-up plus live equal to uninterrupted delivery at every cut.

use ui_state::{Msg, SessionState};
use wire::{Item, Kind, Phase, ToolState};

use crate::harness::*;

/// A live session: a snapshot, then items and appends as the broadcast
/// carries them, with each item's newest full revision tracked.
fn live_stream(kind: Kind, seed: u64) -> (Vec<Msg>, Vec<Item>) {
    let mut rng = Rng::new(seed);
    let mut msgs = vec![ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[]))];
    let mut newest: Vec<Item> = Vec::new();
    let mut revision = 1;
    for order in 1..=30u64 {
        revision += 1;
        let item = match rng.below(5) {
            0 => read(kind, order, revision),
            1 => grep(kind, order, revision),
            2 => command(kind, order, revision, ToolState::Running),
            _ => streaming(kind, order, revision, "a"),
        };
        msgs.push(ev_item(item.clone()));
        newest.push(item);
        let last = newest.last_mut().unwrap();
        // Appends touch only the newest item.
        if last.text == "a" {
            for _ in 0..rng.below(3) {
                revision += 1;
                msgs.push(ev_append(&last.key, last.revision, revision, "b"));
                last.text.push('b');
                last.revision = revision;
            }
        }
        // Revisions of an older item: a tool finishing.
        if rng.below(3) == 0 && order > 2 {
            let target = rng.below(order - 1) as usize;
            if newest[target].text.is_empty() {
                revision += 1;
                let mut done = command(kind, newest[target].order, revision, ToolState::Succeeded);
                done.key = newest[target].key.clone();
                msgs.push(ev_item(done.clone()));
                newest[target] = done;
            }
        }
    }
    (msgs, newest)
}

fn run(state: &mut SessionState, msgs: &[Msg]) {
    for msg in msgs {
        apply_checked(state, msg.clone());
    }
}

#[test]
fn replay_from_any_checkpoint_equals_the_uninterrupted_state() {
    for kind in KINDS {
        let (msgs, _) = live_stream(kind, 7);
        let mut msgs = msgs;
        msgs.insert(1, caught_up(1));
        let mut whole = SessionState::new(agent(kind));
        run(&mut whole, &msgs);
        for cut in 0..=msgs.len() {
            let mut checkpoint = SessionState::new(agent(kind));
            run(&mut checkpoint, &msgs[..cut]);
            // A clone is the trace's starting state; replaying the rest from
            // it reproduces the state exactly.
            let mut resumed = checkpoint.clone();
            run(&mut resumed, &msgs[cut..]);
            assert_eq!(resumed, whole, "{kind:?} cut {cut}");
        }
    }
}

#[test]
fn catch_up_plus_live_equals_uninterrupted_delivery_at_every_cut() {
    for kind in KINDS {
        for seed in 1..6 {
            let (msgs, _) = live_stream(kind, seed);
            let mut uninterrupted = SessionState::new(agent(kind));
            run(&mut uninterrupted, &msgs);
            apply_checked(&mut uninterrupted, caught_up(999));
            for cut in 1..=msgs.len() {
                // A late subscriber: the store's full rows at the cut, CaughtUp,
                // then the live rest from the cut, overlap included.
                let mut store = SessionState::new(agent(kind));
                run(&mut store, &msgs[..cut]);
                let mut late = SessionState::new(agent(kind));
                apply_checked(&mut late, msgs[0].clone());
                for held in store.transcript().iter() {
                    apply_checked(&mut late, ev_item(held.item.clone()));
                }
                let overlap = cut.saturating_sub(2).max(1);
                run(&mut late, &msgs[overlap..]);
                apply_checked(&mut late, caught_up(999));
                assert_eq!(
                    projection(late.transcript()),
                    projection(uninterrupted.transcript()),
                    "{kind:?} seed {seed} cut {cut}"
                );
            }
        }
    }
}

#[test]
fn full_items_arriving_in_any_order_inside_the_window_give_the_same_state() {
    for kind in KINDS {
        for seed in 1..30 {
            let mut rng = Rng::new(seed);
            let base: Vec<Msg> = (1..=12)
                .map(|order| ev_item(text(kind, order, order, "v0")))
                .collect();
            // Several revisions per key, as a re-derivation, a page and the
            // broadcast might each deliver them.
            let mut revisions: Vec<Msg> = Vec::new();
            for order in 1..=12u64 {
                for revision in 0..rng.below(4) {
                    let item = match (order + revision) % 3 {
                        0 => read(kind, order, 100 + order * 10 + revision),
                        1 => text(kind, order, 100 + order * 10 + revision, "later"),
                        _ => command(
                            kind,
                            order,
                            100 + order * 10 + revision,
                            ToolState::Succeeded,
                        ),
                    };
                    let mut item = item;
                    item.key = format!("k{order}");
                    revisions.push(ev_item(item));
                }
            }
            revisions.push(ev_snapshot(snapshot(kind, 5, Phase::Working, &[], &[])));
            revisions.push(ev_snapshot(snapshot(kind, 9, Phase::Idle, &[], &[])));
            let mut expected = SessionState::new(agent(kind));
            run(&mut expected, &base);
            let mut shuffled = revisions.clone();
            // Snapshots keep their stream order; items move freely.
            let mut in_order = SessionState::new(agent(kind));
            run(&mut in_order, &base);
            run(&mut in_order, &revisions);
            rng.shuffle(&mut shuffled);
            let snapshots: Vec<Msg> = revisions
                .iter()
                .filter(|msg| is_snapshot(msg))
                .cloned()
                .collect();
            let mut snapshots = snapshots.into_iter();
            let shuffled: Vec<Msg> = shuffled
                .into_iter()
                .map(|msg| {
                    if is_snapshot(&msg) {
                        snapshots.next().unwrap()
                    } else {
                        msg
                    }
                })
                .collect();
            run(&mut expected, &shuffled);
            assert_eq!(expected, in_order, "{kind:?} seed {seed}");
        }
    }
}

#[test]
fn a_page_and_the_live_stream_commute() {
    for kind in KINDS {
        let mut first = SessionState::new(agent(kind));
        run(
            &mut first,
            &[ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[]))],
        );
        for order in 10..=12 {
            apply_checked(&mut first, ev_item(read(kind, order, order)));
        }
        let mut second = first.clone();
        let older: Vec<Item> = (6..=9)
            .rev()
            .map(|order| read(kind, order, order))
            .collect();
        let live = [
            ev_item(read(kind, 13, 13)),
            ev_item(text(kind, 11, 20, "revised")),
            ev_item(text(kind, 14, 21, "x")),
        ];
        apply_page(&mut first, older.clone(), false);
        run(&mut first, &live);
        run(&mut second, &live);
        apply_page(&mut second, older, false);
        assert_eq!(first, second);
    }
}

fn is_snapshot(msg: &Msg) -> bool {
    matches!(msg, Msg::Event(event) if matches!(event.of, Some(wire::session_event::Of::Snapshot(_))))
}
