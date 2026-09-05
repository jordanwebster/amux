//! What the account switcher and the fleet on either side of it show.
//!
//! The screenshots the operator judges switching by come from these
//! fixtures, rendered through the production view. The claim they have to
//! carry is that switching accounts shows another device: the fleet after a
//! switch must contain nothing of the account it left.

use amux_tui::fixtures::{NamedState, fixture};
use amux_tui::{ColorMode, FrameContext, Theme, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const VIEWPORT: (u16, u16) = (120, 40);

fn frame(state: NamedState) -> String {
    let built = fixture(state);
    let backend = TestBackend::new(VIEWPORT.0, VIEWPORT.1);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let context = FrameContext {
        viewport: VIEWPORT,
        theme: Theme::dark(ColorMode::TrueColor),
        now: built.now,
    };
    terminal
        .draw(|frame| render(&built.model, &built.view, &context, frame))
        .unwrap_or_else(|error| panic!("{state} failed to render: {error}"));
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer.cell((x, y)).expect("cell in area").symbol());
        }
        text.push('\n');
    }
    text
}

/// Every account on the installation, with what tells them apart: the
/// label, the address behind it and what its link is doing.
#[test]
fn the_switcher_lists_every_profile_with_label_email_and_status() {
    let frame = frame(NamedState::ProfileSwitcher);
    for expected in [
        "switch profile",
        "Personal",
        "robin@example.com",
        "Work",
        "robin@northwind.example",
        "connected",
        "Conference laptop",
        "logged out",
        "enter switch",
        "esc close",
    ] {
        assert!(
            frame.contains(expected),
            "the switcher frame does not show {expected:?}:\n{frame}"
        );
    }
}

/// The claim the pair of fleet screenshots makes. Two profiles on one
/// installation are two devices, so the account switched to shares no host
/// and no agent with the account switched away from.
#[test]
fn the_switched_fleet_keeps_no_agent_from_the_profile_the_switcher_left() {
    let personal = frame(NamedState::Fleet);
    let work = frame(NamedState::FleetSwitched);

    for left_behind in ["fix-auth", "codex-retry", "mbp"] {
        assert!(
            personal.contains(left_behind),
            "the personal fleet should show {left_behind:?}:\n{personal}"
        );
    }
    for left_behind in ["fix-auth", "codex-retry"] {
        assert!(
            !work.contains(left_behind),
            "the switched fleet still shows {left_behind:?} from the previous account:\n{work}"
        );
    }
    // The host column clips, so the name is checked by its stem.
    for arrived in ["ship-invoices", "audit-deps", "northwind"] {
        assert!(
            work.contains(arrived),
            "the switched fleet should show {arrived:?}:\n{work}"
        );
    }
}
