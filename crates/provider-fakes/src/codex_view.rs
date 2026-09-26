//! `fake-codex resume <thread>`: the Codex terminal view a raw attach
//! opens on an agent's thread.
//!
//! The real one is Codex's TUI repainting the thread from its rollout. The
//! fake draws plain lines a test can look for: the thread it opened, its
//! size at start and after every resize, and each line typed into it. It
//! ends on Ctrl-C or Ctrl-D, at the end of its input, or when its terminal
//! hangs up or it is asked to terminate.

use std::io::{Read, Write};
use std::sync::mpsc;

enum Event {
    Bytes(Vec<u8>),
    #[cfg_attr(not(unix), allow(dead_code))]
    Resized,
    Ended,
}

/// The thread a `resume` command line names, if it is one. Values of
/// Codex's global options are skipped.
pub fn resumed_thread(args: &[String]) -> Option<&str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" | "--model" | "-m" | "--profile" | "-p" | "--sandbox" | "-s"
            | "--ask-for-approval" | "-a" | "--cd" | "-C" => {
                args.next();
            }
            "resume" => return args.next().map(String::as_str),
            flag if flag.starts_with('-') => {}
            _ => return None,
        }
    }
    None
}

pub fn run(thread: &str) -> i32 {
    crate::pty::raw_mode();
    let (tx, events) = mpsc::channel();
    signals(tx.clone());
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0u8; 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::Ended);
                    return;
                }
                Ok(read) => {
                    if tx.send(Event::Bytes(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let cwd = std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    draw(&format!("fake codex resume {thread} in {cwd}"));
    draw(&crate::pty::size_line());
    let mut line = Vec::new();
    while let Ok(event) = events.recv() {
        match event {
            Event::Bytes(bytes) => {
                for byte in bytes {
                    match byte {
                        0x03 | 0x04 => return 0,
                        b'\r' | b'\n' => {
                            draw(&format!("> {}", String::from_utf8_lossy(&line)));
                            line.clear();
                        }
                        byte => line.push(byte),
                    }
                }
            }
            Event::Resized => draw(&crate::pty::size_line()),
            Event::Ended => return 0,
        }
    }
    0
}

fn draw(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "{text}\r\n");
    let _ = stdout.flush();
}

fn signals(tx: mpsc::Sender<Event>) {
    #[cfg(unix)]
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime");
        runtime.block_on(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut resized), Ok(mut term), Ok(mut hup)) = (
                signal(SignalKind::window_change()),
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            loop {
                let event = tokio::select! {
                    _ = resized.recv() => Event::Resized,
                    _ = term.recv() => Event::Ended,
                    _ = hup.recv() => Event::Ended,
                };
                let ended = matches!(event, Event::Ended);
                if tx.send(event).is_err() || ended {
                    return;
                }
            }
        });
    });
    #[cfg(not(unix))]
    let _ = tx;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn a_resume_command_line_names_its_thread_past_global_options() {
        assert_eq!(
            resumed_thread(&args(&["--config", "a=b", "resume", "t1"])),
            Some("t1")
        );
        assert_eq!(resumed_thread(&args(&["resume", "t2"])), Some("t2"));
        assert_eq!(
            resumed_thread(&args(&["app-server", "--listen", "stdio://"])),
            None
        );
        assert_eq!(resumed_thread(&args(&[])), None);
    }
}
