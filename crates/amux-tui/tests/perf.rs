use std::time::{Duration, Instant};

use amux_tui::chat::{handle_chat_key, handle_chat_mouse};
use amux_tui::fixtures::{Fixture, long_feed};
use amux_tui::{FrameContext, PaintStats, Theme, render};
use amux_ui::StructuredProtocol;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

const VIEWPORT: (u16, u16) = (120, 40);
const ENTRIES: usize = 1_000;
const SAMPLES: usize = 50;
const BUDGET: Duration = Duration::from_millis(8);

struct Harness {
    fixture: Fixture,
    terminal: Terminal<TestBackend>,
}

impl Harness {
    fn new(protocol: StructuredProtocol) -> Self {
        Self {
            fixture: long_feed(protocol, ENTRIES),
            terminal: Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1))
                .expect("test terminal"),
        }
    }

    fn draw(&mut self) -> (Buffer, Duration) {
        let context = FrameContext {
            viewport: VIEWPORT,
            theme: Theme::default(),
            now: self.fixture.now,
        };
        let started = Instant::now();
        self.terminal
            .draw(|frame| render(&self.fixture.model, &self.fixture.view, &context, frame))
            .expect("draw long-feed frame");
        let elapsed = started.elapsed();
        (self.terminal.backend().buffer().clone(), elapsed)
    }

    fn stats(&self) -> PaintStats {
        self.fixture
            .view
            .chat
            .as_ref()
            .expect("long-feed fixture opens a chat")
            .paint_stats()
    }

    fn feed_total_rows(&self) -> usize {
        self.fixture
            .view
            .chat
            .as_ref()
            .expect("long-feed fixture opens a chat")
            .feed_total_rows()
            .expect("a rendered chat retains feed metrics")
    }

    fn type_char(&mut self, character: char) {
        let chat = self
            .fixture
            .view
            .chat
            .as_mut()
            .expect("long-feed fixture opens a chat");
        let _ = handle_chat_key(
            chat,
            &self.fixture.model,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            VIEWPORT,
            self.fixture.now,
        );
    }

    fn wheel_up(&mut self) {
        let chat = self
            .fixture
            .view
            .chat
            .as_mut()
            .expect("long-feed fixture opens a chat");
        assert!(handle_chat_mouse(
            chat,
            &self.fixture.model,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 4,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            VIEWPORT,
        ));
    }
}

fn protocols() -> [StructuredProtocol; 2] {
    [StructuredProtocol::Claude, StructuredProtocol::Codex]
}

#[test]
fn steady_state_frame_at_1000_entries_is_within_budget() {
    for protocol in protocols() {
        let mut harness = Harness::new(protocol);
        let _ = harness.draw();
        let mut samples = (0..SAMPLES)
            .map(|_| {
                let (_, elapsed) = harness.draw();
                assert_eq!(harness.stats().painted, 0, "{protocol:?} cache miss");
                elapsed
            })
            .collect::<Vec<_>>();
        samples.sort_unstable();
        let median = samples[SAMPLES / 2];
        println!(
            "{protocol:?} median steady-state frame at {ENTRIES} entries: {:.3} ms",
            median.as_secs_f64() * 1_000.0
        );
        if !cfg!(debug_assertions) {
            assert!(
                median < BUDGET,
                "{protocol:?} median {median:?} exceeded {BUDGET:?}"
            );
        }
    }
}

#[test]
fn composer_keystroke_repaints_no_feed_block() {
    for protocol in protocols() {
        let mut harness = Harness::new(protocol);
        let _ = harness.draw();
        harness.type_char('x');
        let _ = harness.draw();
        assert_eq!(
            harness.stats().painted,
            0,
            "{protocol:?} repainted feed content after typing"
        );
    }
}

#[test]
fn wheel_event_repaints_no_feed_block() {
    for protocol in protocols() {
        let mut harness = Harness::new(protocol);
        let _ = harness.draw();
        harness.wheel_up();
        let _ = harness.draw();
        assert_eq!(
            harness.stats().painted,
            0,
            "{protocol:?} repainted feed content after wheel input"
        );
    }
}

#[test]
fn row_count_reuses_the_paint_pass() {
    for protocol in protocols() {
        let mut harness = Harness::new(protocol);
        let _ = harness.draw();
        let full_paint_rows = harness.feed_total_rows();
        assert!(harness.stats().painted > 0, "{protocol:?} cold paint");

        let _ = harness.draw();
        assert_eq!(harness.feed_total_rows(), full_paint_rows);
        assert_eq!(
            harness.stats().painted,
            0,
            "{protocol:?} row counting painted feed content"
        );
    }
}

#[test]
fn cache_is_invisible_in_the_frame() {
    for protocol in protocols() {
        let mut harness = Harness::new(protocol);
        let (cold, _) = harness.draw();
        let (warm, _) = harness.draw();
        assert_eq!(cold, warm, "{protocol:?} cold and warm frame bytes differ");
        assert_eq!(harness.stats().painted, 0, "{protocol:?} warm cache miss");
    }
}
