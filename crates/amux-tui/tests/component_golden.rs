//! Full-frame goldens for the component gallery and exploration pair.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p amux-tui --test
//! component_golden` and review both the text and semantic style maps.

use amux_tui::fixtures::{NamedState, fixture};
use amux_tui::report_flow::paint;
use amux_tui::{ColorMode, FrameContext, Theme, render};
use amux_ui::report::Mark;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

const VIEWPORT: (u16, u16) = (120, 40);
const WIDTH_DECLARING_GOLDENS: &[(&str, (u16, u16))] = &[
    ("a2a_fleet_family_60col", (60, 14)),
    ("fleet_ranked_60col", (60, 11)),
    ("fleet_ranked_80col", (80, 11)),
    ("chat_quit_armed_panel_narrow", (60, 22)),
    ("chat_session_facts_60col", (60, 20)),
    ("fleet_too_narrow", (12, 11)),
];
const STATES: &[NamedState] = &[
    NamedState::ComponentGallery,
    NamedState::ComponentGalleryCodex,
    NamedState::ExplorationCollapsed,
    NamedState::ExplorationExpanded,
    NamedState::ClaudeScrolledBack,
    NamedState::CodexScrolledBack,
    NamedState::HelpOverlay,
    NamedState::ProfileSwitcher,
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

fn assert_cell_dimensions(name: &str, rendered: &str, viewport: (u16, u16), exact: bool) {
    let lines = rendered.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        viewport.1 as usize,
        "golden {name} should have {} rows, got {}",
        viewport.1,
        lines.len()
    );
    for (row, line) in lines.into_iter().enumerate() {
        let serialized_cells = line.chars().count();
        if exact {
            assert_eq!(
                serialized_cells, viewport.0 as usize,
                "golden {name} row {row} should have {} cells",
                viewport.0
            );
        } else {
            // A wide glyph occupies its lead cell and leaves a serialized
            // continuation cell; combining marks can add another scalar.
            // Text rows can therefore contain more scalars than buffer cells,
            // but never fewer. Style maps are ASCII and remain exact above.
            assert!(
                serialized_cells >= viewport.0 as usize,
                "golden {name} row {row} is shorter than {} serialized cells",
                viewport.0
            );
        }
    }
}

#[test]
fn viewport_policy_keeps_only_width_declaring_goldens_nonstandard() {
    assert_eq!(
        WIDTH_DECLARING_GOLDENS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        [
            "a2a_fleet_family_60col",
            "fleet_ranked_60col",
            "fleet_ranked_80col",
            "chat_quit_armed_panel_narrow",
            "chat_session_facts_60col",
            "fleet_too_narrow",
        ]
    );
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        return;
    }

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for entry in std::fs::read_dir(directory).expect("read golden directory") {
        let entry = entry.expect("golden directory entry");
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .expect("UTF-8 golden name");
        let rendered = std::fs::read_to_string(&path).expect("read golden");
        let viewport = WIDTH_DECLARING_GOLDENS
            .iter()
            .find_map(|(exception, viewport)| (*exception == name).then_some(*viewport))
            .unwrap_or(VIEWPORT);

        if let Some(combined) = rendered.strip_prefix("--- text ---\n") {
            let (text, styles) = combined
                .split_once("--- styles ---\n")
                .unwrap_or_else(|| panic!("combined golden {name} has no styles section"));
            assert_cell_dimensions(&format!("{name} text"), text, viewport, false);
            assert_cell_dimensions(&format!("{name} styles"), styles, viewport, true);
        } else {
            assert_cell_dimensions(name, &rendered, viewport, name.contains("styles"));
        }
    }
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

/// Serialize a rendered buffer the way `capture` does, for a frame that
/// was not produced by `render`.
fn serialize(buffer: &Buffer, theme: Theme) -> Capture {
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

/// The capture overlay: a frozen screen with two marked rectangles and the
/// prompt asking about the second one. The frozen frame underneath is an
/// ordinary gallery render, so the golden shows exactly what a person sees
/// when they stop the world and start marking it up.
#[test]
fn report_overlay_marks_the_frozen_screen_and_asks_about_it() {
    let theme = Theme::dark(ColorMode::TrueColor);
    let frozen = frozen_gallery(theme);

    let marks = [
        Mark {
            x: 4,
            y: 3,
            width: 30,
            height: 4,
            note: "this block should be folded".into(),
        },
        Mark {
            x: 60,
            y: 20,
            width: 24,
            height: 2,
            note: "the age never updates".into(),
        },
    ];
    let mut overlay = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1)).expect("terminal");
    overlay
        .draw(|frame| {
            paint(
                frame,
                &frozen,
                theme,
                marks.iter(),
                None,
                "what is wrong here? the age never updates▏  enter keep · esc discard",
            )
        })
        .expect("draw the overlay");

    let capture = serialize(overlay.backend().buffer(), theme);
    assert_golden("report_overlay_dark", &capture.text);
    assert_golden("report_overlay_dark_styles", &capture.styles);
}

/// The same overlay before any box has been drawn: the prompt offers to
/// move a cursor, and the golden is where that cursor is shown to exist.
#[test]
fn report_overlay_shows_the_cursor_before_any_box_is_drawn() {
    let theme = Theme::dark(ColorMode::TrueColor);
    let frozen = frozen_gallery(theme);

    let mut overlay = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1)).expect("terminal");
    overlay
        .draw(|frame| {
            paint(
                frame,
                &frozen,
                theme,
                [].iter(),
                Some((18, 8)),
                "marked 0:  move hjkl/arrows · drag or space to start a box · enter finish",
            )
        })
        .expect("draw the overlay");

    let capture = serialize(overlay.backend().buffer(), theme);
    assert_golden("report_overlay_cursor_dark", &capture.text);
    assert_golden("report_overlay_cursor_dark_styles", &capture.styles);
}

/// The gallery, rendered and frozen — the screen both overlay goldens sit
/// on top of.
fn frozen_gallery(theme: Theme) -> Buffer {
    let fixture = fixture(NamedState::ComponentGallery);
    let mut terminal = Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1)).expect("terminal");
    let context = FrameContext {
        viewport: VIEWPORT,
        theme,
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .expect("draw the screen that will be frozen");
    terminal.backend().buffer().clone()
}

/// The screenshot states for the review page reach it the way a person
/// does — the chord, the daemon's frozen diff, and the chat's own key
/// handler — while the page's goldens drive `ReviewView` directly. Both
/// must land on the same screen, or a committed PNG would show a page the
/// running program never produces.
///
/// Only the page states are compared. The page covers the whole frame, so
/// the conversation underneath it cannot show through; the token state
/// hands the screen back to the chat, and these fixtures and the chat
/// goldens sit on different conversations.
#[test]
fn review_screenshot_states_render_the_pages_their_goldens_describe() {
    for (state, golden) in [
        (NamedState::ReviewOpen, "review_open"),
        (NamedState::ReviewSelection, "review_selection"),
        (NamedState::ReviewCommentBox, "review_comment_box"),
        (NamedState::ReviewThreads, "review_threads"),
        (NamedState::ReviewFileList, "review_file_list"),
        (NamedState::ReviewFolded, "review_folded"),
        (NamedState::ReviewBranchBase, "review_branch_base"),
    ] {
        let rendered = capture(state, Theme::default()).text;
        let expected = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden")
                .join(format!("{golden}.txt")),
        )
        .unwrap_or_else(|_| panic!("golden {golden} exists"));
        assert_eq!(
            rendered,
            expected,
            "{} diverged from {golden}",
            state.name()
        );
    }
}

/// `q` hands the screen back to the chat with the review folded into one
/// draft token, counting what is behind it, beside the words typed after
/// it — the state the screenshot set captures a sent review from.
#[test]
fn the_review_token_state_shows_the_draft_the_review_rides_in() {
    let rendered = capture(NamedState::ChatReviewToken, Theme::default()).text;
    assert!(
        rendered.contains("[Review \u{b7} 2 comments] \u{2014} two things before this lands"),
        "the draft carries the token and the words after it: {rendered}"
    );
}
