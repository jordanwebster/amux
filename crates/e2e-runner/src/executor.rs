use crate::parser::{Directory, Terminal, TestCase, TestConfig, TestStep};

type PreparedEnvironment = (Vec<Directory>, Vec<TestConfig>, Vec<Terminal>);
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

use crate::terminal::TestTerminal;

#[derive(Debug)]
struct ResolvedCommand {
    program: String,
    args: Vec<String>,
}

/// Check if an amux command is non-interactive (runs and exits).
fn is_oneshot_amux_command(command: &ResolvedCommand) -> bool {
    let mut index = 0;
    while index < command.args.len() {
        match command.args[index].as_str() {
            "--config" => index += 2,
            arg if arg.starts_with("--config=") => index += 1,
            _ => break,
        }
    }

    let Some(subcommand) = command.args.get(index).map(String::as_str) else {
        return true;
    };

    match subcommand {
        "list" | "ls" => true,
        "server" => match command.args.get(index + 1).map(String::as_str) {
            Some("connect" | "stop" | "suspend" | "resume") => true,
            Some("start") => !command.args[index + 2..]
                .iter()
                .any(|arg| arg == "--foreground"),
            _ => false,
        },
        _ => false,
    }
}

fn default_socket_path(base_dir: &Path, test_name: &str, config_name: &str) -> PathBuf {
    let pid = std::process::id();
    #[cfg(unix)]
    {
        let _ = base_dir;
        std::env::temp_dir().join(format!("amux-test-{pid}-{test_name}-{config_name}.sock"))
    }
    #[cfg(windows)]
    {
        let _ = base_dir;
        PathBuf::from(format!(
            r"\\.\pipe\amux-test-{pid}-{test_name}-{config_name}"
        ))
    }
}

/// Result of running a test
#[derive(Debug)]
pub struct TestResult {
    pub passed: bool,
    pub error: Option<String>,
}

/// Configuration for the executor
pub struct ExecutorConfig {
    /// Path to the amux binary
    pub amux_binary: PathBuf,
    /// Path to the test-agent binary
    pub test_agent_binary: PathBuf,
    /// Timeout for read operations
    pub timeout: Duration,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            amux_binary: PathBuf::from("target/debug/amux"),
            test_agent_binary: PathBuf::from("target/debug/test-agent"),
            timeout: Duration::from_millis(200),
        }
    }
}

/// Variable context for substitution
struct VariableContext {
    /// directory name -> path
    directories: HashMap<String, PathBuf>,
    /// config name -> socket_path
    configs: HashMap<String, PathBuf>,
    /// config name -> tcp_port
    tcp_ports: HashMap<String, u16>,
}

impl VariableContext {
    fn new() -> Self {
        Self {
            directories: HashMap::new(),
            configs: HashMap::new(),
            tcp_ports: HashMap::new(),
        }
    }

    /// Substitute variables in a string.
    /// Supports: $name.path (for directories), $name.socket_path (for configs), $name.tcp_port (for configs)
    fn substitute(&self, input: &str) -> String {
        let mut result = input.to_string();

        // Substitute directory variables: $name.path
        for (name, path) in &self.directories {
            let var = format!("${}.path", name);
            result = result.replace(&var, &path.to_string_lossy());
        }

        // Substitute config variables: $name.socket_path
        for (name, socket_path) in &self.configs {
            let var = format!("${}.socket_path", name);
            result = result.replace(&var, &socket_path.to_string_lossy());
        }

        // Substitute config variables: $name.tcp_port
        for (name, port) in &self.tcp_ports {
            let var = format!("${}.tcp_port", name);
            result = result.replace(&var, &port.to_string());
        }

        result
    }
}

/// Test executor
pub struct Executor {
    pub config: ExecutorConfig,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    /// Run a test case
    pub fn run_test(&self, test_case: &TestCase) -> TestResult {
        match self.run_test_inner(test_case) {
            Ok(()) => TestResult {
                passed: true,
                error: None,
            },
            Err(e) => TestResult {
                passed: false,
                error: Some(e),
            },
        }
    }

    fn run_test_inner(&self, test_case: &TestCase) -> Result<(), String> {
        // Create temp directory for test artifacts
        let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp dir: {}", e))?;

        // Prepare environment by auto-injecting missing fields
        let (directories, configs, terminals) =
            self.prepare_environment(test_case, temp_dir.path())?;

        // Build variable context
        let mut var_ctx = VariableContext::new();

        // Create temp directories and populate variable context
        let mut dir_temp_dirs: Vec<TempDir> = Vec::new();
        for dir in &directories {
            let path = if let Some(ref p) = dir.path {
                PathBuf::from(p)
            } else {
                // Create a unique temp directory
                let td = TempDir::new()
                    .map_err(|e| format!("Failed to create temp dir for {}: {}", dir.name, e))?;
                let path = td.path().to_path_buf();
                dir_temp_dirs.push(td);
                path
            };
            // Canonicalize to resolve symlinks (e.g., /var -> /private/var on macOS)
            let canonical_path = path.canonicalize().unwrap_or(path);
            var_ctx.directories.insert(dir.name.clone(), canonical_path);
        }

        // Generate config files and populate config paths in variable context
        let mut config_paths: HashMap<String, PathBuf> = HashMap::new();

        for cfg in &configs {
            // Determine socket path
            let socket_path = match &cfg.socket_path {
                Some(p) if p != "auto" => PathBuf::from(p),
                _ => default_socket_path(temp_dir.path(), &test_case.name, &cfg.name),
            };

            // Clean up any existing socket
            #[cfg(unix)]
            let _ = std::fs::remove_file(&socket_path);

            // Determine TCP port (auto-assign if not specified)
            let tcp_port = match cfg.tcp_port {
                Some(p) => p,
                None => {
                    // Bind to port 0 to get an available port from the OS
                    let listener = std::net::TcpListener::bind("127.0.0.1:0")
                        .map_err(|e| format!("Failed to find free TCP port: {}", e))?;
                    listener
                        .local_addr()
                        .map_err(|e| format!("Failed to get assigned port: {}", e))?
                        .port()
                }
            };

            // Determine WebSocket port (auto-assign if not specified)
            let ws_port = match cfg.websocket_port {
                Some(p) => p,
                None => {
                    // Bind to port 0 to get an available port from the OS
                    let listener = std::net::TcpListener::bind("127.0.0.1:0")
                        .map_err(|e| format!("Failed to find free WebSocket port: {}", e))?;
                    listener
                        .local_addr()
                        .map_err(|e| format!("Failed to get assigned WebSocket port: {}", e))?
                        .port()
                }
            };

            // Allocate a state-file path so each test has an isolated state
            // dir. Left unwritten: init flags live in the config file below;
            // state.yaml is seeded on first write by the binary under test.
            let state_dir = temp_dir.path().join(format!("{}_state", cfg.name));
            std::fs::create_dir_all(&state_dir)
                .map_err(|e| format!("Failed to create state dir: {}", e))?;
            let state_path = state_dir.join("state.yaml");

            // Generate YAML config file.
            // Use single quotes for paths: YAML double-quoted strings treat
            // backslashes as escape characters, so Windows paths like
            // `C:\Users` produce invalid `\U` escapes. Single-quoted YAML
            // strings are literal (no escape processing).
            //
            // `enable_cloud_mode: false` and `prevent_idle_sleep: false` keep
            // `amux init` from prompting during test runs.
            // `check_for_updates: false` keeps e2e output hermetic.
            let host_name = cfg
                .host_name
                .clone()
                .unwrap_or_else(|| test_case.name.clone());
            let yaml_content = format!(
                "host_name: '{}'\nsocket_path: '{}'\ntcp_port: {}\nwebsocket_port: {}\nrandomise_link_name: false\nenable_cloud_mode: false\nprevent_idle_sleep: false\ncheck_for_updates: false\nstate_path: '{}'\n",
                host_name,
                socket_path.display(),
                tcp_port,
                ws_port,
                state_path.display()
            );

            let config_file_path = temp_dir.path().join(format!("{}.yaml", cfg.name));
            std::fs::write(&config_file_path, yaml_content)
                .map_err(|e| format!("Failed to write config file: {}", e))?;

            config_paths.insert(cfg.name.clone(), config_file_path);
            var_ctx.configs.insert(cfg.name.clone(), socket_path);
            var_ctx.tcp_ports.insert(cfg.name.clone(), tcp_port);
        }

        // Map terminal names to their config and cwd
        let terminal_configs: HashMap<String, (String, PathBuf)> = terminals
            .iter()
            .map(|t| {
                let config_name = t.config.clone().unwrap_or_else(|| configs[0].name.clone());
                let cwd = t
                    .cwd
                    .as_ref()
                    .and_then(|name| var_ctx.directories.get(name))
                    .cloned()
                    .unwrap_or_else(|| {
                        var_ctx
                            .directories
                            .values()
                            .next()
                            .cloned()
                            .unwrap_or_else(|| {
                                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                            })
                    });
                (t.name.clone(), (config_name, cwd))
            })
            .collect();

        // Execute test steps, then clean up servers regardless of result
        let result =
            self.execute_steps(&test_case.steps, &terminal_configs, &config_paths, &var_ctx);

        // Cleanup: shut down background servers spawned during the test.
        for config_path in config_paths.values() {
            let _ = Command::new(&self.config.amux_binary)
                .args(["--config", &config_path.to_string_lossy(), "server", "stop"])
                .output();
        }
        #[cfg(unix)]
        for socket_path in var_ctx.configs.values() {
            let _ = std::fs::remove_file(socket_path);
        }

        result
    }

    fn execute_steps(
        &self,
        steps: &[TestStep],
        terminal_configs: &HashMap<String, (String, PathBuf)>,
        config_paths: &HashMap<String, PathBuf>,
        var_ctx: &VariableContext,
    ) -> Result<(), String> {
        let mut active_terminals: HashMap<String, TestTerminal> = HashMap::new();
        let mut oneshot_outputs: HashMap<String, String> = HashMap::new();
        let mut current_terminal: Option<String> = None;

        for step in steps {
            match step {
                TestStep::SwitchTerminal(name) => {
                    current_terminal = Some(name.clone());
                }
                TestStep::Sleep(ms) => {
                    std::thread::sleep(Duration::from_millis(*ms));
                }
                TestStep::Input(input) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let (config_name, cwd) = terminal_configs
                        .get(term_name)
                        .ok_or(format!("Unknown terminal: {}", term_name))?;
                    let config_path = config_paths
                        .get(config_name)
                        .ok_or(format!("Unknown config: {}", config_name))?;

                    let input_substituted = var_ctx.substitute(input);
                    let is_amux_command =
                        input_substituted == "amux" || input_substituted.starts_with("amux ");

                    if is_amux_command && !active_terminals.contains_key(term_name) {
                        let transformed =
                            self.transform_command(&input_substituted, config_path)?;

                        if is_oneshot_amux_command(&transformed) {
                            let output = Command::new(&transformed.program)
                                .args(&transformed.args)
                                .current_dir(cwd)
                                .output()
                                .map_err(|e| format!("Failed to run oneshot command: {}", e))?;

                            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                            let combined = if stderr.is_empty() {
                                stdout
                            } else if stdout.is_empty() {
                                stderr
                            } else {
                                format!("{}{}", stdout, stderr)
                            };
                            oneshot_outputs.insert(term_name.clone(), combined);
                        } else {
                            let terminal = TestTerminal::spawn(
                                &transformed.program,
                                &transformed.args,
                                cwd,
                                &HashMap::new(),
                            )
                            .map_err(|e| {
                                format!("Failed to spawn terminal {}: {}", term_name, e)
                            })?;

                            active_terminals.insert(term_name.clone(), terminal);

                            // Wait for amux to initialize
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    } else if let Some(terminal) = active_terminals.get_mut(term_name) {
                        terminal
                            .send_line(&input_substituted)
                            .map_err(|e| format!("Failed to send input: {}", e))?;
                    } else {
                        return Err(format!(
                            "Terminal {} not initialized. First command must be 'amux ...'",
                            term_name
                        ));
                    }
                }
                TestStep::ExpectOutput(expected) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;

                    let expected_substituted = var_ctx.substitute(expected);
                    let expected_with_newline = format!("{}\n", expected_substituted);

                    let actual = if let Some(output) = oneshot_outputs.remove(term_name) {
                        output
                    } else {
                        let terminal = active_terminals
                            .get_mut(term_name)
                            .ok_or(format!("Terminal {} not initialized", term_name))?;

                        terminal
                            .read_expected(&expected_with_newline, self.config.timeout)
                            .map_err(|e| format!("Failed to read output: {}", e))?
                    };

                    if actual != expected_with_newline {
                        return Err(format!(
                            "Output mismatch in terminal {}:\n  Expected: {:?}\n  Actual:   {:?}",
                            term_name, expected_with_newline, actual
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Prepare environment by auto-injecting missing fields
    fn prepare_environment(
        &self,
        test_case: &TestCase,
        _temp_dir: &Path,
    ) -> Result<PreparedEnvironment, String> {
        let mut directories = test_case.directories.clone();
        let mut configs = test_case.configs.clone();
        let terminals = test_case.terminals.clone();

        // If no directories, create a default "cwd" directory
        if directories.is_empty() {
            directories.push(Directory {
                name: "cwd".to_string(),
                path: None, // Will be auto-generated
            });
        }

        // If no configs, create a default "local" config
        if configs.is_empty() {
            configs.push(TestConfig {
                name: "local".to_string(),
                host_name: None,
                socket_path: None,
                tcp_port: None,
                websocket_port: None,
            });
        }

        // Terminals must have at least a name - validation
        if terminals.is_empty() {
            return Err("No terminals defined".to_string());
        }

        Ok((directories, configs, terminals))
    }

    /// Transform a command by:
    /// 1. Replacing "amux" with the absolute path to amux binary
    /// 2. Injecting --config after amux
    /// 3. Replacing "test-agent" with the absolute path
    fn transform_command(
        &self,
        input: &str,
        config_path: &Path,
    ) -> Result<ResolvedCommand, String> {
        let mut parts =
            shell_words::split(input).map_err(|e| format!("Failed to parse command: {}", e))?;
        if parts.is_empty() {
            return Err("Command cannot be empty".to_string());
        }

        if parts[0] == "amux" {
            parts[0] = self.config.amux_binary.display().to_string();
            parts.insert(1, "--config".to_string());
            parts.insert(2, config_path.display().to_string());
        }

        for part in &mut parts {
            if part == "test-agent" {
                *part = self.config.test_agent_binary.display().to_string();
            }
        }

        Ok(ResolvedCommand {
            program: parts.remove(0),
            args: parts,
        })
    }
}
