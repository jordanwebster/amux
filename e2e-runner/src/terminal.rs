use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

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

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);
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

    /// Send raw bytes to the terminal
    pub fn send_raw(&mut self, data: &[u8]) -> Result<(), TerminalError> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Read output until we see a NUL byte (sync signal from test-agent)
    /// Returns the output without the NUL byte
    pub fn read_until_nul(&mut self, timeout: Duration) -> Result<String, TerminalError> {
        let start = std::time::Instant::now();
        let mut buffer = [0u8; 1024];

        loop {
            if start.elapsed() > timeout {
                return Err(TerminalError {
                    message: format!(
                        "Timeout waiting for NUL byte. Buffer so far: {:?}",
                        String::from_utf8_lossy(&self.output_buffer)
                    ),
                });
            }

            // Non-blocking read attempt with small timeout
            // Note: portable-pty doesn't support non-blocking reads directly,
            // so we use a small sleep and check
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    // EOF - process ended
                    let output = String::from_utf8_lossy(&self.output_buffer).to_string();
                    self.output_buffer.clear();
                    return Ok(output);
                }
                Ok(n) => {
                    // Check for NUL byte
                    for i in 0..n {
                        if buffer[i] == 0x00 {
                            // Found NUL - add everything before it to buffer
                            self.output_buffer.extend_from_slice(&buffer[..i]);
                            let output = String::from_utf8_lossy(&self.output_buffer).to_string();
                            self.output_buffer.clear();

                            // Any data after NUL goes into buffer for next read
                            if i + 1 < n {
                                self.output_buffer.extend_from_slice(&buffer[i + 1..n]);
                            }

                            return Ok(output);
                        }
                    }
                    // No NUL found - add to buffer and continue
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

    /// Read all available output (with timeout), not waiting for NUL
    /// Used for commands that exit (like list-agents)
    pub fn read_until_exit(&mut self, timeout: Duration) -> Result<String, TerminalError> {
        let start = std::time::Instant::now();
        let mut buffer = [0u8; 1024];

        loop {
            if start.elapsed() > timeout {
                // Return what we have so far
                let output = String::from_utf8_lossy(&self.output_buffer).to_string();
                self.output_buffer.clear();
                return Ok(output);
            }

            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    // EOF - process ended
                    let output = String::from_utf8_lossy(&self.output_buffer).to_string();
                    self.output_buffer.clear();
                    return Ok(output);
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

    /// Drain and return any buffered output
    pub fn drain_buffer(&mut self) -> String {
        let output = String::from_utf8_lossy(&self.output_buffer).to_string();
        self.output_buffer.clear();
        output
    }
}
