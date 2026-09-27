//! `amux attach` as a person runs it: the real binary in a terminal of its
//! own, against a real daemon hosting agents on the fake providers.
//!
//! Terminal Claude attaches in files mode: the retained tail replays, keys
//! and a resize reach the one terminal, a second attacher sees the same
//! screen, and the leader's detach leaves the agent running. Codex attaches
//! in stream mode, and the fleet opened over it returns to the same view
//! process. Headless Claude has no terminal and is pointed at its chat.

#![cfg(unix)]

mod support;

use std::time::Duration;

use provider_fakes::Step;
use pty_host::{PtyProcess, PtySize, PtySpawn};
use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::{PATIENCE, chat, client, created_id, line_of, texts, until};

/// The leader, Ctrl+A, and its two chords.
const DETACH: &[u8] = b"\x01d";
const FLEET: &[u8] = b"\x01s";

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

/// `amux attach <agent>` running in a terminal the test holds: everything
/// it wrote, and the screen a terminal would show.
struct Term {
    process: PtyProcess,
    output: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    written: Vec<u8>,
    screen: vt100::Parser,
}

impl Term {
    fn attach(desk: &Desk, agent: &str, rows: u16, cols: u16) -> Term {
        let path = format!(
            "{}:{}",
            desk.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let process = pty_host::spawn(PtySpawn {
            command: desk.amux_path(),
            args: vec!["attach".into(), agent.into()],
            cwd: desk.work.clone(),
            env: vec![
                ("AMUX_CONFIG".into(), desk.config.clone().into()),
                ("PATH".into(), path.into()),
                ("TERM".into(), "xterm-256color".into()),
            ],
            env_remove: vec!["AMUX_LOG".into()],
            size: PtySize { rows, cols },
        })
        .expect("amux attach starts in a terminal");
        let output = process.handle.output();
        Term {
            process,
            output,
            written: Vec::new(),
            screen: vt100::Parser::new(rows, cols, 0),
        }
    }

    /// Answers the queries a terminal answers: the cursor position, which
    /// the fleet's terminal library asks when it starts, and the device
    /// attributes terminal Claude asks when it starts. Answers are typed
    /// into amux, as a terminal types them.
    async fn answer(&self, bytes: &[u8]) {
        if bytes.windows(4).any(|window| window == b"\x1b[6n") {
            let (row, col) = self.screen.screen().cursor_position();
            self.type_keys(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes())
                .await;
        }
        if bytes.windows(3).any(|window| window == b"\x1b[c") {
            self.type_keys(b"\x1b[?1;2c").await;
        }
    }

    fn said(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }

    fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    /// Reads until `done` holds of what the terminal shows.
    async fn until(&mut self, what: &str, done: impl Fn(&Term) -> bool) {
        let waited = tokio::time::timeout(PATIENCE, async {
            while !done(self) {
                match self.output.recv().await {
                    Some(bytes) => {
                        self.written.extend_from_slice(&bytes);
                        self.screen.process(&bytes);
                        self.answer(&bytes).await;
                    }
                    None => return false,
                }
            }
            true
        })
        .await;
        assert!(
            waited == Ok(true),
            "waiting for {what}; the terminal shows:\n{}\nand was written:\n{}",
            self.contents(),
            self.said()
        );
    }

    /// Reads until the terminal has been written `text`.
    async fn drawn(&mut self, text: &str) {
        self.until(text, |term| term.said().contains(text)).await;
    }

    /// Reads until the screen shows `text` now.
    async fn shows(&mut self, text: &str) {
        self.until(text, |term| term.contents().contains(text))
            .await;
    }

    async fn type_keys(&self, keys: &[u8]) {
        self.process
            .handle
            .write(keys)
            .await
            .expect("typing reaches amux");
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.screen.set_size(rows, cols);
        self.process
            .handle
            .resize(PtySize { rows, cols })
            .expect("the terminal resizes");
    }

    /// Waits for amux to exit, reading what it says on the way out.
    async fn exits(mut self) -> String {
        let status = tokio::time::timeout(PATIENCE, async {
            loop {
                tokio::select! {
                    Some(bytes) = self.output.recv() => self.written.extend_from_slice(&bytes),
                    status = self.process.exit.wait() => break status,
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("amux attach did not exit; it said:\n{}", self.said()));
        while let Ok(Some(bytes)) =
            tokio::time::timeout(Duration::from_millis(200), self.output.recv()).await
        {
            self.written.extend_from_slice(&bytes);
        }
        assert!(status.success(), "amux attach failed:\n{}", self.said());
        self.said()
    }
}

fn desk(steps: Vec<Step>) -> Desk {
    Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        steps,
    )
}

async fn create(desk: &Desk, kind: &str, name: &str) -> Vec<u8> {
    let work = desk.work.to_string_lossy().into_owned();
    let created = desk
        .run(&[
            "create", kind, "--name", name, "--cwd", &work, "--prompt", "start",
        ])
        .await;
    created_id(&created)
}

#[tokio::test]
async fn terminal_claude_attaches_twice_takes_keys_and_a_resize_and_detaches() {
    let desk = desk(vec![
        text("hello from the agent"),
        Step::TurnEnd,
        text("second turn done"),
        Step::TurnEnd,
    ]);
    desk.run(&["server", "start"]).await;
    let scout = create(&desk, "claude_pty", "scout").await;
    let mut chats = client(&desk.socket).await;
    until("the first turn", async || {
        texts(&chat(&mut chats, &scout).await).contains(&"hello from the agent")
    })
    .await;

    say("$ amux attach scout");
    let mut first = Term::attach(&desk, "scout", 30, 100);
    first.drawn("hello from the agent").await;
    first.resize(33, 111);
    first.drawn("size 33x111").await;

    say("$ amux attach scout  (a second terminal)");
    let mut second = Term::attach(&desk, "scout", 33, 111);
    second.drawn("hello from the agent").await;
    second.type_keys(b"second\r").await;
    for term in [&mut first, &mut second] {
        term.drawn("> second").await;
        term.drawn("second turn done").await;
    }

    say("<leader> d in the second terminal");
    second.type_keys(DETACH).await;
    let said = second.exits().await;
    assert!(said.contains("[detached from scout]"), "{said}");

    // The first is still attached, and the agent still running.
    first.resize(40, 120);
    first.drawn("size 40x120").await;
    let listing = desk.run(&["ls"]).await;
    assert!(!line_of(&listing, "scout").contains("exited"), "{listing}");
    first.type_keys(DETACH).await;
    let said = first.exits().await;
    assert!(said.contains("[detached from scout]"), "{said}");
}

#[tokio::test]
async fn codex_attaches_in_stream_mode_and_the_fleet_returns_to_the_same_view() {
    let desk = desk(vec![text("codex was here"), Step::TurnEnd]);
    desk.run(&["server", "start"]).await;
    let coder = create(&desk, "codex", "coder").await;
    let mut chats = client(&desk.socket).await;
    until("the codex turn", async || {
        texts(&chat(&mut chats, &coder).await).contains(&"codex was here")
    })
    .await;

    say("$ amux attach coder");
    let mut term = Term::attach(&desk, "coder", 30, 100);
    term.shows("fake codex resume").await;
    term.type_keys(b"one\r").await;
    term.shows("> one").await;
    // Half a line, held by the view process until Enter.
    term.type_keys(b"par").await;

    say("<leader> s: the fleet over the attached view");
    term.type_keys(FLEET).await;
    term.shows("o terminal").await;
    assert!(term.contents().contains("coder"), "{}", term.contents());

    say("o: back to the same view");
    term.type_keys(b"o").await;
    term.shows("> one").await;
    term.type_keys(b"tial\r").await;
    // A new view would have started with an empty line.
    term.shows("> partial").await;

    term.type_keys(DETACH).await;
    let said = term.exits().await;
    assert!(said.contains("[detached from coder]"), "{said}");
}

#[tokio::test]
async fn headless_claude_has_no_terminal_and_is_pointed_at_its_chat() {
    let desk = desk(vec![text("headless"), Step::TurnEnd]);
    desk.run(&["server", "start"]).await;
    create(&desk, "claude_sdk", "quiet").await;
    let refused = desk.amux(&["attach", "quiet"]).await;
    let said = String::from_utf8_lossy(&refused.stderr);
    say(format!("$ amux attach quiet\n  {said}"));
    assert!(!refused.status.success());
    assert!(said.contains("has no terminal"), "{said}");
    assert!(said.contains("open its chat"), "{said}");
}
