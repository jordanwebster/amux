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

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
/// cursor, bracketed paste — without it a pasted CR would arrive as Enter
/// and submit a partial prompt). Raw mode is termios, not bytes, and is
/// handled by the guard.
pub fn write_enter_chrome(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(out, EnterAlternateScreen, Hide, EnableBracketedPaste)
}

/// Bytes that restore the terminal from chrome mode: leave the alternate
/// screen, show the cursor, disable bracketed paste. Every exit path —
/// orderly, error, panic, signal — must emit these (the terminal-hygiene
/// set, `docs/UI.md`).
pub fn write_restore(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(out, LeaveAlternateScreen, Show, DisableBracketedPaste)
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
    /// leave alternate screen + show cursor + disable bracketed paste,
    /// mirroring [`super::restore_now`] (locked to crossterm's actual
    /// bytes by a unit test below). Deliberately unconditional, like the
    /// rest of this handler.
    pub(super) const RESTORE_BYTES: &[u8] = b"\x1b[<1u\x1b[?1049l\x1b[?25h\x1b[?2004l";

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
    fn signal_restore_bytes_match_write_restore() {
        let mut out: Vec<u8> = Vec::new();
        crossterm::execute!(out, PopKeyboardEnhancementFlags).expect("write to vec");
        write_restore(&mut out).expect("write to vec");
        assert_eq!(out, signal_restore::RESTORE_BYTES);
    }
}
