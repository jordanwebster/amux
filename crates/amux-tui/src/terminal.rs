//! Terminal ownership: alternate screen, raw mode, and guaranteed restore.
//!
//! The chrome is alt-screen only and never writes terminal scrollback.
//! Restore is RAII-backed with a process-global panic hook that restores
//! before reporting — guaranteed on orderly exits, best-effort on unwind,
//! and nothing survives SIGKILL. The byte-emitting halves are generic over
//! `Write` so the tier-3 vt100 harness can assert the exact sequences.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};

use base64::Engine as _;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// True while the chrome owns the terminal (alt screen + raw mode). The
/// panic hook restores iff set, and clears it so restore runs once.
static CHROME_OWNS_TERMINAL: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// Whether the terminal answered the kitty keyboard-enhancement probe —
/// probed once per process (the terminal does not change under us), on
/// the first chrome entry, inside the guard lifecycle. Tier gate for the
/// kitty-only chords (Ctrl+Enter, Shift+Enter): hints and the `?`
/// overlay derive from it; dispatch itself trusts the events that
/// actually arrive.
static KITTY_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Whether enhancement flags are currently pushed (on the alternate
/// screen's stack). Restore pops iff set — kitty keeps per-screen flag
/// stacks, so the pop must happen before leaving the alternate screen.
static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

/// The probe result, once a chrome session has run; false before.
pub(crate) fn kitty_active() -> bool {
    KITTY_SUPPORTED.get().copied().unwrap_or(false)
}

/// Bytes that put the terminal into chrome mode (alternate screen, hidden
/// cursor, bracketed paste, and mouse capture. Bracketed paste prevents a
/// pasted CR from submitting a partial prompt; mouse capture lets the
/// alternate-screen feed own wheel events. Raw mode is termios, not bytes,
/// and is handled by the guard.
pub fn write_enter_chrome(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste,
        EnableMouseCapture
    )
}

/// Bytes that restore the terminal from chrome mode. Input modes are
/// disabled before the alternate screen is left, then the cursor is shown.
/// Every exit path — orderly, error, panic, signal — must emit these (the
/// terminal-hygiene set, `docs/UI.md`).
pub fn write_restore(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    )
}

/// Maximum source payload accepted by [`write_osc52`]. Keeping the bound
/// before base64 encoding avoids an unbounded terminal control sequence while
/// preserving the copied text exactly below the limit.
const OSC52_MAX_BYTES: usize = 100 * 1024;

/// Emit an OSC 52 clipboard sequence for `text`.
///
/// Payloads above 100 KiB are truncated at a UTF-8 character boundary. The
/// returned notice lets the event loop tell the user when that happened.
pub fn write_osc52(out: &mut impl Write, text: &str) -> io::Result<Option<String>> {
    let mut end = text.len().min(OSC52_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end < text.len();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&text.as_bytes()[..end]);

    out.write_all(b"\x1b]52;c;")?;
    out.write_all(encoded.as_bytes())?;
    out.write_all(b"\x07")?;
    out.flush()?;

    Ok(truncated.then(|| "copied first 100 KiB to clipboard (message truncated)".to_string()))
}

fn restore_now() {
    // Pop the keyboard-enhancement flags first, while still on the
    // alternate screen (kitty keeps per-screen flag stacks). Guarded by
    // the pushed flag so legacy-Windows consoles never see the CSI.
    if KITTY_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = write_restore(&mut io::stdout());
}

/// Install (once) a panic hook that restores the terminal before the
/// default hook reports — a panic report on the alternate screen would
/// vanish with it.
pub fn install_panic_hook() {
    #[cfg(unix)]
    signal_restore::install();
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if CHROME_OWNS_TERMINAL.swap(false, Ordering::SeqCst) {
                restore_now();
            }
            // Best-effort Msg recording, deliberately AFTER restore: a dump
            // is worthless if writing it delays putting the terminal back
            // and the report lands on a vanishing alternate screen.
            amux_ui::write_panic_dump(&info.to_string());
            previous(info);
        }));
    });
}

/// RAII terminal ownership for one chrome session.
pub struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        if let Err(error) = write_enter_chrome(&mut io::stdout()) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        CHROME_OWNS_TERMINAL.store(true, Ordering::SeqCst);
        // Kitty keyboard protocol, feature-detected (CHAT.md
        // §Keybindings' kitty tier): probe once per process — the query
        // needs raw mode and rides crossterm's internal event reader —
        // then push the disambiguate flag each session so Ctrl+Enter and
        // Shift+Enter arrive distinguishable. Pushed on the alternate
        // screen, popped by every restore path before leaving it.
        let supported = *KITTY_SUPPORTED
            .get_or_init(|| crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false));
        if supported
            && crossterm::execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok()
        {
            KITTY_PUSHED.store(true, Ordering::SeqCst);
        }
        Ok(Self { restored: false })
    }

    /// Orderly restore (also runs on drop; calling it explicitly makes the
    /// handoff points read as what they are).
    pub fn restore(mut self) {
        self.restore_once();
    }

    fn restore_once(&mut self) {
        if !self.restored {
            self.restored = true;
            CHROME_OWNS_TERMINAL.store(false, Ordering::SeqCst);
            restore_now();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore_once();
    }
}

/// Async-signal-safe terminal restore on SIGINT/SIGTERM/SIGHUP.
///
/// A tokio signal arm in the chrome loop would leave the process deaf to
/// signals while suspended for an attach (tokio's handler stays installed
/// process-wide, but nothing polls the stream mid-attach). Instead: a
/// low-level handler that restores the saved cooked termios (`tcsetattr`
/// is async-signal-safe), writes the leave-alt-screen/show-cursor bytes
/// (raw `write` is async-signal-safe), then re-raises the default
/// disposition so the process still dies exactly as expected. The restore
/// is deliberately unconditional: on a terminal that is already sane it is
/// a no-op, and that is what lets it cover both chrome mode and the
/// mid-attach passthrough phase.
#[cfg(unix)]
mod signal_restore {
    use std::sync::{Once, OnceLock};

    static INSTALL: Once = Once::new();
    static SAVED_TERMIOS: OnceLock<libc::termios> = OnceLock::new();

    /// Pop keyboard-enhancement flags (before leaving the alternate
    /// screen — kitty keeps per-screen stacks; a pop with nothing pushed
    /// is a no-op, and unknown-CSI-tolerant terminals ignore it), then
    /// disable mouse capture and bracketed paste, show the cursor, then
    /// leave the alternate screen,
    /// mirroring [`super::restore_now`] (locked to crossterm's actual
    /// bytes by a unit test below). Deliberately unconditional, like the
    /// rest of this handler.
    pub(super) const RESTORE_BYTES: &[u8] = b"\x1b[<1u\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[?25h\x1b[?1049l";

    pub(super) fn install() {
        INSTALL.call_once(|| {
            // Save the cooked termios once, before raw mode first engages.
            let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } == 0 {
                let _ = SAVED_TERMIOS.set(term);
            }
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                // Safety: the handler body calls only async-signal-safe
                // functions (tcsetattr, write) and signal_hook's re-raise.
                unsafe {
                    let _ = signal_hook::low_level::register(sig, move || {
                        if let Some(term) = SAVED_TERMIOS.get() {
                            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, term);
                        }
                        libc::write(
                            libc::STDOUT_FILENO,
                            RESTORE_BYTES.as_ptr().cast(),
                            RESTORE_BYTES.len(),
                        );
                        let _ = signal_hook::low_level::emulate_default_handler(sig);
                    });
                }
            }
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The signal handler's hardcoded bytes must stay in lockstep with
    /// what the orderly restore path (crossterm) actually emits: the
    /// keyboard-enhancement pop, then `write_restore`.
    #[test]
    fn signal_restore_bytes_match_mouse_aware_write_restore() {
        let mut out: Vec<u8> = Vec::new();
        crossterm::execute!(out, PopKeyboardEnhancementFlags).expect("write to vec");
        write_restore(&mut out).expect("write to vec");
        assert_eq!(out, signal_restore::RESTORE_BYTES);
    }

    #[test]
    fn enter_chrome_enables_mouse_capture() {
        let mut out = Vec::new();
        write_enter_chrome(&mut out).expect("write to vec");
        assert_eq!(
            out,
            b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h"
        );
    }

    #[test]
    fn osc52_writes_the_exact_clipboard_sequence() {
        let mut out = Vec::new();
        assert_eq!(write_osc52(&mut out, "hello").expect("write to vec"), None);
        eprintln!("OSC 52 bytes: {out:?}");
        assert_eq!(out, b"\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_caps_source_text_at_100_kib_without_splitting_utf8() {
        let text = format!("{}€", "a".repeat(OSC52_MAX_BYTES - 1));
        let mut out = Vec::new();
        let notice = write_osc52(&mut out, &text)
            .expect("write to vec")
            .expect("oversized text reports truncation");

        assert!(notice.contains("100 KiB"));
        assert!(notice.contains("truncated"));
        assert!(out.starts_with(b"\x1b]52;c;"));
        assert_eq!(out.last(), Some(&b'\x07'));
        let encoded = &out[b"\x1b]52;c;".len()..out.len() - 1];
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(decoded.len(), OSC52_MAX_BYTES - 1);
        assert!(decoded.iter().all(|byte| *byte == b'a'));
    }
}
