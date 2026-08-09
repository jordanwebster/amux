//! Terminal ownership: alternate screen, raw mode, and guaranteed restore.
//!
//! The chrome is alt-screen only and never writes terminal scrollback.
//! Restore is RAII-backed with a process-global panic hook that restores
//! before reporting — guaranteed on orderly exits, best-effort on unwind,
//! and nothing survives SIGKILL. The byte-emitting halves are generic over
//! `Write` so the tier-3 vt100 harness can assert the exact sequences.

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// True while the chrome owns the terminal (alt screen + raw mode). The
/// panic hook restores iff set, and clears it so restore runs once.
static CHROME_OWNS_TERMINAL: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// Bytes that put the terminal into chrome mode (alternate screen, hidden
/// cursor). Raw mode is termios, not bytes, and is handled by the guard.
pub fn write_enter_chrome(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(out, EnterAlternateScreen, Hide)
}

/// Bytes that restore the terminal from chrome mode: leave the alternate
/// screen, show the cursor. Every exit path — orderly, error, panic — must
/// emit these.
pub fn write_restore(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(out, LeaveAlternateScreen, Show)
}

fn restore_now() {
    let _ = disable_raw_mode();
    let _ = write_restore(&mut io::stdout());
}

/// Install (once) a panic hook that restores the terminal before the
/// default hook reports — a panic report on the alternate screen would
/// vanish with it.
pub fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if CHROME_OWNS_TERMINAL.swap(false, Ordering::SeqCst) {
                restore_now();
            }
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
