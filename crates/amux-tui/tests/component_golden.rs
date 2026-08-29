//! Full-frame goldens for the component gallery and exploration pair.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test
//! component_golden` and review both the text and semantic style maps.

use amux_tui::fixtures::{NamedState, fixture};
use amux_tui::{ColorMode, FrameContext, Theme, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const VIEWPORT: (u16, u16) = (120, 40);
const STATES: &[NamedState] = &[
    NamedState::ComponentGallery,
    NamedState::ComponentGalleryCodex,
    NamedState::ExplorationCollapsed,
    NamedState::ExplorationExpanded,
    NamedState::ClaudeScrolledBack,
    NamedState::CodexScrolledBack,
    NamedState::HelpOverlay,
];

struct Capture {
    text: String,
    styles: String,
}

fn capture(state: NamedState, theme: Theme) -> Capture {
    let fixture = fixture(state);
    let backend = TestBackend::new(VIEWPORT.0, VIEWPORT.1);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let context = FrameContext {
        viewport: VIEWPORT,
        theme,
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    let mut styles = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, y)).expect("cell in area");
            text.push_str(cell.symbol());
            styles.push(theme.classify(cell.style()));
        }
        text.push('\n');
        styles.push('\n');
    }
    Capture { text, styles }
}

fn golden_stem(state: NamedState, theme: &str) -> String {
    format!("{}_{}", state.name().replace('-', "_"), theme)
}

fn assert_golden(name: &str, rendered: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name} — run with UPDATE_GOLDENS=1"));
    assert_eq!(rendered, expected, "frame {name} diverged");
}

fn changed_rows_are_diff_rows(capture: &Capture, page: &str) {
    for class in ['-', '+'] {
        let marker = class.to_string();
        let mut changed_rows = 0;
        for (row, (text, styles)) in capture.text.lines().zip(capture.styles.lines()).enumerate() {
            if styles.contains(class) {
                changed_rows += 1;
                assert!(
                    text.split_whitespace().any(|field| field == marker),
                    "{page} row {row} uses the {class:?} diff class without a {class:?} diff marker:\n{text}\n{styles}"
                );
            }
        }
        assert!(
            changed_rows > 0,
            "{page} should show at least one {class:?} diff row"
        );
    }
}

fn changed_rows_have_numbered_gutters(capture: &Capture, page: &str) {
    for class in ['-', '+'] {
        let marker = class.to_string();
        for (row, (text, styles)) in capture.text.lines().zip(capture.styles.lines()).enumerate() {
            if !styles.contains(class) {
                continue;
            }
            let fields: Vec<_> = text.split_whitespace().collect();
            let marker_at = fields
                .iter()
                .position(|field| *field == marker)
                .expect("a changed style row was already proved to have a marker");
            assert!(
                fields[..marker_at]
                    .iter()
                    .any(|field| field.chars().all(|ch| ch.is_ascii_digit())),
                "{page} row {row} has no line number before its {class:?} marker:\n{text}"
            );
        }
    }
}

#[test]
fn component_states_match_text_and_style_goldens_in_both_themes() {
    for state in STATES {
        for (theme_name, theme) in [
            ("dark", Theme::dark(ColorMode::TrueColor)),
            ("light", Theme::light(ColorMode::TrueColor)),
        ] {
            let capture = capture(*state, theme);
            let stem = golden_stem(*state, theme_name);
            assert_golden(&stem, &capture.text);
            assert_golden(&format!("{stem}_styles"), &capture.styles);
        }
    }
}

#[test]
fn gallery_style_maps_lock_surfaces_and_diff_rows() {
    for (theme_name, theme) in [
        ("dark", Theme::dark(ColorMode::TrueColor)),
        ("light", Theme::light(ColorMode::TrueColor)),
    ] {
        let claude = capture(NamedState::ComponentGallery, theme);
        for class in ['U', 'A', 'P', '+', '-'] {
            assert!(
                claude.styles.contains(class),
                "Claude {theme_name} gallery has no {class:?} style class"
            );
        }
        changed_rows_are_diff_rows(&claude, &format!("Claude {theme_name} gallery"));

        let codex = capture(NamedState::ComponentGalleryCodex, theme);
        changed_rows_are_diff_rows(&codex, &format!("Codex {theme_name} gallery"));
        changed_rows_have_numbered_gutters(&codex, &format!("Codex {theme_name} gallery"));
    }
}

#[test]
fn exploration_pair_expands_members_in_order_and_keeps_the_edit_visible() {
    let theme = Theme::dark(ColorMode::TrueColor);
    let collapsed = capture(NamedState::ExplorationCollapsed, theme).text;
    let expanded = capture(NamedState::ExplorationExpanded, theme).text;

    assert!(!collapsed.contains("Grep \"max_attempts\""));
    let grep = expanded
        .find("Grep \"max_attempts\"")
        .expect("first member");
    let config = expanded.find("Read sync/config.rs").expect("second member");
    let client = expanded.find("Read sync/client.rs").expect("third member");
    let retry = expanded
        .find("Grep \"RetryConfig\"")
        .expect("fourth member");
    assert!(grep < config && config < client && client < retry);

    for frame in [&collapsed, &expanded] {
        assert!(frame.contains("Edit sync/config.rs · +3 −1"));
    }
}

#[test]
fn interaction_states_name_the_paused_view_and_copy_chord() {
    let theme = Theme::dark(ColorMode::TrueColor);
    for state in [
        NamedState::ClaudeScrolledBack,
        NamedState::CodexScrolledBack,
    ] {
        let frame = capture(state, theme).text;
        assert!(
            frame.contains("scrolled back"),
            "{state} should say plainly that following is paused:\n{frame}"
        );
    }

    let help = capture(NamedState::HelpOverlay, theme).text;
    assert!(
        help.contains("C-a y") && help.contains("copy the focused block"),
        "help should name the effective copy chord and action:\n{help}"
    );
}
