#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use node::installation::{InstallationRoot, ProfileId, ProfileLabel, ProfilePaths, Registry};
use node::{InstallationConfig, ProfileConfig};
use pty_host::{PtyProcess, PtySize, PtySpawn};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);

struct Fixture {
    _temp: tempfile::TempDir,
    installation: InstallationConfig,
    profiles: BTreeMap<String, PathBuf>,
    agent: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new(labels: &[&str]) -> Self {
        Self::named("process-host", labels)
    }

    fn named(host_name: &str, labels: &[&str]) -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ap")
            .tempdir_in("/tmp")
            .expect("create short process-test root");
        let root = temp.path().canonicalize().unwrap();
        let installation = InstallationConfig {
            root: root.clone(),
            front_door_socket: root.join("amux.sock"),
            host_name: host_name.into(),
            prevent_idle_sleep: Some(false),
            keymaps_dir: root.join("keymaps"),
            path: Some(root.join("installation.yaml")),
            ..InstallationConfig::default()
        };
        write_yaml(installation.path.as_ref().unwrap(), &installation);

        let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone())).unwrap();
        let mut profiles = BTreeMap::new();
        for label in labels {
            let id = ProfileId(uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("amux-process-test:{host_name}:{label}").as_bytes(),
            ));
            registry
                .create(
                    id,
                    ProfileLabel {
                        override_name: Some((*label).to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
            let paths = ProfilePaths::for_id(&root, id).unwrap();
            let profile = paths.config_path.unwrap();
            write_yaml(
                &profile,
                &ProfileConfig {
                    installation_config: installation.path.clone().unwrap(),
                    socket_path: paths.socket_path,
                    data_dir: paths.data_dir,
                    state_path: paths.state_path,
                    cloud_url: "https://amux.invalid".into(),
                    cloud_refresh_secs: None,
                    lan: Default::default(),
                },
            );
            profiles.insert((*label).to_string(), profile);
        }

        let agent = root.join("test-agent");
        std::fs::write(
            &agent,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  if [ \"$line\" = pid ]; then echo \"pid: $$\"; elif [ \"$line\" = exit ]; then echo \"echo: exit\"; exit 0; else echo \"echo: $line\"; fi\ndone\n",
        )
        .unwrap();
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();

        Self {
            _temp: temp,
            installation,
            profiles,
            agent,
            binary: PathBuf::from(env!("CARGO_BIN_EXE_amux")),
        }
    }

    fn set_lan_port(&self, label: &str, port: u16) {
        let path = self.profile(label);
        let mut profile = ProfileConfig::from_file(path).unwrap();
        profile.lan.port = port;
        write_yaml(path, &profile);
    }

    fn set_cloud_url(&self, url: &str) {
        for path in self.profiles.values() {
            let mut profile = ProfileConfig::from_file(path).unwrap();
            profile.cloud_url = url.to_string();
            profile.cloud_refresh_secs = Some(1);
            write_yaml(path, &profile);
        }
    }

    fn set_update_manifest_url(&mut self, url: String) {
        self.installation.update_manifest_url = url;
        write_yaml(self.installation.path.as_ref().unwrap(), &self.installation);
    }

    fn use_binary_copy(&mut self) {
        let copy = self.installation.root.join("amux-process-binary");
        std::fs::copy(env!("CARGO_BIN_EXE_amux"), &copy).unwrap();
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o700)).unwrap();
        self.binary = copy;
    }

    fn profile(&self, label: &str) -> &Path {
        self.profiles.get(label).unwrap()
    }

    fn command(&self, label: &str, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .env("AMUX_CONFIG", self.profile(label))
            .env("AMUX_LOG", self.installation.root.join("daemon.log"))
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled");
        command
    }

    fn run(&self, label: &str, args: &[&str]) -> Output {
        let output = self.command(label, args).output().unwrap();
        println!(
            "$ amux {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{output:?}");
        output
    }

    fn terminal(&self, label: &str, args: &[String], cwd: &Path) -> Terminal {
        let env = vec![
            (OsString::from("AMUX_CONFIG"), self.profile(label).into()),
            (
                OsString::from("AMUX_LOG"),
                self.installation.root.join("daemon.log").into_os_string(),
            ),
            (
                OsString::from("AMUX_TEST_DISCOVERY_MODE"),
                OsString::from("disabled"),
            ),
        ];
        Terminal::spawn(PtySpawn {
            command: self.binary.clone(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
            env,
            env_remove: vec![OsString::from("NO_COLOR")],
            size: PtySize {
                rows: 40,
                cols: 120,
            },
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(label) = self.profiles.keys().next() {
            let _ = self.command(label, &["server", "stop"]).output();
        }
    }
}

struct Terminal {
    process: PtyProcess,
    output: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    seen: Vec<u8>,
}

impl Terminal {
    fn spawn(spec: PtySpawn) -> Self {
        let process = pty_host::spawn(spec).unwrap();
        let output = process.handle.output();
        Self {
            process,
            output,
            seen: Vec::new(),
        }
    }

    /// Bounded like every read in this harness: a terminal that stops
    /// draining its input must fail with what it had seen, not wedge the
    /// whole suite until the recipe's own deadline fires.
    async fn write(&self, input: &[u8]) {
        tokio::time::timeout(PROCESS_TIMEOUT, self.process.handle.write(input))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out writing {} bytes to the terminal; output:\n{}",
                    input.len(),
                    String::from_utf8_lossy(&self.seen)
                )
            })
            .unwrap();
    }

    async fn line(&self, input: &str) {
        self.write(format!("{input}\n").as_bytes()).await;
    }

    async fn wait_for(&mut self, needle: &str) -> String {
        let needle = needle.as_bytes();
        tokio::time::timeout(PROCESS_TIMEOUT, async {
            while !self
                .seen
                .windows(needle.len())
                .any(|window| window == needle)
            {
                let chunk = self.output.recv().await.unwrap_or_else(|| {
                    panic!(
                        "terminal exited before {:?}; output:\n{}",
                        String::from_utf8_lossy(needle),
                        String::from_utf8_lossy(&self.seen)
                    )
                });
                self.seen.extend_from_slice(&chunk);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; output:\n{}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.seen)
            )
        });
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    async fn exit(&mut self) -> pty_host::ExitStatus {
        tokio::time::timeout(PROCESS_TIMEOUT, self.process.exit.wait())
            .await
            .expect("terminal process did not exit")
    }
}

fn write_yaml(path: &Path, value: &impl serde::Serialize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_yaml::to_string(value).unwrap()).unwrap();
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn pid_from(output: &str) -> i32 {
    output
        .split("pid:")
        .nth(1)
        .and_then(|tail| {
            tail.trim_start()
                .split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .unwrap()
        .parse()
        .unwrap()
}

async fn assert_process_exited(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async move {
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("child process {pid} was signalled but not reaped"));
}

fn available_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn pair_direct(source: &Fixture, destination: &Fixture) {
    let port = available_port();
    source.set_lan_port("local", port);
    source.run("local", &["server", "start"]);
    destination.run("local", &["server", "start"]);
    source.run(
        "local",
        &["pair", "--demo", "--pin", "654321", "--for", "5m"],
    );
    let address = format!("127.0.0.1:{port}");
    let mut pair = destination
        .command("local", &["pair", &address])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    pair.stdin.take().unwrap().write_all(b"654321\n").unwrap();
    let output = pair.wait_with_output().unwrap();
    println!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{output:?}");
}

async fn wait_for_listing(fixture: &Fixture, expected: &str) -> String {
    tokio::time::timeout(PROCESS_TIMEOUT, async {
        loop {
            let output = fixture.command("local", &["list"]).output().unwrap();
            let output = text(&output);
            if output.contains(expected) {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("remote listing never contained {expected:?}"))
}

struct RelayProcess {
    child: Child,
    _temp: tempfile::TempDir,
}

impl RelayProcess {
    fn start(account: &node_test_support::ProcessAccountFixture, port: u16) -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ar")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = root.join("relay.yaml");
        write_yaml(
            &config,
            &serde_json::json!({
                "host_name": "relay",
                "socket_path": root.join("relay.sock"),
                "state_path": root.join("state.yaml"),
                "data_dir": root.join("data"),
                "tcp_port": port,
                "udp_port": port,
                "cloud_url": account.url(),
                "prevent_idle_sleep": false,
            }),
        );
        let child = Command::new(env!("CARGO_BIN_EXE_amux"))
            .args([
                "--config",
                config.to_str().unwrap(),
                "server",
                "start",
                "--cloud",
                "--foreground",
            ])
            .env("AMUX_TLS_CERT", account.tls_cert())
            .env("AMUX_TLS_KEY", account.tls_key())
            .env("AMUX_LOG", root.join("relay.log"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                Instant::now() < deadline,
                "relay did not listen on port {port}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, _temp: temp }
    }
}

impl Drop for RelayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn account(sub: &str, name: &str, email: &str) -> node_test_support::TestAccount {
    node_test_support::TestAccount {
        sub: sub.into(),
        name: Some(name.into()),
        email: Some(email.into()),
        tier: node_test_support::Tier::Free,
    }
}

#[test]
fn bare_help_prints_cli_and_pair_help_without_a_tty() {
    for args in [vec![], vec!["pair", "--help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_amux"))
            .args(&args)
            .env_remove("AMUX_CONFIG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let output = text(&output);
        if args.is_empty() {
            assert!(
                output.contains("Terminal multiplexer for AI agents"),
                "unexpected help output:\n{output}"
            );
        } else {
            assert!(output.contains("Pair this device with another amux daemon"));
            assert!(output.contains("--qr-payload <LINK>"));
        }
    }
}

#[test]
fn split_installation_starts_stops_and_lists_profiles() {
    let fixture = Fixture::new(&["personal", "work"]);
    let started = text(&fixture.run("personal", &["server", "start"]));
    assert!(started.contains("Server started."));
    let profiles = text(&fixture.run("personal", &["profiles"]));
    assert!(profiles.contains("personal"));
    assert!(profiles.contains("work"));
    assert!(text(&fixture.run("personal", &["server", "stop"])).contains("Server shutting down."));
    assert!(text(&fixture.run("personal", &["server", "start"])).contains("Server started."));
}

#[test]
fn server_start_is_idempotent_and_stop_releases_daemon() {
    let fixture = Fixture::new(&["local"]);
    assert!(text(&fixture.run("local", &["server", "start"])).contains("Server started."));
    assert!(text(&fixture.run("local", &["server", "start"])).contains("Server already running."));
    assert!(text(&fixture.run("local", &["server", "stop"])).contains("Server shutting down."));
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.installation.front_door_socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!fixture.installation.front_door_socket.exists());
}

#[tokio::test]
async fn new_agent_round_trips_terminal_input() {
    let fixture = Fixture::new(&["local"]);
    fixture.run("local", &["server", "start"]);
    let project = fixture.installation.root.join("round-trip");
    std::fs::create_dir(&project).unwrap();
    let mut terminal = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "round-trip".into(),
        ],
        &project,
    );
    terminal.line("hello process test").await;
    terminal.wait_for("echo: hello process test").await;
    terminal.line("exit").await;
    terminal.wait_for("[session ended]").await;
    assert!(terminal.exit().await.success());
}

#[tokio::test]
async fn local_attach_reports_agent_exit() {
    let fixture = Fixture::new(&["local"]);
    fixture.run("local", &["server", "start"]);
    let mut terminal = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "ends".into(),
        ],
        &fixture.installation.root,
    );
    terminal.line("pid").await;
    let pid = pid_from(&terminal.wait_for("pid:").await);
    terminal.line("exit").await;
    terminal.wait_for("[session ended]").await;
    assert!(terminal.exit().await.success());
    assert_process_exited(pid).await;
}

#[tokio::test]
async fn two_local_agents_remain_independently_interactive() {
    let fixture = Fixture::new(&["local"]);
    fixture.run("local", &["server", "start"]);
    let mut alpha = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "alpha".into(),
        ],
        &fixture.installation.root,
    );
    let mut beta = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "beta".into(),
        ],
        &fixture.installation.root,
    );
    alpha.line("alpha still alive").await;
    beta.line("beta still alive").await;
    alpha.wait_for("echo: alpha still alive").await;
    beta.wait_for("echo: beta still alive").await;
    alpha.line("exit").await;
    beta.line("exit").await;
    alpha.wait_for("[session ended]").await;
    beta.wait_for("[session ended]").await;
    assert!(alpha.exit().await.success());
    assert!(beta.exit().await.success());
}

#[tokio::test]
async fn list_prints_local_agents_and_working_directories() {
    let fixture = Fixture::new(&["local"]);
    fixture.run("local", &["server", "start"]);
    let alpha_dir = fixture.installation.root.join("alpha-dir");
    let beta_dir = fixture.installation.root.join("beta-dir");
    std::fs::create_dir(&alpha_dir).unwrap();
    std::fs::create_dir(&beta_dir).unwrap();
    let mut alpha = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "alpha".into(),
        ],
        &alpha_dir,
    );
    let mut beta = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "beta".into(),
        ],
        &beta_dir,
    );
    alpha.line("ready-alpha").await;
    beta.line("ready-beta").await;
    alpha.wait_for("echo: ready-alpha").await;
    beta.wait_for("echo: ready-beta").await;

    let listing = text(&fixture.run("local", &["list"]));
    assert!(listing.contains(&format!("alpha [test-agent] - {}", alpha_dir.display())));
    assert!(listing.contains(&format!("beta [test-agent] - {}", beta_dir.display())));
    alpha.line("exit").await;
    beta.line("exit").await;
    alpha.wait_for("[session ended]").await;
    beta.wait_for("[session ended]").await;
    assert!(alpha.exit().await.success());
    assert!(beta.exit().await.success());
}

/// Two processes have to make progress at once here: this terminal keeps
/// writing while the agent echoes back, and the echo is what unblocks the
/// write. On one worker a filled pipe can park the writer before the reader
/// is ever polled, which is a deadlock in the harness rather than the
/// backpressure this is about.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_pipe_backpressure_does_not_lose_input() {
    let fixture = Fixture::new(&["local"]);
    fixture.run("local", &["server", "start"]);
    let mut terminal = fixture.terminal(
        "local",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "backpressure".into(),
        ],
        &fixture.installation.root,
    );
    terminal.line("ready").await;
    terminal.wait_for("echo: ready").await;
    let handle = terminal.process.handle.clone();
    let mut input = Vec::new();
    for _ in 0..512 {
        input.extend_from_slice(&[b'x'; 256]);
        input.push(b'\n');
    }
    input.extend_from_slice(b"backpressure-finished\n");
    let write = tokio::spawn(async move { handle.write(&input).await });
    terminal.wait_for("backpressure-finished").await;
    write.await.unwrap().unwrap();
    terminal.line("exit").await;
    terminal.wait_for("[session ended]").await;
    assert!(terminal.exit().await.success());
}

#[tokio::test]
async fn server_stop_kills_agents_in_every_profile() {
    let fixture = Fixture::new(&["personal", "work"]);
    fixture.run("personal", &["server", "start"]);
    let mut personal = fixture.terminal(
        "personal",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "personal-agent".into(),
        ],
        &fixture.installation.root,
    );
    let mut work = fixture.terminal(
        "work",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "work-agent".into(),
        ],
        &fixture.installation.root,
    );
    personal.line("pid").await;
    work.line("pid").await;
    let personal_pid = pid_from(&personal.wait_for("pid:").await);
    let work_pid = pid_from(&work.wait_for("pid:").await);

    fixture.run("personal", &["server", "stop"]);
    personal.wait_for("[server shutting down]").await;
    work.wait_for("[server shutting down]").await;
    assert!(!personal.exit().await.success());
    assert!(!work.exit().await.success());
    assert_process_exited(personal_pid).await;
    assert_process_exited(work_pid).await;

    fixture.run("personal", &["server", "start"]);
    assert!(
        text(&fixture.run("personal", &["--profile", "personal", "list"]))
            .contains("No agents running.")
    );
    assert!(
        text(&fixture.run("personal", &["--profile", "work", "list"]))
            .contains("No agents running.")
    );
}

#[test]
fn worktree_template_starts_and_probes_daemon() {
    let temp = tempfile::Builder::new()
        .prefix("aw")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().canonicalize().unwrap().join("profile-root");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/worktree-profile.py");
    let generated = Command::new("python3")
        .arg(script)
        .arg(&root)
        .arg("example-tree")
        .output()
        .unwrap();
    assert!(generated.status.success(), "{generated:?}");
    let installation = InstallationConfig {
        root: root.clone(),
        front_door_socket: root.join("amux.sock"),
        host_name: "worktree-host".into(),
        prevent_idle_sleep: Some(false),
        keymaps_dir: root.join("keymaps"),
        path: Some(root.join("installation.yaml")),
        ..InstallationConfig::default()
    };
    write_yaml(installation.path.as_ref().unwrap(), &installation);
    let profile = root.join("profile.yaml");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_amux"))
            .args(args)
            .env("AMUX_CONFIG", &profile)
            .env("AMUX_LOG", root.join("daemon.log"))
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .output()
            .unwrap()
    };
    struct Stop<'a>(&'a dyn Fn(&[&str]) -> Output);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = (self.0)(&["server", "stop"]);
        }
    }
    let _stop = Stop(&run);
    let started = run(&["server", "start"]);
    assert!(started.status.success(), "{started:?}");
    let profiles = run(&["profiles"]);
    assert!(profiles.status.success(), "{profiles:?}");
    assert!(text(&profiles).contains("example-tree"));
    let list = run(&["list"]);
    assert!(list.status.success(), "{list:?}");
    assert!(text(&list).contains("No agents running."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_prints_remote_agents_and_working_directories() {
    let source = Fixture::named("host-a", &["local"]);
    let destination = Fixture::named("host-b", &["local"]);
    pair_direct(&source, &destination);
    let project = source.installation.root.join("remote-project");
    std::fs::create_dir(&project).unwrap();
    let mut agent = source.terminal(
        "local",
        &[
            "new".into(),
            source.agent.display().to_string(),
            "--name".into(),
            "remote-agent".into(),
        ],
        &project,
    );
    agent.line("remote-ready").await;
    agent.wait_for("echo: remote-ready").await;
    let listing = wait_for_listing(&destination, "remote-agent").await;
    assert!(listing.contains(&project.display().to_string()));
    agent.line("exit").await;
    agent.wait_for("[session ended]").await;
    assert!(agent.exit().await.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_attach_reports_agent_exit() {
    let source = Fixture::named("host-a", &["local"]);
    let destination = Fixture::named("host-b", &["local"]);
    pair_direct(&source, &destination);
    let mut owner = source.terminal(
        "local",
        &[
            "new".into(),
            source.agent.display().to_string(),
            "--name".into(),
            "remote-ended".into(),
        ],
        &source.installation.root,
    );
    owner.line("hello remote").await;
    owner.wait_for("echo: hello remote").await;
    wait_for_listing(&destination, "remote-ended").await;
    let mut attached = destination.terminal(
        "local",
        &["attach".into(), "remote-ended".into()],
        &destination.installation.root,
    );
    attached.wait_for("echo: hello remote").await;
    owner.line("exit").await;
    owner.wait_for("[session ended]").await;
    attached.wait_for("[session ended]").await;
    assert!(owner.exit().await.success());
    assert!(attached.exit().await.success());
}

#[cfg(unix)]
async fn third_party_connect(socket: PathBuf) -> tonic::transport::Channel {
    use hyper_util::rt::TokioIo;
    use tonic::transport::Endpoint;
    use tower::service_fn;

    Endpoint::from_static("http://localhost")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .connect_with_connector(service_fn(move |_| {
            let socket = socket.clone();
            async move {
                tokio::net::UnixStream::connect(socket)
                    .await
                    .map(TokioIo::new)
            }
        }))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn third_party_client_discovers_profile_sockets_and_agents() {
    let fixture = Fixture::new(&["personal", "work"]);
    fixture.run("personal", &["server", "start"]);
    let project = fixture.installation.root.join("third-party-project");
    std::fs::create_dir(&project).unwrap();
    let mut personal = fixture.terminal(
        "personal",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "personal-agent".into(),
        ],
        &project,
    );
    let mut work = fixture.terminal(
        "work",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "work-agent".into(),
        ],
        &project,
    );
    personal.line("ready-personal").await;
    work.line("ready-work").await;
    personal.wait_for("echo: ready-personal").await;
    work.wait_for("echo: ready-work").await;

    let mut directory = wire::profile_service_client::ProfileServiceClient::new(
        third_party_connect(fixture.installation.front_door_socket.clone()).await,
    );
    let profiles = directory
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    assert_eq!(profiles.len(), 2);
    for profile in profiles {
        let mut client = wire::client_service_client::ClientServiceClient::new(
            third_party_connect(PathBuf::from(&profile.socket_path)).await,
        );
        let agents = client
            .list_agents(wire::ListAgentsRequest {})
            .await
            .unwrap()
            .into_inner()
            .agents;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].working_dir, project.display().to_string());
        let expected_name = format!("{}-agent", profile.label);
        assert_eq!(agents[0].name.as_deref(), Some(expected_name.as_str()));
    }
    personal.line("exit").await;
    work.line("exit").await;
    personal.wait_for("[session ended]").await;
    work.wait_for("[session ended]").await;
    assert!(personal.exit().await.success());
    assert!(work.exit().await.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_profile_logins_print_bound_accounts() {
    let relay_port = available_port();
    let alice = account(
        "11111111-1111-4111-8111-111111111111",
        "Alice Example",
        "alice@example.test",
    );
    let bob = account(
        "22222222-2222-4222-8222-222222222222",
        "Bob Example",
        "bob@example.test",
    );
    let identity = node_test_support::ProcessAccountFixture::start(
        vec![alice.clone(), bob.clone()],
        "relay",
        relay_port,
    )
    .await;
    let _relay = RelayProcess::start(&identity, relay_port);
    let fixture = Fixture::new(&["personal", "work"]);
    fixture.set_cloud_url(&identity.url());
    let personal_id = node::load_profile_config(fixture.profile("personal"))
        .unwrap()
        .profile_id
        .to_string();
    let work_id = node::load_profile_config(fixture.profile("work"))
        .unwrap()
        .profile_id
        .to_string();
    let ca = identity.tls_ca();
    let start = fixture
        .command("personal", &["server", "start"])
        .env("AMUX_CLOUD_TLS_CA", ca)
        .output()
        .unwrap();
    assert!(start.status.success(), "{start:?}");
    fixture.run(
        "personal",
        &["--profile", "personal", "profile", "rename", "--clear"],
    );
    fixture.run(
        "personal",
        &["--profile", "work", "profile", "rename", "--clear"],
    );

    identity.select_account(&alice.sub);
    let alice_login = fixture
        .command("personal", &["--profile", &personal_id, "login"])
        .env("AMUX_CLOUD_TLS_CA", ca)
        .output()
        .unwrap();
    assert!(alice_login.status.success(), "{alice_login:?}");
    assert!(text(&alice_login).contains("Signed in as alice@example.test"));

    identity.select_account(&bob.sub);
    let bob_login = fixture
        .command("personal", &["--profile", &work_id, "login"])
        .env("AMUX_CLOUD_TLS_CA", ca)
        .output()
        .unwrap();
    assert!(bob_login.status.success(), "{bob_login:?}");
    assert!(text(&bob_login).contains("Signed in as bob@example.test"));

    let profiles = tokio::time::timeout(PROCESS_TIMEOUT, async {
        loop {
            let output = fixture
                .command("personal", &["profiles"])
                .env("AMUX_CLOUD_TLS_CA", ca)
                .output()
                .unwrap();
            let output = text(&output);
            if output.matches("bound / connected (free, quic)").count() == 2 {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("both profiles did not connect to the relay");
    for expected in [
        "Alice Example",
        "alice@example.test",
        "Bob Example",
        "bob@example.test",
    ] {
        assert!(
            profiles.contains(expected),
            "profile output omitted {expected}:\n{profiles}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_replaces_binary_and_resumes_all_profiles() {
    let mut fixture = Fixture::new(&["personal", "work"]);
    fixture.use_binary_copy();
    let identity = node_test_support::ProcessAccountFixture::start_with_update(
        vec![account(
            "11111111-1111-4111-8111-111111111111",
            "Alice Example",
            "alice@example.test",
        )],
        "unused-relay",
        1,
        Some(("999.0.0", &fixture.binary)),
    )
    .await;
    fixture.set_update_manifest_url(format!("{}/update/manifest.json", identity.url()));
    fixture.run("personal", &["server", "start"]);
    let mut personal = fixture.terminal(
        "personal",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "personal-agent".into(),
        ],
        &fixture.installation.root,
    );
    let mut work = fixture.terminal(
        "work",
        &[
            "new".into(),
            fixture.agent.display().to_string(),
            "--name".into(),
            "work-agent".into(),
        ],
        &fixture.installation.root,
    );
    personal.line("before personal").await;
    work.line("before work").await;
    personal.wait_for("echo: before personal").await;
    work.wait_for("echo: before work").await;

    let update = fixture.command("personal", &["update"]).output().unwrap();
    assert!(update.status.success(), "{update:?}");
    let update = text(&update);
    for expected in [
        "process-test replacement fixture",
        "Suspended 2 agent(s).",
        "Server shutting down.",
        "Updated to v999.0.0.",
        "Restarting server...",
        "Resumed 2 agent(s).",
    ] {
        assert!(
            update.contains(expected),
            "update output omitted {expected}:\n{update}"
        );
    }
    let personal_list = text(&fixture.run("personal", &["--profile", "personal", "list"]));
    let work_list = text(&fixture.run("personal", &["--profile", "work", "list"]));
    assert!(personal_list.contains("personal-agent"));
    assert!(work_list.contains("work-agent"));
    personal.wait_for("[server updating]").await;
    work.wait_for("[server updating]").await;
    assert!(!personal.exit().await.success());
    assert!(!work.exit().await.success());

    let mut personal = fixture.terminal(
        "personal",
        &["attach".into(), "personal-agent".into()],
        &fixture.installation.root,
    );
    let mut work = fixture.terminal(
        "work",
        &["attach".into(), "work-agent".into()],
        &fixture.installation.root,
    );
    personal.line("after personal").await;
    work.line("after work").await;
    personal.wait_for("echo: after personal").await;
    work.wait_for("echo: after work").await;
    personal.line("exit").await;
    work.line("exit").await;
    personal.wait_for("[session ended]").await;
    work.wait_for("[session ended]").await;
    assert!(personal.exit().await.success());
    assert!(work.exit().await.success());
}
