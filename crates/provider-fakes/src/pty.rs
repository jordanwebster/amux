//! `fake-claude-pty`: Claude Code in a terminal, without the TUI.
//!
//! What the host reads from a terminal Claude is not the screen but the
//! session transcript Claude appends to, the hook payloads it hands the
//! commands its `--settings` name, and its messaging socket. This fake keeps
//! all three and draws only plain text on the terminal.

use std::io::{Read, Write};
use std::path::Path;

use serde_json::Value;

use crate::claude::Args;
use crate::playback::{self, Channel, Process};
use crate::{DRIFT_EXIT, Mode};

pub fn main() -> i32 {
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-claude-pty: {error}");
            return DRIFT_EXIT;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    match mode {
        Mode::Playback(process) => match play(&process, &Args::parse(&args)) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("fake-claude-pty: {error}");
                DRIFT_EXIT
            }
        },
        Mode::Script(script) => runtime.block_on(engine::run(script, Args::parse(&args))),
    }
}

/// Run each hook command for the payload's event with the payload on
/// stdin, one after another, waiting for each so the host sees payloads in
/// the order they were raised.
pub fn run_hooks(args: &Args, payload: &[u8], env: &[(String, String)]) -> std::io::Result<()> {
    let event = serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|payload| payload.get("hook_event_name")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    for command in args.hook_commands(&event) {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(payload)?;
        child.wait()?;
    }
    Ok(())
}

/// Append one row to a session transcript, creating it and its directory.
pub fn append_row(path: &Path, row: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = row.to_vec();
    line.push(b'\n');
    file.write_all(&line)
}

/// Put the terminal into raw mode, so keys arrive byte for byte and output
/// leaves unchanged. Not a terminal (a test driving stdio) is left alone.
pub fn raw_mode() {
    #[cfg(unix)]
    // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed, then filled,
    // termios owned by this frame.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) == 0 {
            libc::cfmakeraw(&mut termios);
            libc::tcsetattr(0, libc::TCSANOW, &termios);
        }
    }
}

fn play(process: &Process, args: &Args) -> Result<(), String> {
    raw_mode();
    let config = playback::claude_config_dir();
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut session = String::from("unnamed");
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Output => {
                stdout
                    .write_all(&event.bytes)
                    .and_then(|()| stdout.flush())
                    .map_err(|error| format!("event {index}: writing: {error}"))?;
            }
            Channel::Input => {
                let mut typed = vec![0; event.bytes.len()];
                stdin
                    .read_exact(&mut typed)
                    .map_err(|error| format!("event {index}: reading: {error}"))?;
                if typed != event.bytes {
                    return Err(format!(
                        "event {index}: host typed {:?}, recording has {:?}",
                        String::from_utf8_lossy(&typed),
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            Channel::Transcript => {
                if let Some(named) = playback::row_session(&event.bytes) {
                    session = named;
                }
                append_row(
                    &playback::transcript_path(&config, &cwd, &session),
                    &event.bytes,
                )
                .map_err(|error| format!("event {index}: transcript: {error}"))?;
            }
            Channel::Hook => {
                if let Some(named) = playback::row_session(&event.bytes) {
                    session = named;
                }
                run_hooks(args, &event.bytes, &[])
                    .map_err(|error| format!("event {index}: hook: {error}"))?;
            }
        }
    }
    Ok(())
}

mod engine {
    use super::Args;
    use crate::Script;

    pub async fn run(_script: Script, _args: Args) -> i32 {
        0
    }
}
