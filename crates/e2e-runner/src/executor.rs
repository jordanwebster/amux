use crate::parser::{Directory, RetryPolicy, Terminal, TestCase, TestConfig, TestStep};

type PreparedEnvironment = (Vec<Directory>, Vec<TestConfig>, Vec<Terminal>);
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use tempfile::TempDir;

use crate::terminal::TestTerminal;

#[derive(Debug, Clone)]
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

fn run_oneshot_command(command: &ResolvedCommand, cwd: &Path) -> Result<String, String> {
    let output = Command::new(&command.program)
        .args(&command.args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Failed to run oneshot command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(if stderr.is_empty() {
        stdout
    } else if stdout.is_empty() {
        stderr
    } else {
        format!("{}{}", stdout, stderr)
    })
}

fn retry_oneshot_until_expected(
    command: &ResolvedCommand,
    cwd: &Path,
    expected: &str,
    policy: RetryPolicy,
    first_output: String,
) -> Result<String, String> {
    let started = Instant::now();
    let timeout = Duration::from_millis(policy.timeout_ms);
    let interval = Duration::from_millis(policy.interval_ms);
    let mut actual = first_output;

    while actual != expected && started.elapsed() < timeout {
        thread::sleep(interval);
        actual = run_oneshot_command(command, cwd)?;
    }

    Ok(actual)
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

const E2E_USER_ID: &str = "11111111-1111-4111-8111-111111111111";
const E2E_REFRESH_TOKEN: &str = "e2e-refresh";
const E2E_ACCESS_TOKEN: &str = "e2e-access";
const E2E_JWT_KID: &str = "e2e-key";
const E2E_JWT_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDa10VP9rc+oAG3
+JhkaPK/OJSo2y00s5pUobICMpfWApDpnoEsPJf/3yvRvlIJnQMK+eQNtxSdngFP
1O3P6vgpL0MkB7CAOxbe2WB4LFZ0wHuQzxyO0Bv9YDLvZidNg7FKyxhnARyVK0m0
OFwvW5dn/L6POAxEouadWWHbeyDem1BsOcEAT2spQzqeVKZc2VlZJ2FO/CapGYvi
p7bMOAMbiIPdklfTFRj1eGA4BlLlrw645YEvlo0fCMrVZNYb+sGI1SImVfoToaxl
dcT1lagnKE1+ERL8jPqLcbx66jIOq/Nf5hRJOHuayfjz2uUqfYduumKu5NlryBv+
OiVeQmm3AgMBAAECggEARcoJDKs9XPdiFO1ui/b8EwdUQVVEYV41hW/beN/xlApV
dGtb/mOEhdECBG2RdAdihQmUNNuB85IEERVyka/5XAj6fG8HVp2BeagRH8HkAG+x
+EhUbybnBjK7i6UkO5AX5iZGrfKoztlzM8oVe/TVoA/2JW5WWz0oFl3+2yO1I8gE
4qTcP+iNFgNa2SDu0ALjiDgVUDhap+Rs4R9qd5mxswdGYUfD9oqBcouxGZVPgv6n
Xe66iowrnWfc25bD3swXPmsTBF1lncGPuSHrVwPYBFLb6rSbjtOh8aJf61qqRO/s
w44UIcOAhZ15Qv5I1rbbPHoDiRK1a1VUEpPyWxZ/IQKBgQD8YwovOoSRSnzVorIa
RlstKai7iOE6cFkrEVQUvojJcUNmfW99cMtCrGDkXQTahnpov7m2qRho/oNCPYpr
tECo8vyiMW857CaLVZVQiHO5PvzkdCKqhdR4CNsDYpXCMqBtL0Qgn6WagsU1Qw4X
uj9wgOxtBHoSgufe8rf1hXyLWwKBgQDd+Uo8gvP+rua4g4zkXZzyeucvQC5KJHqM
8YNPdaZ8+cb72yMIp/p3BqPoj2zzyX+uGW7opEwwQjAO5VG5wh+jW6j09s6e5Zes
3IJ55v5f40ioUkpxPaa3EQOSRQfi9EVRVJv6bPidRVCS4rtQMccB3oCxq+iQ8OCj
QAf7PNwV1QKBgCG9N6pSn1Aw7fk9M6PxjdS+wfC3/qvqQvFP8raHNg//1SvJTvMs
9e8mzhkZGkIAQjLolnIFrt6yT2e2hF+bjB1Jxl4ET8Mlf42W1kwawaWc9v+vSscS
9vFI9cZBEpYQYIPYErptvRynqKdTHHotireGdJSqSYtZ9pdGSTNIMfsLAoGAYSHj
MFOFfZ7/ayJ1lsC4GwtY+r41A1CvJ9nPQggTkICkaDVeQT1wRoFrXCrW3F8CNib+
92JdzIhKC1qhxo2B1rQXXQpbJAEHvCbKGZnRGhiVBMLtvFvkBhu12l3Gs7N8WbiS
gKUKrZdVSNFach82HEVHP3ggTrx5MDamx3O8QvkCgYBytDiq72xVjklJLWOsSbBo
X7zVUE2d5y01FsN30UQ4nNdCGmEdvu206B1n3Clel1kepYd9Mn7JQgzrBQQwz5UP
06MxC3IfJVYcFmiZ7Kb4ggeBW1QbUbbsb2Jbuv7wNoPcIAE2c5PwnmgacnhVB58O
qr8VpwUTpFt0PnPahUNCRw==
-----END PRIVATE KEY-----"#;
const E2E_JWK_N: &str = "2tdFT_a3PqABt_iYZGjyvziUqNstNLOaVKGyAjKX1gKQ6Z6BLDyX_98r0b5SCZ0DCvnkDbcUnZ4BT9Ttz-r4KS9DJAewgDsW3tlgeCxWdMB7kM8cjtAb_WAy72YnTYOxSssYZwEclStJtDhcL1uXZ_y-jzgMRKLmnVlh23sg3ptQbDnBAE9rKUM6nlSmXNlZWSdhTvwmqRmL4qe2zDgDG4iD3ZJX0xUY9XhgOAZS5a8OuOWBL5aNHwjK1WTWG_rBiNUiJlX6E6GsZXXE9ZWoJyhNfhES_Iz6i3G8euoyDqvzX-YUSTh7msn489rlKn2HbrpiruTZa8gb_jolXkJptw";
const E2E_JWK_E: &str = "AQAB";

struct CloudFixture {
    url: String,
    running: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for CloudFixture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Serialize)]
struct RoutingClaims<'a> {
    sub: &'a str,
    client_id: &'a str,
    host: &'a str,
    port: u16,
    exp: u64,
    aud: &'a str,
}

fn allocate_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("failed to allocate local port: {error}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| format!("failed to read allocated local port: {error}"))
}

fn configure_cloud_fixture(configs: &mut [TestConfig]) -> Result<Option<CloudFixture>, String> {
    let needs_cloud = configs
        .iter()
        .any(|config| config.cloud_relay || config.enable_cloud_mode == Some(true));
    if !needs_cloud {
        return Ok(None);
    }

    let relay = configs
        .iter_mut()
        .find(|config| config.cloud_relay)
        .ok_or_else(|| {
            "cloud-enabled e2e configs require one config with cloud_relay: true".to_string()
        })?;
    let routing_host = relay
        .host_name
        .clone()
        .unwrap_or_else(|| relay.name.clone());
    let routing_port = match relay.tcp_port {
        Some(port) => port,
        None => {
            let port = allocate_local_port()?;
            relay.tcp_port = Some(port);
            port
        }
    };
    relay.enable_cloud_mode.get_or_insert(true);
    relay.enforce_tls_in_cloud_mode.get_or_insert(false);

    let fixture = CloudFixture::start(routing_host, routing_port)?;
    for config in configs {
        if config.cloud_relay || config.enable_cloud_mode == Some(true) {
            config.cloud_url = Some(fixture.url.clone());
        }
    }
    Ok(Some(fixture))
}

impl CloudFixture {
    fn start(routing_host: String, routing_port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("failed to start fake cloud API: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read fake cloud API address: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("failed to configure fake cloud API: {error}"))?;

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        let thread_host = routing_host.clone();
        let thread = thread::spawn(move || {
            while thread_running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle_cloud_request(stream, &thread_host, routing_port),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            url: format!("http://{addr}"),
            running,
            thread: Some(thread),
        })
    }
}

fn handle_cloud_request(mut stream: TcpStream, routing_host: &str, routing_port: u16) {
    let mut request = [0_u8; 4096];
    let bytes_read = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..bytes_read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let (status, body) = match path {
        "/connect/token" => (
            "200 OK",
            serde_json::json!({
                "access_token": E2E_ACCESS_TOKEN,
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": E2E_REFRESH_TOKEN,
            })
            .to_string(),
        ),
        "/api/connect" => {
            let token = routing_token(routing_host, routing_port);
            (
                "200 OK",
                serde_json::json!({
                    "host": "127.0.0.1",
                    "port": routing_port,
                    "token": token,
                    "expires_at": (Utc::now() + ChronoDuration::hours(1)).to_rfc3339(),
                })
                .to_string(),
            )
        }
        "/.well-known/openid-configuration/jwks" => (
            "200 OK",
            serde_json::json!({
                "keys": [{
                    "kid": E2E_JWT_KID,
                    "kty": "RSA",
                    "alg": "RS256",
                    "use": "sig",
                    "n": E2E_JWK_N,
                    "e": E2E_JWK_E,
                }]
            })
            .to_string(),
        ),
        _ => ("404 Not Found", "{}".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn routing_token(routing_host: &str, routing_port: u16) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(E2E_JWT_KID.to_string());
    let claims = RoutingClaims {
        sub: E2E_USER_ID,
        client_id: "e2e",
        host: routing_host,
        port: routing_port,
        exp: (Utc::now() + ChronoDuration::hours(1)).timestamp() as u64,
        aud: "amux_token",
    };
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(E2E_JWT_PRIVATE_KEY.as_bytes())
            .expect("embedded e2e RSA key should parse"),
    )
    .expect("embedded e2e RSA key should sign routing token")
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
    /// Supports: $name.path (for directories), $name.socket_path and
    /// $name.tcp_port (for configs)
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

        for (name, tcp_port) in &self.tcp_ports {
            let var = format!("${}.tcp_port", name);
            result = result.replace(&var, &tcp_port.to_string());
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
        let (directories, mut configs, terminals) =
            self.prepare_environment(test_case, temp_dir.path())?;
        let _cloud_fixture = configure_cloud_fixture(&mut configs)?;

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

            let tcp_port = cfg.tcp_port.unwrap_or(allocate_local_port()?);

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
            let host_name = cfg
                .host_name
                .clone()
                .unwrap_or_else(|| test_case.name.clone());
            let enable_cloud_mode = cfg.enable_cloud_mode.unwrap_or(false);
            if enable_cloud_mode && !cfg.cloud_relay {
                std::fs::write(
                    state_dir.join("auth.yaml"),
                    format!("refresh_token: '{}'\n", E2E_REFRESH_TOKEN),
                )
                .map_err(|e| format!("Failed to write auth file: {}", e))?;
            }
            let mut yaml_content = format!(
                "host_name: '{}'\nsocket_path: '{}'\ntcp_port: {}\nrandomise_link_name: false\nenable_cloud_mode: {}\nprevent_idle_sleep: false\nstate_path: '{}'\n",
                host_name,
                socket_path.display(),
                tcp_port,
                enable_cloud_mode,
                state_path.display()
            );
            if let Some(cloud_url) = &cfg.cloud_url {
                yaml_content.push_str(&format!("cloud_url: '{}'\n", cloud_url));
            }
            if let Some(enforce_tls) = cfg.enforce_tls_in_cloud_mode {
                yaml_content.push_str(&format!("enforce_tls_in_cloud_mode: {}\n", enforce_tls));
            }

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
        let mut last_oneshot_commands: HashMap<String, (ResolvedCommand, PathBuf)> = HashMap::new();
        let mut retry_next_expect: Option<RetryPolicy> = None;
        let mut current_terminal: Option<String> = None;

        for step in steps {
            match step {
                TestStep::SwitchTerminal(name) => {
                    current_terminal = Some(name.clone());
                }
                TestStep::Sleep(ms) => {
                    std::thread::sleep(Duration::from_millis(*ms));
                }
                TestStep::RetryNextExpect(policy) => {
                    retry_next_expect = Some(*policy);
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
                            let combined = run_oneshot_command(&transformed, cwd)?;
                            last_oneshot_commands
                                .insert(term_name.clone(), (transformed, cwd.clone()));
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
                    let retry_policy = retry_next_expect.take();

                    let expected_substituted = var_ctx.substitute(expected);
                    let expected_with_newline = format!("{}\n", expected_substituted);

                    let actual = if let Some(output) = oneshot_outputs.remove(term_name) {
                        if let Some(policy) = retry_policy {
                            let (command, cwd) = last_oneshot_commands.get(term_name).ok_or(
                                "Retry directive requires a previous one-shot amux command",
                            )?;
                            retry_oneshot_until_expected(
                                command,
                                cwd,
                                &expected_with_newline,
                                policy,
                                output,
                            )?
                        } else {
                            output
                        }
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
                enable_cloud_mode: None,
                cloud_url: None,
                tcp_port: None,
                enforce_tls_in_cloud_mode: None,
                cloud_relay: false,
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
