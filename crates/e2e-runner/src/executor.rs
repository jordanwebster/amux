use crate::parser::{Directory, RetryPolicy, Terminal, TestCase, TestConfig, TestStep};

type PreparedEnvironment = (Vec<Directory>, Vec<TestConfig>, Vec<Terminal>);
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
            "--config" | "--profile" => index += 2,
            arg if arg.starts_with("--config=") || arg.starts_with("--profile=") => index += 1,
            _ => break,
        }
    }

    let Some(subcommand) = command.args.get(index).map(String::as_str) else {
        return true;
    };

    match subcommand {
        "client" | "list" | "ls" | "profiles" | "init" | "login" | "logout" => true,
        "profile" => {
            command
                .args
                .get(index + 1)
                .is_none_or(|arg| arg != "delete")
                || command.args.iter().any(|arg| arg == "--yes")
        }
        "server" => match command.args.get(index + 1).map(String::as_str) {
            Some("stop" | "suspend" | "resume") => true,
            Some("start") => !command.args[index + 2..]
                .iter()
                .any(|arg| arg == "--foreground"),
            _ => false,
        },
        _ => false,
    }
}

fn run_oneshot_command(
    command: &ResolvedCommand,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let output = Command::new(&command.program)
        .args(&command.args)
        .current_dir(cwd)
        .env_remove("AMUX_CONFIG")
        .envs(env)
        .output()
        .map_err(|e| format!("Failed to run oneshot command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!(
            "Command exited with {}:\n{stdout}{stderr}",
            output.status
        ));
    }

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
    env: &HashMap<String, String>,
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
        actual = run_oneshot_command(command, cwd, env)?;
    }

    Ok(actual)
}

fn append_config_logs(
    mut error: String,
    config_envs: &HashMap<String, HashMap<String, String>>,
) -> String {
    let mut names = config_envs.keys().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let Some(log_path) = config_envs.get(name).and_then(|env| env.get("AMUX_LOG")) else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(log_path) else {
            continue;
        };
        if contents.trim().is_empty() {
            continue;
        }
        error.push_str(&format!("\n\n--- {name} amux.log ({log_path}) ---\n"));
        error.push_str(&tail_lines(&contents, 200));
    }
    error
}

fn tail_lines(contents: &str, max_lines: usize) -> String {
    let lines = contents.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    let mut tail = lines[start..].join("\n");
    if !tail.is_empty() {
        tail.push('\n');
    }
    tail
}

fn write_fixture_yaml(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    std::fs::write(
        path,
        serde_yaml::to_string(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

fn default_socket_path(base_dir: &Path, test_name: &str, config_name: &str) -> PathBuf {
    let pid = std::process::id();
    #[cfg(unix)]
    {
        let _ = base_dir;
        PathBuf::from("/tmp").join(format!("amux-test-{pid}-{test_name}-{config_name}.sock"))
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
    /// Optional directory for command/output transcripts captured at the CLI boundary.
    pub transcript_dir: Option<PathBuf>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            amux_binary: PathBuf::from("target/debug/amux"),
            test_agent_binary: PathBuf::from("target/debug/test-agent"),
            timeout: Duration::from_secs(2),
            transcript_dir: std::env::var_os("E2E_TRANSCRIPT_DIR").map(PathBuf::from),
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
    /// captured output variable name -> value
    captures: HashMap<String, String>,
}

const E2E_USER_ID: &str = "11111111-1111-4111-8111-111111111111";

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
const E2E_CLOUD_TLS_CA: &str = r#"-----BEGIN CERTIFICATE-----
MIIDGTCCAgGgAwIBAgIUb37PUGBCfNL02QQ6MxsMhbku/FkwDQYJKoZIhvcNAQEL
BQAwHDEaMBgGA1UEAwwRYW11eC1lMmUtY2xvdWQtY2EwHhcNMjYwNTE5MTAyNTI2
WhcNMzYwNTE2MTAyNTI2WjAcMRowGAYDVQQDDBFhbXV4LWUyZS1jbG91ZC1jYTCC
ASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBANfq+5Eq48ZaLg2wcpKun4db
EcAQy48B0z+ML/pgQjPA98bpqiY/qCjS9gQc3wLSM486LaTh6+Us5q9AEZb09ZuY
H9PDqkZ4bKXxIdkNHOFjq8QoFSkzOZJz9zKEZ87JH2Z8FMrXJkgWJEmJL0RNtOMx
eXKAgC5eztoOPMA0UQTJWCKv5RRPeGOG+I6ps03pyMKh7QPTj3fRdeB4UhTtomNi
CJRgwwZigf8aq+YezTXCKPpQlziP5PrENlmN96t4Xl2jiCkKkBL8uHGU2z662esf
wRwFXOB+ZW1dhFBtkJ2+HTvs0R9zXfPmd/oj1CPQP3Z2dImSBIA43vqLT7PJ9qUC
AwEAAaNTMFEwHQYDVR0OBBYEFAswMn/umvBjTlplcV7rRKLYILYCMB8GA1UdIwQY
MBaAFAswMn/umvBjTlplcV7rRKLYILYCMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZI
hvcNAQELBQADggEBAAGq6C5cvBYrXC48egmjL/e79o7sCOsjh7BRTElkebeNoreN
yRarwcSzUwOfa6YFIqECdS5FPKOmRH+NtlBQl+ZQyq4lh7CKWREOLVJYzfpBdi28
Q/OsWGUuA3oYfvDyXm3xxEXeY/kFTAC+yTOCqRQfN0jCL58Y3K+69iyV5DWLqzd6
k2VDVdU1pv0J87BwWIMXWRI1Unb2cnUlOeVfB9B1Wh7A07WgfJ6OEQ3mOMmi3xd/
JnOQYM8lYvIQnui0RKlNX1WsNnfnYN34KvLIf8RACeCmMYSeXiD/xm0efkw24sgg
WWw8TqG1kFSTmb1KjJdbmC4mdO/TkkKYMsvwhPg=
-----END CERTIFICATE-----"#;
const E2E_CLOUD_TLS_CERT: &str = r#"-----BEGIN CERTIFICATE-----
MIIDSzCCAjOgAwIBAgIUNKY7gJVX/L7F63F2Am2QrzwpyAQwDQYJKoZIhvcNAQEL
BQAwHDEaMBgGA1UEAwwRYW11eC1lMmUtY2xvdWQtY2EwHhcNMjYwNTE5MTAyNTI2
WhcNMzYwNTE2MTAyNTI2WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqG
SIb3DQEBAQUAA4IBDwAwggEKAoIBAQDdZk1QHIDbSam+fJvWb7+YCKbDB0iF70j7
l0POY0UP1D4ElsyHMDqHXkn9qnry1KTf1sP2/6fhLUYV8LfW103Zi4CI0L4Utz4H
vwep8+Nz3OHPxc/Ue/NQl/bGs9z+URKF+8tC+sFN5/oEpfzYq3PgDPS0BFLHHX3b
xDjwpynLYxvpuTS3aECth+v6/iZiwMxAQi3eAlIxtdLKd7JswNBOBChx7zW0/L4K
DIzX8VXxyIUH8ESBV0Fs9exTQLP5OmIqkQ3KsUns3F2dGMjkToOs8n3cZUZQUMP2
PNWTS3aycOwuaGajyRDJNPWt+BkmS40cPHubFSKGYODAHbg5DxARAgMBAAGjgYww
gYkwCQYDVR0TBAIwADALBgNVHQ8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEw
GgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMB0GA1UdDgQWBBR9KWwfNLKu8JP1
zdbCBORo5wz/1TAfBgNVHSMEGDAWgBQLMDJ/7prwY05aZXFe60Si2CC2AjANBgkq
hkiG9w0BAQsFAAOCAQEAAB9ftHtsPyTYVBf5O3TlAimSz04nVF5FPmpGPIZ6HGLu
pJxNqT8a2f4b5EYBcncoz5p+YTz1wvZ/aJmDdwHcNFqOhcwX886O4nNv1HIfmY9K
Di2Y9qj1CLh5ZSq+2HaYbm2bN0dNvteNKEewtdVV2XTmbxBF+lJIAwAWEv6IhdNY
tJKq9+Csgn2qViv+63uuRky2vS0zVBNwnkvtGs9fmYTmcRyUUDJ5WkJ2+1Yly7B8
XbSNyBpRotjjUtssfsvgPpDpv7eM2P9iUuvTaIMWQa2x+2IO8oMRqZBX8LRqznsK
q5VD8R2wlqfk62Nxa0kWrZICp/6rW+57CpqlyhTcCg==
-----END CERTIFICATE-----"#;
const E2E_CLOUD_TLS_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDdZk1QHIDbSam+
fJvWb7+YCKbDB0iF70j7l0POY0UP1D4ElsyHMDqHXkn9qnry1KTf1sP2/6fhLUYV
8LfW103Zi4CI0L4Utz4Hvwep8+Nz3OHPxc/Ue/NQl/bGs9z+URKF+8tC+sFN5/oE
pfzYq3PgDPS0BFLHHX3bxDjwpynLYxvpuTS3aECth+v6/iZiwMxAQi3eAlIxtdLK
d7JswNBOBChx7zW0/L4KDIzX8VXxyIUH8ESBV0Fs9exTQLP5OmIqkQ3KsUns3F2d
GMjkToOs8n3cZUZQUMP2PNWTS3aycOwuaGajyRDJNPWt+BkmS40cPHubFSKGYODA
Hbg5DxARAgMBAAECggEABWaSyZAobzD40bYQccaqvHau5WBZcIr0aM65JLZfNOek
gPrSD79+w1aVeiPyeSyevlHALgJGghkB9f8NPPxmNcGlZ7D6h17WRdzEacI9RieZ
NTb0vuYsepwlCkEmSZMzXyP+l/+t6iHtLg0ugcqM5RDr1zL+d0T3kP4ptWpz0TY/
a6iBJcQyzFi/GXsRv5+qgv2c98KWGnyRQ5HtgPJvffbe5tPUhzusmmEhNTx6wGMI
HPh3Zqdaq069zlDz9t1YsKW/QlpkweJNSK7Lja3ZGJX2ri14o3H3vaplfs2eAgKO
AIS4cU2B6xhQVOTgnK1ACmI94FtQFiFLYGeL/CSgwQKBgQDxk3xiSx58A+5gKdMS
cfiJeiMuNyisZ1+AiNRGlXP0Wji6B93By718LlZyTqdg1xc8yFw8YE73NywU11Ky
QCFXSTdatOFihUv3BXesZOwxITMg20ZJ2RtDauFUabHNa5lWeiaU/bcCCPOywSuf
qG7YDl4Iw7IlMm6MifuT0NheRQKBgQDqnmqZDUYMUKN01AYM4/Vlk+0LLMsLP/sx
I+BQQ8YQMtRClgqClHNhV7KKQDYaVHtltmlFHuISsq4e10w6HoTv30CFKgtnCjJg
ItN2T1VH6Fk1e3BPX/IptDkM0f09ugxR9LqDLfp7i5ZUqqFRUPCplUvwLvMJ6FRN
iLia9JsdXQKBgGDzrhHM0Bk5gqu5XWqjrvmNuRzNKle2zQ9K2tbRGE5S/z059vfW
CuARwMPzaR1mdX8BcnMQu+BfliNvH1NGhZsAWWTf/yyJDqm+2f6oKlq1Vk2zcwwk
Q9rUxEYafS9SJaIdN+rHwHDiott0x0s2T/YKHhcqYw6mpNNmdT8nrA55AoGBAJxY
zx6JKunf/t1GwXVrn8d+KVPuGKy5iVI43y19zIpU5QAubniQJsdyoobgvW0UaVrh
kQs/xlXBfqkMvj5owhv7gUp8NzcGI4XPD23i9ijCHFi4lqI+hOjnsbDqasDsr3Ma
DASI6kfUQGzRfEjtEENiO0Wmc81hZnR4rNSONqP9AoGBAMRjgn8H/vD6VoLLH3JH
1dkJs1UeYDuphsxQDSL8BSZ3A/KdpVXTWr/GpQP+CEirAJMB7SUhvKt8w8ON2gS5
e+JjcThHZj+6Byi+a909uj8Ql6adrYeAa+tODJRSICJYHjhGHMGT/3cqsjmEKwkW
1Y9XHTmF4wHjXj00ZfEgep1N
-----END PRIVATE KEY-----"#;

struct CloudFixture {
    url: String,
    identity: Arc<Mutex<IdentityState>>,
    routing_tls_ca: PathBuf,
    routing_tls_cert: PathBuf,
    routing_tls_key: PathBuf,
    _tls_dir: TempDir,
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
        .any(|config| config.cloud_relay || config.cloud_account.is_some());
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
    relay.host_name = Some(routing_host.clone());
    let routing_port = match relay.tcp_port {
        Some(0) | None => {
            let port = allocate_local_port()?;
            relay.tcp_port = Some(port);
            port
        }
        Some(port) => port,
    };
    let accounts = relay.accounts.clone();
    let fixture = CloudFixture::start(routing_host, routing_port, &accounts)?;
    for config in configs {
        config.cloud_url = Some(fixture.url.clone());
    }
    Ok(Some(fixture))
}

impl CloudFixture {
    fn start(routing_host: String, routing_port: u16, accounts: &[String]) -> Result<Self, String> {
        let tls_dir =
            TempDir::new().map_err(|error| format!("failed to create cloud TLS dir: {error}"))?;
        let routing_tls_ca = tls_dir.path().join("cloud-routing-ca.pem");
        let routing_tls_cert = tls_dir.path().join("cloud-routing-cert.pem");
        let routing_tls_key = tls_dir.path().join("cloud-routing-key.pem");
        std::fs::write(&routing_tls_ca, E2E_CLOUD_TLS_CA)
            .map_err(|error| format!("failed to write cloud TLS CA: {error}"))?;
        std::fs::write(&routing_tls_cert, E2E_CLOUD_TLS_CERT)
            .map_err(|error| format!("failed to write cloud TLS cert: {error}"))?;
        std::fs::write(&routing_tls_key, E2E_CLOUD_TLS_KEY)
            .map_err(|error| format!("failed to write cloud TLS key: {error}"))?;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("failed to start fake cloud API: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read fake cloud API address: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("failed to configure fake cloud API: {error}"))?;

        let identity = Arc::new(Mutex::new(IdentityState::new(accounts)?));
        let thread_identity = identity.clone();
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        let thread_host = routing_host.clone();
        let thread = thread::spawn(move || {
            while thread_running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle_cloud_request(
                        stream,
                        &thread_host,
                        routing_port,
                        &mut thread_identity.lock().unwrap(),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            url: format!("http://{addr}"),
            identity,
            routing_tls_ca,
            routing_tls_cert,
            routing_tls_key,
            _tls_dir: tls_dir,
            running,
            thread: Some(thread),
        })
    }
}

#[derive(Clone, Copy)]
struct FixtureAccount {
    sub: &'static str,
    display: &'static str,
    email: &'static str,
}

fn fixture_account(name: &str) -> Result<FixtureAccount, String> {
    match name {
        "alice" => Ok(FixtureAccount {
            sub: E2E_USER_ID,
            display: "Alice Example",
            email: "alice@example.test",
        }),
        "bob" => Ok(FixtureAccount {
            sub: "22222222-2222-4222-8222-222222222222",
            display: "Bob Example",
            email: "bob@example.test",
        }),
        _ => Err(format!("Unknown fixture account {name:?}")),
    }
}

struct IdentityState {
    accounts: VecDeque<FixtureAccount>,
    fallback: FixtureAccount,
    devices: HashMap<String, FixtureAccount>,
    refresh: HashMap<String, FixtureAccount>,
    access: HashMap<String, FixtureAccount>,
}

impl IdentityState {
    fn new(accounts: &[String]) -> Result<Self, String> {
        let accounts = accounts
            .iter()
            .map(|name| fixture_account(name))
            .collect::<Result<VecDeque<_>, _>>()?;
        Ok(Self {
            fallback: accounts
                .back()
                .copied()
                .unwrap_or(fixture_account("alice")?),
            accounts,
            devices: HashMap::new(),
            refresh: HashMap::new(),
            access: HashMap::new(),
        })
    }

    fn tokens(&mut self, account: FixtureAccount) -> serde_json::Value {
        let refresh = uuid::Uuid::new_v4().to_string();
        let access = uuid::Uuid::new_v4().to_string();
        self.refresh.insert(refresh.clone(), account);
        self.access.insert(access.clone(), account);
        serde_json::json!({"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 3600})
    }

    fn respond(
        &mut self,
        path: &str,
        form: &HashMap<String, String>,
        bearer: Option<&str>,
        routing_host: &str,
        routing_port: u16,
    ) -> (&'static str, serde_json::Value) {
        match path {
            "/connect/deviceauthorization" => {
                let scopes = form
                    .get("scope")
                    .map(String::as_str)
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>();
                if !["openid", "profile", "email", "offline_access", "api"]
                    .iter()
                    .all(|scope| scopes.contains(scope))
                {
                    return (
                        "400 Bad Request",
                        serde_json::json!({"error": "invalid_scope"}),
                    );
                }
                let account = self.accounts.pop_front().unwrap_or(self.fallback);
                let device = uuid::Uuid::new_v4().to_string();
                self.devices.insert(device.clone(), account);
                (
                    "200 OK",
                    serde_json::json!({"device_code": device, "user_code": "E2E-APPROVED", "verification_uri": "https://identity.example.test/activate", "expires_in": 600, "interval": 1}),
                )
            }
            "/connect/token" => {
                let account = match form.get("grant_type").map(String::as_str) {
                    Some("urn:ietf:params:oauth:grant-type:device_code") => form
                        .get("device_code")
                        .and_then(|code| self.devices.remove(code)),
                    Some("refresh_token") => form
                        .get("refresh_token")
                        .and_then(|token| self.refresh.remove(token)),
                    _ => None,
                };
                match account {
                    Some(account) => ("200 OK", self.tokens(account)),
                    None => (
                        "400 Bad Request",
                        serde_json::json!({"error": "invalid_grant"}),
                    ),
                }
            }
            "/connect/userinfo" | "/api/connect" => {
                let Some(account) = bearer.and_then(|token| self.access.get(token)) else {
                    return (
                        "401 Unauthorized",
                        serde_json::json!({"error": "invalid_token"}),
                    );
                };
                if path == "/connect/userinfo" {
                    (
                        "200 OK",
                        serde_json::json!({"sub": account.sub, "name": account.display, "email": account.email}),
                    )
                } else {
                    (
                        "200 OK",
                        serde_json::json!({"host": "localhost", "port": routing_port, "token": routing_token(routing_host, routing_port, account.sub), "expires_at": (Utc::now() + ChronoDuration::hours(1)).to_rfc3339()}),
                    )
                }
            }
            "/.well-known/openid-configuration/jwks" => (
                "200 OK",
                serde_json::json!({"keys": [{"kid": E2E_JWT_KID, "kty": "RSA", "alg": "RS256", "use": "sig", "n": E2E_JWK_N, "e": E2E_JWK_E}]}),
            ),
            _ => ("404 Not Found", serde_json::json!({})),
        }
    }
}

fn handle_cloud_request(
    mut stream: TcpStream,
    routing_host: &str,
    routing_port: u16,
    identity: &mut IdentityState,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let (headers_end, content_length) = loop {
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
        if request.len() > 65536 {
            return;
        }
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if length > 65536 {
                return;
            }
            break (end + 4, length);
        }
    };
    while request.len() < headers_end + content_length {
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
    }
    let headers = String::from_utf8_lossy(&request[..headers_end]);
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let bearer = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| value.trim().strip_prefix("Bearer "));
    let form = url::form_urlencoded::parse(&request[headers_end..headers_end + content_length])
        .into_owned()
        .collect();
    let (status, body) = identity.respond(path, &form, bearer, routing_host, routing_port);
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn routing_token(routing_host: &str, routing_port: u16, subject: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(E2E_JWT_KID.to_string());
    let claims = RoutingClaims {
        sub: subject,
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
            captures: HashMap::new(),
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

        for (name, value) in &self.captures {
            let var = format!("${name}");
            result = result.replace(&var, value);
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
        let temp_dir = tempfile::Builder::new()
            .prefix("ae")
            .tempdir_in(if cfg!(unix) {
                PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;

        // Prepare environment by auto-injecting missing fields
        let (directories, mut configs, terminals) =
            self.prepare_environment(test_case, temp_dir.path())?;
        let cloud_fixture = configure_cloud_fixture(&mut configs)?;

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
        let mut config_envs: HashMap<String, HashMap<String, String>> = HashMap::new();

        for (index, cfg) in configs.iter().enumerate() {
            // Determine socket path
            let socket_path = match &cfg.socket_path {
                Some(p) if p != "auto" => PathBuf::from(p),
                _ => default_socket_path(temp_dir.path(), &test_case.name, &cfg.name),
            };

            // Clean up any existing socket
            #[cfg(unix)]
            let _ = std::fs::remove_file(&socket_path);

            let tcp_port = match cfg.tcp_port {
                Some(0) => Some(allocate_local_port()?),
                Some(port) => Some(port),
                None => None,
            };

            let checkout = temp_dir.path().join(format!("c{index}"));
            let root = if cfg.worktree {
                checkout.join(".wt/amux")
            } else {
                checkout.clone()
            };
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            let id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("amux-e2e:{}:{}", test_case.name, cfg.name).as_bytes(),
            )
            .to_string();
            let profile_dir = root.join("profiles").join(&id);
            let state_dir = if cfg.cloud_relay {
                root.join("state")
            } else {
                profile_dir.join("state")
            };
            if cfg.cloud_relay {
                std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
            }
            let state_path = state_dir.join("state.yaml");
            let data_home = root.join("data_home");
            let xdg_state_home = root.join("xdg_state_home");
            let log_path = root.join("amux.log");
            let host_name = cfg
                .host_name
                .clone()
                .unwrap_or_else(|| test_case.name.clone());
            let cloud_url = cfg.cloud_url.as_deref().unwrap_or("https://amux.sh");
            let (config_file_path, socket_path) = if cfg.cloud_relay {
                let path = root.join("relay.yaml");
                write_fixture_yaml(
                    &path,
                    &serde_json::json!({
                        "host_name": host_name, "socket_path": socket_path,
                        "state_path": state_path, "data_dir": root.join("data"),
                        "tcp_port": tcp_port, "cloud_url": cloud_url, "prevent_idle_sleep": false,
                    }),
                )?;
                (path, socket_path)
            } else {
                let installation_path = root.join("installation.yaml");
                let profile_names = if cfg.profiles.is_empty() {
                    vec![cfg.name.clone()]
                } else {
                    cfg.profiles.clone()
                };
                write_fixture_yaml(
                    &installation_path,
                    &serde_json::json!({
                        "root": root, "front_door_socket": root.join("amux.sock"),
                        "host_name": host_name, "prevent_idle_sleep": false,
                        "keymaps_dir": root.join("keymaps"),
                    }),
                )?;
                let (profile_path, socket) = if cfg.worktree {
                    // Use the actual worktree template and generator so this fixture catches
                    // changes in either half of the developer layout.
                    let source = std::fs::read_to_string(crate::workspace_root().join(".wt.toml"))
                        .map_err(|e| e.to_string())?;
                    let wt: toml::Value = toml::from_str(&source).map_err(|e| e.to_string())?;
                    let template = wt["files"][".wt/amux/installation.yaml"]["content"]
                        .as_str()
                        .ok_or("missing worktree installation template")?;
                    std::fs::write(
                        &installation_path,
                        template
                            .replace("{{root()}}", &checkout.to_string_lossy())
                            .replace("{{name_short()}}", &cfg.name),
                    )
                    .map_err(|e| e.to_string())?;
                    let generated = Command::new("python3")
                        .arg(crate::workspace_root().join("scripts/worktree-profile.py"))
                        .arg(&root)
                        .arg(&cfg.name)
                        .output()
                        .map_err(|e| e.to_string())?;
                    if !generated.status.success() {
                        return Err(String::from_utf8_lossy(&generated.stderr).into_owned());
                    }
                    let path = PathBuf::from(String::from_utf8_lossy(&generated.stdout).trim());
                    let profile_id = path
                        .parent()
                        .and_then(Path::file_name)
                        .ok_or("missing generated profile id")?
                        .to_string_lossy()
                        .to_string();
                    var_ctx
                        .captures
                        .insert(format!("{}.id", cfg.name), profile_id.clone());
                    (
                        root.join("profile.yaml"),
                        root.join("profiles").join(format!("{profile_id}.sock")),
                    )
                } else {
                    let mut records = Vec::new();
                    let mut first = None;
                    for (profile_index, name) in profile_names.iter().enumerate() {
                        let profile_id = if profile_index == 0 {
                            id.clone()
                        } else {
                            uuid::Uuid::new_v5(
                                &uuid::Uuid::NAMESPACE_URL,
                                format!("amux-e2e:{}:{}:{name}", test_case.name, cfg.name)
                                    .as_bytes(),
                            )
                            .to_string()
                        };
                        let dir = root.join("profiles").join(&profile_id);
                        std::fs::create_dir_all(dir.join("state")).map_err(|e| e.to_string())?;
                        let path = dir.join("config.yaml");
                        let socket = root.join("profiles").join(format!("{profile_id}.sock"));
                        write_fixture_yaml(
                            &path,
                            &serde_json::json!({
                                "installation_config": installation_path, "socket_path": socket,
                                "state_path": dir.join("state/state.yaml"), "data_dir": dir.join("data"),
                                "cloud_url": cloud_url, "tcp_port": if profile_index == 0 { tcp_port } else { None },
                            }),
                        )?;
                        records.push(serde_json::json!({"id": profile_id, "label": {"override_name": name}, "binding": null, "paused": false, "revision": 1}));
                        var_ctx
                            .captures
                            .insert(format!("{}.{name}.id", cfg.name), profile_id.clone());
                        var_ctx.captures.insert(
                            format!("{}.{name}.socket_path", cfg.name),
                            socket.display().to_string(),
                        );
                        if first.is_none() {
                            var_ctx
                                .captures
                                .insert(format!("{}.id", cfg.name), profile_id);
                            first = Some((path, socket));
                        }
                    }
                    write_fixture_yaml(
                        &root.join("registry.yaml"),
                        &serde_json::json!({"profiles": records}),
                    )?;
                    first.ok_or("at least one profile required")?
                };
                var_ctx.captures.insert(
                    format!("{}.front_door", cfg.name),
                    root.join("amux.sock").display().to_string(),
                );
                (profile_path, socket)
            };

            let config_home = root.join("config_home");
            if !cfg.cloud_relay {
                std::fs::create_dir_all(config_home.join("amux")).map_err(|e| e.to_string())?;
                std::fs::copy(
                    root.join("installation.yaml"),
                    config_home.join("amux/config.yaml"),
                )
                .map_err(|e| e.to_string())?;
            }
            config_paths.insert(cfg.name.clone(), config_file_path);
            let mut env = HashMap::from([
                (
                    "XDG_CONFIG_HOME".to_string(),
                    config_home.display().to_string(),
                ),
                (
                    "XDG_DATA_HOME".to_string(),
                    data_home.to_string_lossy().to_string(),
                ),
                (
                    "XDG_STATE_HOME".to_string(),
                    xdg_state_home.to_string_lossy().to_string(),
                ),
                (
                    "AMUX_LOG".to_string(),
                    log_path.to_string_lossy().to_string(),
                ),
            ]);
            if let Some(fixture) = &cloud_fixture {
                env.insert(
                    "AMUX_CLOUD_TLS_CA".to_string(),
                    fixture.routing_tls_ca.to_string_lossy().to_string(),
                );
                if cfg.cloud_relay {
                    env.insert(
                        "AMUX_TLS_CERT".to_string(),
                        fixture.routing_tls_cert.to_string_lossy().to_string(),
                    );
                    env.insert(
                        "AMUX_TLS_KEY".to_string(),
                        fixture.routing_tls_key.to_string_lossy().to_string(),
                    );
                }
            }
            config_envs.insert(cfg.name.clone(), env);
            var_ctx.configs.insert(cfg.name.clone(), socket_path);
            if let Some(tcp_port) = tcp_port {
                var_ctx.tcp_ports.insert(cfg.name.clone(), tcp_port);
            }
        }

        let mut transcript = String::new();
        let initialized = self.initialize_device_configs(
            &configs,
            &config_paths,
            &config_envs,
            cloud_fixture.as_ref(),
            &mut transcript,
        );

        // Map terminal names to their config and cwd
        let terminal_configs: HashMap<String, (String, PathBuf, bool)> = terminals
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
                (t.name.clone(), (config_name, cwd, t.installation))
            })
            .collect();

        // Execute test steps, then clean up servers regardless of result
        let result = initialized
            .and_then(|()| {
                self.execute_steps(
                    &test_case.steps,
                    &terminal_configs,
                    &config_paths,
                    &config_envs,
                    &mut var_ctx,
                    &mut transcript,
                )
            })
            .map_err(|error| append_config_logs(error, &config_envs));

        // Cleanup: shut down background servers spawned during the test.
        for cfg in configs.iter().filter(|cfg| !cfg.cloud_relay) {
            let _ = Command::new(&self.config.amux_binary)
                .args(["server", "stop"])
                .env_remove("AMUX_CONFIG")
                .envs(&config_envs[&cfg.name])
                .output();
        }
        #[cfg(unix)]
        for socket_path in var_ctx.configs.values() {
            let _ = std::fs::remove_file(socket_path);
        }

        if let Some(directory) = &self.config.transcript_dir {
            std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
            transcript.push_str(&format!(
                "\nResult: {}\n",
                if result.is_ok() { "PASS" } else { "FAIL" }
            ));
            std::fs::write(
                directory.join(format!("{}.txt", test_case.name)),
                transcript,
            )
            .map_err(|e| e.to_string())?;
        }
        result
    }

    fn initialize_device_configs(
        &self,
        configs: &[TestConfig],
        config_paths: &HashMap<String, PathBuf>,
        config_envs: &HashMap<String, HashMap<String, String>>,
        fixture: Option<&CloudFixture>,
        transcript: &mut String,
    ) -> Result<(), String> {
        for cfg in configs.iter().filter(|cfg| !cfg.cloud_relay) {
            let config_path = config_paths
                .get(&cfg.name)
                .ok_or_else(|| format!("Unknown config: {}", cfg.name))?;
            let env = config_envs
                .get(&cfg.name)
                .ok_or_else(|| format!("Missing env for config: {}", cfg.name))?;
            let mut commands = vec![vec!["init".to_string()]];
            if let Some(account) = &cfg.cloud_account {
                fixture
                    .ok_or("login requires cloud fixture")?
                    .identity
                    .lock()
                    .unwrap()
                    .accounts
                    .push_front(fixture_account(account)?);
                commands.push(vec!["login".to_string()]);
            }
            for args in commands {
                let command = ResolvedCommand {
                    program: self.config.amux_binary.display().to_string(),
                    args: [
                        vec!["--config".into(), config_path.display().to_string()],
                        args.clone(),
                    ]
                    .concat(),
                };
                let output = run_oneshot_command(&command, &crate::workspace_root(), env)?;
                transcript.push_str(&format!(
                    "[setup {}] amux {}\n{output}",
                    cfg.name,
                    args.join(" ")
                ));
            }
        }
        Ok(())
    }

    fn execute_steps(
        &self,
        steps: &[TestStep],
        terminal_configs: &HashMap<String, (String, PathBuf, bool)>,
        config_paths: &HashMap<String, PathBuf>,
        config_envs: &HashMap<String, HashMap<String, String>>,
        var_ctx: &mut VariableContext,
        transcript: &mut String,
    ) -> Result<(), String> {
        let mut active_terminals: HashMap<String, TestTerminal> = HashMap::new();
        let mut oneshot_outputs: HashMap<String, String> = HashMap::new();
        let mut last_oneshot_commands: HashMap<
            String,
            (ResolvedCommand, PathBuf, HashMap<String, String>),
        > = HashMap::new();
        let mut retry_next_expect: Option<RetryPolicy> = None;
        let mut current_terminal: Option<String> = None;

        for step in steps {
            match step {
                TestStep::ProcessExited(pid) => {
                    let pid: i32 = var_ctx
                        .substitute(pid)
                        .parse()
                        .map_err(|_| "invalid captured process id")?;
                    if pid <= 1 {
                        return Err("refusing invalid process id".into());
                    }
                    #[cfg(unix)]
                    {
                        let start = Instant::now();
                        loop {
                            // SAFETY: signal 0 only probes whether this process exists.
                            if unsafe { libc::kill(pid, 0) } == -1 {
                                let error = std::io::Error::last_os_error();
                                if error.raw_os_error() == Some(libc::ESRCH) {
                                    break;
                                }
                                return Err(format!("Cannot probe process {pid}: {error}"));
                            }
                            if start.elapsed() > Duration::from_secs(5) {
                                return Err(format!("Agent process {pid} is still alive"));
                            }
                            thread::sleep(Duration::from_millis(20));
                        }
                        transcript.push_str(&format!("[agent process {pid} exited]\n"));
                    }
                    #[cfg(not(unix))]
                    return Err(format!(
                        "Process probe unsupported for {pid} on this platform"
                    ));
                }
                TestStep::ExpectContains(expected) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let output = oneshot_outputs
                        .get(term_name)
                        .ok_or("@@contains requires a completed one-shot command")?;
                    let expected = var_ctx.substitute(expected);
                    if !output.contains(&expected) {
                        return Err(format!("Expected {expected:?} in output:\n{output}"));
                    }
                }
                TestStep::Exit(code) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let mut terminal = active_terminals
                        .remove(term_name)
                        .ok_or("@@exit requires a PTY command")?;
                    terminal
                        .wait_exit(*code, Duration::from_secs(5))
                        .map_err(|e| e.to_string())?;
                    transcript.push_str(&format!("[exit {code}]\n"));
                }
                TestStep::SwitchTerminal(name) => {
                    transcript.push_str(&format!("\n@{name}\n"));
                    current_terminal = Some(name.clone());
                }
                TestStep::Sleep(ms) => {
                    std::thread::sleep(Duration::from_millis(*ms));
                }
                TestStep::RetryNextExpect(policy) => {
                    retry_next_expect = Some(*policy);
                }
                TestStep::CaptureOutput { name, prefix } => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let prefix_substituted = var_ctx.substitute(prefix);

                    let actual = if let Some(output) = oneshot_outputs.remove(term_name) {
                        let Some(line_end) = output.find('\n') else {
                            return Err(format!(
                                "Capture in terminal {term_name} expected one output line, got {output:?}"
                            ));
                        };
                        let line = output[..line_end].to_string();
                        let remainder = output[line_end + 1..].to_string();
                        if !remainder.is_empty() {
                            oneshot_outputs.insert(term_name.clone(), remainder);
                        }
                        line
                    } else {
                        let terminal = active_terminals
                            .get_mut(term_name)
                            .ok_or(format!("Terminal {} not initialized", term_name))?;
                        terminal
                            .read_line(self.config.timeout)
                            .map_err(|e| format!("Failed to capture output: {}", e))?
                    };

                    if active_terminals.contains_key(term_name) {
                        transcript.push_str(&format!("{actual}\n"));
                    }
                    if !actual.starts_with(&prefix_substituted) {
                        return Err(format!(
                            "Capture mismatch in terminal {}:\n  Expected prefix: {:?}\n  Actual:          {:?}",
                            term_name, prefix_substituted, actual
                        ));
                    }
                    let value = actual[prefix_substituted.len()..].trim().to_string();
                    if value.is_empty() {
                        return Err(format!(
                            "Capture {name} in terminal {term_name} produced an empty value"
                        ));
                    }
                    var_ctx.captures.insert(name.clone(), value);
                }
                TestStep::Input(input) => {
                    let term_name = current_terminal.as_ref().ok_or("No terminal selected")?;
                    let (config_name, cwd, installation) = terminal_configs
                        .get(term_name)
                        .ok_or(format!("Unknown terminal: {}", term_name))?;
                    let config_path = config_paths
                        .get(config_name)
                        .ok_or(format!("Unknown config: {}", config_name))?;
                    let env = config_envs
                        .get(config_name)
                        .ok_or(format!("Missing env for config: {}", config_name))?;

                    let input_substituted = var_ctx.substitute(input);
                    transcript.push_str(&format!("\n[{term_name}] > {input_substituted}\n"));
                    let is_amux_command = input_substituted == "amux"
                        || input_substituted.starts_with("amux ")
                        || input_substituted.starts_with("e2e-runner client ");

                    if is_amux_command && !active_terminals.contains_key(term_name) {
                        oneshot_outputs.remove(term_name);
                        last_oneshot_commands.remove(term_name);
                        let transformed = self.transform_command(
                            &input_substituted,
                            (!installation).then_some(config_path.as_path()),
                        )?;

                        if is_oneshot_amux_command(&transformed) {
                            let combined = run_oneshot_command(&transformed, cwd, env)?;
                            transcript.push_str(&combined);
                            last_oneshot_commands
                                .insert(term_name.clone(), (transformed, cwd.clone(), env.clone()));
                            oneshot_outputs.insert(term_name.clone(), combined);
                        } else {
                            let terminal = TestTerminal::spawn(
                                &transformed.program,
                                &transformed.args,
                                cwd,
                                env,
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
                            let (command, cwd, env) = last_oneshot_commands.get(term_name).ok_or(
                                "Retry directive requires a previous one-shot amux command",
                            )?;
                            retry_oneshot_until_expected(
                                command,
                                cwd,
                                env,
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

                    if active_terminals.contains_key(term_name) {
                        transcript.push_str(&actual);
                    } else if retry_policy.is_some() && !transcript.ends_with(&actual) {
                        transcript.push_str("[retry result]\n");
                        transcript.push_str(&actual);
                    }
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
                cloud_account: None,
                accounts: Vec::new(),
                profiles: Vec::new(),
                worktree: false,
                cloud_url: None,
                tcp_port: None,
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
        config_path: Option<&Path>,
    ) -> Result<ResolvedCommand, String> {
        let mut parts =
            shell_words::split(input).map_err(|e| format!("Failed to parse command: {}", e))?;
        if parts.is_empty() {
            return Err("Command cannot be empty".to_string());
        }

        if parts[0] == "e2e-runner" {
            parts[0] = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .display()
                .to_string();
        }
        if parts[0] == "amux" {
            parts[0] = self.config.amux_binary.display().to_string();
            if let Some(config_path) = config_path {
                parts.insert(1, "--config".to_string());
                parts.insert(2, config_path.display().to_string());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_identity_device_flow_rotates_single_use_tokens_and_separates_accounts() {
        let mut state = IdentityState::new(&["alice".into(), "bob".into()]).unwrap();
        fn call(
            state: &mut IdentityState,
            path: &str,
            form: HashMap<String, String>,
            bearer: Option<&str>,
        ) -> (&'static str, serde_json::Value) {
            state.respond(path, &form, bearer, "relay", 1234)
        }
        assert_eq!(
            call(
                &mut state,
                "/connect/deviceauthorization",
                HashMap::new(),
                None
            )
            .0,
            "400 Bad Request"
        );
        for name in ["alice", "bob"] {
            let (status, device) = call(
                &mut state,
                "/connect/deviceauthorization",
                HashMap::from([(
                    "scope".into(),
                    "openid profile email offline_access api".into(),
                )]),
                None,
            );
            assert_eq!(status, "200 OK");
            let grant = HashMap::from([
                (
                    "grant_type".into(),
                    "urn:ietf:params:oauth:grant-type:device_code".into(),
                ),
                (
                    "device_code".into(),
                    device["device_code"].as_str().unwrap().into(),
                ),
            ]);
            let (status, tokens) = call(&mut state, "/connect/token", grant.clone(), None);
            assert_eq!(status, "200 OK");
            assert_eq!(
                call(&mut state, "/connect/token", grant, None).1["error"],
                "invalid_grant"
            );
            let grant = HashMap::from([
                ("grant_type".into(), "refresh_token".into()),
                (
                    "refresh_token".into(),
                    tokens["refresh_token"].as_str().unwrap().into(),
                ),
            ]);
            let (status, rotated) = call(&mut state, "/connect/token", grant.clone(), None);
            assert_eq!(status, "200 OK");
            assert_ne!(tokens["refresh_token"], rotated["refresh_token"]);
            assert_eq!(
                call(&mut state, "/connect/token", grant, None).1["error"],
                "invalid_grant"
            );
            let (status, info) = call(
                &mut state,
                "/connect/userinfo",
                HashMap::new(),
                rotated["access_token"].as_str(),
            );
            assert_eq!(status, "200 OK");
            let account = fixture_account(name).unwrap();
            assert_eq!(info["sub"], account.sub);
            assert_eq!(info["name"], account.display);
            assert_eq!(info["email"], account.email);
        }
        assert_eq!(
            call(
                &mut state,
                "/connect/userinfo",
                HashMap::new(),
                Some("invalid")
            )
            .0,
            "401 Unauthorized"
        );
    }
}
