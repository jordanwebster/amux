use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

// Note: We disable PTY echo (stty -echo) when spawning commands.
// PTY echo causes input to be echoed back before the command's response.
// Rather than eagerly draining echo bytes after each input, we simply
// disable echo to get cleaner output for test assertions.

/// Quote a string for safe use in a shell command
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

/// A test terminal that wraps a PTY
pub struct TestTerminal {
    _master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// Buffer for accumulating output
    output_buffer: Vec<u8>,
}

impl TestTerminal {
    /// Create a new terminal and spawn a command
    pub fn spawn(
        command: &str,
        args: &[&str],
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

        // Build the shell command that disables echo before exec'ing the real command
        // This prevents PTY echo from cluttering our output
        // We quote each argument to handle paths with spaces or special characters
        let quoted_args: Vec<String> = args.iter().map(|arg| shell_quote(arg)).collect();
        // stty raw: disable all input/output processing (no echo, no NL translation, etc.)
        // Note: This only affects the outer PTY; nested PTYs (like those created by amux)
        // will still have default terminal settings, so we normalize \r\n -> \n when reading.
        let shell_cmd = format!(
            "stty raw; exec {} {}",
            shell_quote(command),
            quoted_args.join(" ")
        );

        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", &shell_cmd]);
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

        let reader = pair.master.try_clone_reader().map_err(|e| TerminalError {
            message: format!("Failed to get PTY reader: {}", e),
        })?;

        let writer = pair.master.take_writer().map_err(|e| TerminalError {
            message: format!("Failed to get PTY writer: {}", e),
        })?;

        Ok(Self {
            _master: pair.master,
            reader,
            writer,
            output_buffer: Vec::new(),
        })
    }

    /// Send input to the terminal (with newline)
    pub fn send_line(&mut self, input: &str) -> Result<(), TerminalError> {
        writeln!(self.writer, "{}", input)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Read output and normalize to match expected string.
    /// Handles \r\n vs \n differences from terminal output processing.
    /// Waits up to `timeout` for enough data to arrive.
    pub fn read_expected(
        &mut self,
        expected: &str,
        timeout: Duration,
    ) -> Result<String, TerminalError> {
        let start = std::time::Instant::now();
        let mut buffer = [0u8; 1024];

        loop {
            // Normalize what we have so far and check if it matches
            let normalized = self.normalize_buffer();

            if normalized.len() >= expected.len() {
                // We have enough normalized bytes - extract and compare
                let result = normalized[..expected.len()].to_string();
                // Calculate how many raw bytes we consumed
                // This is tricky because we normalized \r\n to \n
                let consumed = self.calculate_consumed_bytes(expected.len());
                self.output_buffer.drain(..consumed);
                return Ok(result);
            }

            if start.elapsed() > timeout {
                return Err(TerminalError {
                    message: format!(
                        "Timeout waiting for {} bytes (got {} normalized bytes: {:?})",
                        expected.len(),
                        normalized.len(),
                        normalized
                    ),
                });
            }

            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    return Err(TerminalError {
                        message: format!(
                            "EOF before receiving {} bytes (got {} normalized bytes: {:?})",
                            expected.len(),
                            normalized.len(),
                            normalized
                        ),
                    });
                }
                Ok(n) => {
                    self.output_buffer.extend_from_slice(&buffer[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    return Err(TerminalError {
                        message: format!("Read error: {}", e),
                    });
                }
            }
        }
    }

    /// Normalize buffer: convert \r\n to \n, and standalone \r to \n
    fn normalize_buffer(&self) -> String {
        let s = String::from_utf8_lossy(&self.output_buffer);
        s.replace("\r\n", "\n").replace('\r', "\n")
    }

    /// Calculate how many raw bytes to consume to get `normalized_len` normalized bytes
    fn calculate_consumed_bytes(&self, normalized_len: usize) -> usize {
        let mut raw_idx = 0;
        let mut norm_count = 0;
        let bytes = &self.output_buffer;

        while norm_count < normalized_len && raw_idx < bytes.len() {
            if raw_idx + 1 < bytes.len() && bytes[raw_idx] == b'\r' && bytes[raw_idx + 1] == b'\n' {
                // \r\n counts as one normalized char
                raw_idx += 2;
                norm_count += 1;
            } else {
                raw_idx += 1;
                norm_count += 1;
            }
        }

        raw_idx
    }
}
