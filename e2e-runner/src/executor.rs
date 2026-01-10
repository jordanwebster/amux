use crate::parser::{TestCase, TestStep};
use crate::terminal::TestTerminal;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

/// Result of running a test
#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
    /// Actual output collected during test (for update mode)
    pub actual_outputs: Vec<String>,
}

/// Configuration for the executor
pub struct ExecutorConfig {
    /// Path to the amux binary
    pub amux_binary: PathBuf,
    /// Path to the test-agent binary
    pub test_agent_binary: PathBuf,
    /// Base directory for socket files
    pub socket_dir: PathBuf,
    /// Working directory for commands
    pub working_dir: PathBuf,
    /// Timeout for operations
    pub timeout: Duration,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            amux_binary: PathBuf::from("target/debug/amux"),
            test_agent_binary: PathBuf::from("target/debug/test-agent"),
            socket_dir: PathBuf::from("/tmp"),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            timeout: Duration::from_secs(10),
        }
    }
}

/// Test executor
pub struct Executor {
    config: ExecutorConfig,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    /// Run a test case
    pub fn run_test(&self, test_case: &TestCase) -> TestResult {
        match self.run_test_inner(test_case) {
            Ok(outputs) => TestResult {
                name: test_case.name.clone(),
                passed: true,
                error: None,
                actual_outputs: outputs,
            },
            Err(e) => TestResult {
                name: test_case.name.clone(),
                passed: false,
                error: Some(e),
                actual_outputs: Vec::new(),
            },
        }
    }

    fn run_test_inner(&self, test_case: &TestCase) -> Result<Vec<String>, String> {
        // Create temp directory for config files
        let config_dir = TempDir::new().map_err(|e| format!("Failed to create temp dir: {}", e))?;

        // Generate config files for each config entry
        let mut config_paths: HashMap<String, PathBuf> = HashMap::new();

        for cfg in &test_case.configs {
            // Determine socket path
            let socket_path = match &cfg.socket_path {
                Some(p) if p != "auto" => p.clone(),
                _ => self
                    .config
                    .socket_dir
                    .join(format!("amux-test-{}-{}.sock", test_case.name, cfg.name))
                    .to_string_lossy()
                    .to_string(),
            };

            // Clean up any existing socket
            let _ = std::fs::remove_file(&socket_path);

            // Generate YAML config file
            let host_id = cfg
                .host_id
                .clone()
                .unwrap_or_else(|| test_case.name.clone());
            let yaml_content = format!(
                r#"host_id: "{}"
user_id: "test"
socket_path: "{}"
max_replay_buffer: 10485760
"#,
                host_id, socket_path
            );

            let config_path = config_dir.path().join(format!("{}.yaml", cfg.name));
            std::fs::write(&config_path, yaml_content)
                .map_err(|e| format!("Failed to write config file: {}", e))?;

            config_paths.insert(cfg.name.clone(), config_path);
        }

        // Map terminal names to their config
        let terminal_to_config: HashMap<String, String> = test_case
            .terminals
            .iter()
            .map(|t| (t.name.clone(), t.config.clone()))
            .collect();

        // Active terminals
        let mut terminals: HashMap<String, TestTerminal> = HashMap::new();
        let mut current_terminal: Option<String> = None;
        let mut actual_outputs: Vec<String> = Vec::new();
        let mut last_input: Option<String> = None;

        // Execute test steps
        for step in &test_case.steps {
            match step {
                TestStep::SwitchTerminal(name) => {
                    current_terminal = Some(name.clone());
                }
                TestStep::Input(input) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let config_name = terminal_to_config
                        .get(term_name)
                        .ok_or(format!("Unknown terminal: {}", term_name))?;
                    let config_path = config_paths
                        .get(config_name)
                        .ok_or(format!("Unknown config: {}", config_name))?;

                    // Check if this is an amux command that starts a new terminal session
                    let is_amux_command = input.starts_with("amux ");

                    if is_amux_command && !terminals.contains_key(term_name) {
                        // Transform the command: inject --config and replace test-agent
                        let transformed = self.transform_command(input, config_path);
                        let parts: Vec<&str> = transformed.split_whitespace().collect();

                        // Start a new terminal for this amux session
                        let terminal = TestTerminal::spawn(
                            &parts[0], // amux binary path
                            &parts[1..],
                            &self.config.working_dir,
                            &HashMap::new(), // No env vars needed anymore
                        )
                        .map_err(|e| format!("Failed to spawn terminal {}: {}", term_name, e))?;

                        terminals.insert(term_name.clone(), terminal);

                        // Wait for amux to enter raw mode
                        std::thread::sleep(Duration::from_millis(500));
                    } else if let Some(terminal) = terminals.get_mut(term_name) {
                        // Send input to existing terminal
                        terminal
                            .send_line(input)
                            .map_err(|e| format!("Failed to send input: {}", e))?;
                        // Track for echo stripping
                        last_input = Some(input.clone());
                    } else {
                        return Err(format!(
                            "Terminal {} not initialized. First command must be 'amux ...'",
                            term_name
                        ));
                    }
                }
                TestStep::ExpectOutput(expected) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let terminal = terminals
                        .get_mut(term_name)
                        .ok_or(format!("Terminal {} not initialized", term_name))?;

                    // Read output until NUL (from test-agent)
                    let actual = terminal
                        .read_until_nul(self.config.timeout)
                        .map_err(|e| format!("Failed to read output: {}", e))?;

                    // Strip PTY echo of the input we sent
                    let actual_stripped = if let Some(ref input) = last_input {
                        let echo_pattern = format!("{}\r\n", input);
                        if actual.starts_with(&echo_pattern) {
                            actual[echo_pattern.len()..].to_string()
                        } else {
                            actual.clone()
                        }
                    } else {
                        actual.clone()
                    };

                    let actual_trimmed = actual_stripped.trim();
                    actual_outputs.push(actual_trimmed.to_string());

                    if actual_trimmed != expected {
                        return Err(format!(
                            "Output mismatch in terminal {}:\n  Expected: {:?}\n  Actual:   {:?}",
                            term_name, expected, actual_trimmed
                        ));
                    }
                }
            }
        }

        Ok(actual_outputs)
    }

    /// Transform a command by:
    /// 1. Replacing "amux" with the absolute path to amux binary
    /// 2. Injecting --config after amux
    /// 3. Replacing "test-agent" with the absolute path
    fn transform_command(&self, input: &str, config_path: &Path) -> String {
        let mut result = input.to_string();

        // Replace "amux " with absolute path and --config
        if result.starts_with("amux ") {
            result = format!(
                "{} --config {} {}",
                self.config.amux_binary.display(),
                config_path.display(),
                &result[5..] // Skip "amux "
            );
        }

        // Replace "test-agent" with absolute path
        result = result.replace(
            "test-agent",
            &self.config.test_agent_binary.display().to_string(),
        );

        result
    }
}

/// Clean up test sockets matching a pattern
pub fn cleanup_test_sockets(socket_dir: &Path, test_name: &str) {
    if let Ok(entries) = std::fs::read_dir(socket_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&format!("amux-test-{}", test_name)) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}
