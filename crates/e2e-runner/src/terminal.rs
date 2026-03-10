use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

// Note: We disable PTY echo (stty -echo) when spawning commands.
// PTY echo causes input to be echoed back before the command's response.
// Rather than eagerly draining echo bytes after each input, we simply
// disable echo to get cleaner output for test assertions.

/// Quote a string for safe use in a shell command (Unix only: used by stty/exec spawn path)
#[cfg(unix)]
fn shell_quote(s: &str) -> String {
    // If the string contains no special characters, return as-is
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
    {
        return s.to_string();
    }
    // Otherwise, wrap in single quotes and escape any existing single quotes
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Skip one ANSI escape sequence starting at `pos` (which points to ESC).
/// Returns the index of the first byte after the sequence.
fn skip_one_escape(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos + 1; // skip ESC
    if i >= bytes.len() {
        return i;
    }
    match bytes[i] {
        // CSI sequence: ESC [ ... (terminated by 0x40-0x7E)
        b'[' => {
            i += 1;
            while i < bytes.len() {
                if bytes[i] >= 0x40 && bytes[i] <= 0x7E {
                    return i + 1;
                }
                i += 1;
            }
            i
        }
        // OSC sequence: ESC ] ... (terminated by BEL or ST)
        b']' => {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == 0x07 {
                    return i + 1; // BEL terminator
                }
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2; // ST terminator (ESC \)
                }
                i += 1;
            }
            i
        }
        // Two-byte sequences: ESC followed by one character (e.g., ESC =, ESC >)
        _ => i + 1,
    }
}

/// Render raw terminal bytes into visible text while preserving a mapping back
/// to raw-buffer indices.
///
/// This models the terminal semantics we care about:
/// - ANSI escape sequences are ignored
/// - `\r` moves the cursor to column 0
/// - `\n` commits the current line
/// - later bytes can overwrite earlier bytes on the same line
fn render_terminal(bytes: &[u8]) -> (String, Vec<usize>) {
    let mut rendered = Vec::with_capacity(bytes.len());
    let mut rendered_map = Vec::with_capacity(bytes.len());
    let mut line = Vec::with_capacity(128);
    let mut line_map = Vec::with_capacity(128);
    let mut cursor = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            i = skip_one_escape(bytes, i);
            continue;
        }

        match bytes[i] {
            b'\r' => {
                cursor = 0;
                i += 1;
            }
            b'\n' => {
                rendered.extend_from_slice(&line);
                rendered_map.extend(line_map.iter().copied());
                rendered.push(b'\n');
                rendered_map.push(i + 1);
                line.clear();
                line_map.clear();
                cursor = 0;
                i += 1;
            }
            0x08 => {
                cursor = cursor.saturating_sub(1);
                i += 1;
            }
            byte => {
                if cursor < line.len() {
                    line[cursor] = byte;
                    line_map[cursor] = i + 1;
                } else {
                    while line.len() < cursor {
                        line.push(b' ');
                        line_map.push(i);
                    }
                    line.push(byte);
                    line_map.push(i + 1);
                }
                cursor += 1;
                i += 1;
            }
        }
    }

    rendered.extend_from_slice(&line);
    rendered_map.extend(line_map.iter().copied());

    (
        String::from_utf8_lossy(&rendered).into_owned(),
        rendered_map,
    )
}

/// Error type for terminal operations
#[derive(Debug)]
pub struct TerminalError {
    pub message: String,
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TerminalError {}

impl From<std::io::Error> for TerminalError {
    fn from(e: std::io::Error) -> Self {
        TerminalError {
            message: e.to_string(),
        }
    }
}

/// A test terminal that wraps a PTY.
/// The PTY reader runs in a background thread to avoid blocking on read().
pub struct TestTerminal {
    _master: Box<dyn MasterPty + Send>,
    rx: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    /// Buffer for accumulating output
    output_buffer: Vec<u8>,
}

impl TestTerminal {
    /// Create a new terminal and spawn a command
    pub fn spawn(
        command: &str,
        args: &[String],
        working_dir: &Path,
        env: &HashMap<String, String>,
    ) -> Result<Self, TerminalError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TerminalError {
                message: format!("Failed to create PTY: {}", e),
            })?;

        #[cfg(unix)]
        let mut cmd = {
            // Build the shell command that disables echo before exec'ing the real command.
            // This keeps the existing Unix test behavior stable while Windows uses a direct
            // ConPTY spawn path below.
            let quoted_args: Vec<String> = args.iter().map(|arg| shell_quote(arg)).collect();
            let shell_cmd = format!(
                "stty raw; exec {} {}",
                shell_quote(command),
                quoted_args.join(" ")
            );
            let mut cmd = CommandBuilder::new("sh");
            cmd.args(["-c", &shell_cmd]);
            cmd
        };

        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = CommandBuilder::new(command);
            cmd.args(args);
            cmd
        };

        cmd.cwd(working_dir);
        // Inherit all current environment vars first
        for (key, value) in std::env::vars() {
            cmd.env(key, value);
        }
        // Then override with test-specific vars
        for (key, value) in env {
            cmd.env(key, value);
        }

        let _child = pair.slave.spawn_command(cmd).map_err(|e| TerminalError {
            message: format!("Failed to spawn command: {}", e),
        })?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| TerminalError {
            message: format!("Failed to get PTY reader: {}", e),
        })?;

        let writer = pair.master.take_writer().map_err(|e| TerminalError {
            message: format!("Failed to get PTY writer: {}", e),
        })?;

        // Move the blocking reader into a background thread so read_expected
        // can use recv_timeout instead of blocking forever on read().
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            _master: pair.master,
            rx,
            writer,
            output_buffer: Vec::new(),
        })
    }

    /// Send input to the terminal (with newline)
    pub fn send_line(&mut self, input: &str) -> Result<(), TerminalError> {
        self.writer.write_all(input.as_bytes())?;
        #[cfg(windows)]
        self.writer.write_all(b"\r")?;
        #[cfg(not(windows))]
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Read output, render terminal semantics, and search for `expected`.
    /// ConPTY inserts empty-row padding (`\r\n` per row) between real output
    /// lines, so we search for `expected` as a substring rather than consuming
    /// from the front.
    pub fn read_expected(
        &mut self,
        expected: &str,
        timeout: Duration,
    ) -> Result<String, TerminalError> {
        let start = std::time::Instant::now();

        loop {
            let (rendered, rendered_map) = render_terminal(&self.output_buffer);

            #[cfg(unix)]
            if rendered.len() >= expected.len() {
                let actual = rendered[..expected.len()].to_string();
                let consumed = rendered_map
                    .get(expected.len().saturating_sub(1))
                    .copied()
                    .unwrap_or(0);
                self.output_buffer.drain(..consumed);
                return Ok(actual);
            }

            #[cfg(windows)]
            if let Some(pos) = rendered.find(expected) {
                let end = pos + expected.len();
                let consumed = rendered_map
                    .get(end.saturating_sub(1))
                    .copied()
                    .unwrap_or(0);
                self.output_buffer.drain(..consumed);
                return Ok(expected.to_string());
            }

            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(TerminalError {
                    message: format!(
                        "Timeout waiting for {:?} (got rendered: {:?})",
                        expected, rendered
                    ),
                });
            }

            match self.rx.recv_timeout(remaining) {
                Ok(data) => {
                    self.output_buffer.extend_from_slice(&data);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(TerminalError {
                        message: format!(
                            "EOF waiting for {:?} (got rendered: {:?})",
                            expected, rendered
                        ),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_terminal;

    #[test]
    fn render_terminal_collapses_crlf() {
        let (rendered, _) = render_terminal(b"hello\r\nworld\r\n");
        assert_eq!(rendered, "hello\nworld\n");
    }

    #[test]
    fn render_terminal_treats_carriage_return_as_overwrite() {
        let (rendered, _) = render_terminal(b"hello\rj");
        assert_eq!(rendered, "jello");
    }

    #[test]
    fn render_terminal_ignores_ansi_sequences() {
        let (rendered, _) = render_terminal(b"\x1b[2J\x1b[Hhello\r\n");
        assert_eq!(rendered, "hello\n");
    }
}
