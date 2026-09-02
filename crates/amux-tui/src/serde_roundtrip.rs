//! Renderer state survives a round trip through JSON.
//!
//! View state is serializable so a diagnostic capture can record the screen
//! a person was looking at and replay it later (`docs/CHAT.md` §State
//! transitions). That only works if the serialized form is complete: every
//! field a key handler or painter reads has to come back, and the paint
//! caches — which are re-derived on the next draw — have to stay behind.
//! The proof is a re-render: a view that made the round trip must draw the
//! same frame, cell for cell and style for style, as the view it came from.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::composer::Composer;
use crate::fixtures::{NamedState, all_states, fixture};
use crate::theme::{ColorMode, Theme, ThemeName, Tokens};
use crate::view::{Mode, OpenMode, QuitGuard, ViewState};
use crate::{FrameContext, render};

const VIEWPORT: (u16, u16) = (120, 40);

fn round_trip<T>(value: &T) -> (T, String)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("view state serializes");
    let back = serde_json::from_str(&json).expect("view state deserializes");
    (back, json)
}

/// Draw one fixture's screen at the size every capture uses.
fn draw(state: NamedState, view: &ViewState) -> Buffer {
    let built = fixture(state);
    let backend = TestBackend::new(VIEWPORT.0, VIEWPORT.1);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let context = FrameContext {
        viewport: VIEWPORT,
        theme: Theme::default(),
        now: built.now,
    };
    terminal
        .draw(|frame| render(&built.model, view, &context, frame))
        .unwrap_or_else(|error| panic!("{state} failed to render: {error}"));
    terminal.backend().buffer().clone()
}

#[test]
fn every_fixture_view_round_trips_to_the_same_frame() {
    for state in all_states() {
        let built = fixture(*state);
        let before = draw(*state, &built.view);
        let (restored, json) = round_trip(&built.view);
        let after = draw(*state, &restored);
        assert_eq!(
            after, before,
            "{state} drew a different frame after a round trip through {json}"
        );
    }
}

#[test]
fn paint_caches_do_not_travel() {
    // A long feed is the case that matters: its metrics cache is what key
    // handling reads, so a cache carried across a round trip would be
    // geometry from another process claiming to describe this frame.
    let state = NamedState::ClaudeScrolledBack;
    let built = fixture(state);
    let chat = built.view.chat.as_ref().expect("Claude chat open");
    assert!(
        chat.has_cached_metrics(),
        "the fixture's own draws should have filled the metrics cache"
    );

    let (restored, json) = round_trip(&built.view);
    assert!(
        !json.contains("feed_metrics") && !json.contains("paint_cache"),
        "the caches must not appear in the serialized form: {json}"
    );
    assert!(
        !restored
            .chat
            .as_ref()
            .expect("Claude chat open")
            .has_cached_metrics(),
        "a restored chat must rebuild its metrics on its next draw"
    );

    // And rebuilding them is enough: the restored view draws the scrolled
    // frame, not the bottom of the feed.
    assert_eq!(draw(state, &restored), draw(state, &built.view));
}

#[test]
fn theme_round_trips() {
    for theme in [
        Theme::dark(ColorMode::TrueColor),
        Theme::dark(ColorMode::Ansi),
        Theme::light(ColorMode::TrueColor),
        Theme::light(ColorMode::Ansi),
    ] {
        let (back, _) = round_trip(&theme);
        assert_eq!(back, theme);
    }

    // The parts named in their own right, so a field added to one of them
    // without a serde derive fails here rather than in a capture.
    let tokens: Tokens = Theme::default().tokens;
    assert_eq!(round_trip(&tokens).0, tokens);
    assert_eq!(round_trip(&ColorMode::Ansi).0, ColorMode::Ansi);
    assert_eq!(round_trip(&ThemeName::Imported).0, ThemeName::Imported);
}

#[test]
fn composer_and_quit_guard_round_trip() {
    let mut composer = Composer::default();
    composer.insert_str("first line");
    composer.insert_newline();
    composer.insert_str("second line");
    composer.left();
    composer.kill_to_line_start();
    let back = round_trip(&composer).0;
    assert_eq!(back, composer);
    // The kill buffer travels too — Ctrl+Y after a replayed kill must
    // restore the same text the live session would have restored.
    let mut yanked = back;
    yanked.yank();
    let mut expected = composer;
    expected.yank();
    assert_eq!(yanked.text(), expected.text());

    let mut guard = QuitGuard::default();
    assert_eq!(round_trip(&guard).0, guard);
    guard.press(chrono::Utc::now());
    let armed = round_trip(&guard).0;
    assert!(armed.is_armed());
    assert_eq!(armed, guard);
}

#[test]
fn fleet_modes_round_trip() {
    let mut view = ViewState {
        default_open_mode: OpenMode::Chat,
        filter: "web".to_string(),
        selected: 3,
        scroll: 2,
        pending_g: true,
        dismissed_error_seq: 9,
        notice: Some("host offline".to_string()),
        ..ViewState::default()
    };
    for mode in [
        Mode::Normal,
        Mode::Filter,
        Mode::Help,
        Mode::Rename {
            agent: uuid::Uuid::from_u128(7),
            draft: "renamed".to_string(),
        },
        Mode::ConfirmDelete {
            agent: uuid::Uuid::from_u128(8),
            name: "doomed".to_string(),
        },
    ] {
        view.mode = mode;
        let back = round_trip(&view).0;
        assert_eq!(back.mode, view.mode);
        assert_eq!(back.filter, view.filter);
        assert_eq!(back.notice, view.notice);
        assert_eq!(back.default_open_mode, view.default_open_mode);
    }
}
