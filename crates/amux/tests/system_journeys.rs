//! System journeys: whole stories told with the built binaries as a person
//! and the machine run them, each printing its transcript. `just journey
//! system <id>` runs one; the ids are declared in journeys/manifest.json.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use provider_fakes::{SCRIPT_ENV, Script};
use support::{
    Channel, amux_binary, binaries, chat, client, count, created_id, gated_turn, line_of, texts,
    turns_journaled, until, until_within,
};
use tokio::process::Command;

/// How long agents outlive their daemon before they drain and exit.
const GRACE_SECS: u64 = 3;

/// Prints one line of the transcript.
fn say(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
}

/// A desktop install in a temporary directory: supervised, its binary a
/// copy the supervisor may replace, `claude` and `codex` the fakes.
struct Desk {
    root: tempfile::TempDir,
    bin: PathBuf,
    config: PathBuf,
    socket: PathBuf,
    data: PathBuf,
    work: PathBuf,
}

impl Desk {
    fn new(installed_version: &str, releases_url: &str, steps: Vec<provider_fakes::Step>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let work = root.path().join("work");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        node::release::restamp(amux_binary(), &bin.join("amux"), installed_version).unwrap();
        // One `claude` for both Claude kinds, as on a real machine: the
        // headless kind is the one started with --print.
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = --print ] && exec {sdk} \"$@\"; done\nexec {pty} \"$@\"\n",
                sdk = binaries().join("fake-claude-sdk").display(),
                pty = binaries().join("fake-claude-pty").display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(binaries().join("fake-codex"), bin.join("codex")).unwrap();
        std::fs::write(
            root.path().join("script.json"),
            serde_json::to_string(&Script {
                steps,
                ..Script::default()
            })
            .unwrap(),
        )
        .unwrap();
        let data = root.path().join("data");
        let socket = root.path().join("amux.sock");
        let config = root.path().join("installation.yaml");
        std::fs::write(
            &config,
            format!(
                "root: {}\nfront_door_socket: {}\nsupervisor: on\nupdates: manual\nkeep_awake: off\n\
                 releases_url: {releases_url}\nagent:\n  grace_secs: {GRACE_SECS}\n  drain_secs: 5\n",
                data.display(),
                socket.display(),
            ),
        )
        .unwrap();
        Self {
            bin,
            config,
            socket,
            data,
            work,
            root,
        }
    }

    fn amux_path(&self) -> PathBuf {
        self.bin.join("amux")
    }

    fn command(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(self.amux_path());
        command
            .args(args)
            .env("AMUX_CONFIG", &self.config)
            .env("PATH", path)
            .env(SCRIPT_ENV, self.root.path().join("script.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.path().join("claude"))
            .env_remove("AMUX_LOG")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn amux(&self, args: &[&str]) -> Output {
        tokio::time::timeout(support::PATIENCE, self.command(args).output())
            .await
            .unwrap_or_else(|_| panic!("amux {args:?} did not finish"))
            .expect("amux runs")
    }

    /// Runs a verb that must succeed, printing it and what it said.
    async fn run(&self, args: &[&str]) -> String {
        let output = self.amux(args).await;
        assert!(
            output.status.success(),
            "amux {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let said = String::from_utf8(output.stdout).unwrap();
        let root = self.root.path().to_string_lossy().into_owned();
        say(format!("$ amux {}", args.join(" ")).replace(&root, "$DESK"));
        for line in said.lines() {
            say(format!("  {line}").replace(&root, "$DESK"));
        }
        said
    }

    fn agent_dir(&self, id: &[u8]) -> PathBuf {
        let profiles = self.data.join("profiles");
        let profile = std::fs::read_dir(&profiles)
            .unwrap()
            .next()
            .expect("a profile directory")
            .unwrap()
            .path();
        profile
            .join("agents")
            .join(uuid::Uuid::from_slice(id).unwrap().to_string())
    }

    fn supervisor_pid(&self) -> Option<i32> {
        let path = self.data.join(node::supervisor::SUPERVISOR_LOCK);
        lock_held(&path)
            .then(|| std::fs::read_to_string(&path).ok()?.trim().parse().ok())
            .flatten()
    }

    /// The daemon the supervisor started last, from its log.
    fn daemon_pid(&self) -> i32 {
        let log = std::fs::read_to_string(self.data.join("supervisor.log")).unwrap();
        log.lines()
            .rev()
            .find_map(|line| {
                line.contains("started the daemon")
                    .then(|| line.rsplit_once("pid=")?.1.trim().parse().ok())
                    .flatten()
            })
            .expect("the supervisor started a daemon")
    }

    fn daemon_running(&self) -> bool {
        lock_held(&self.data.join(node::INSTALLATION_LOCK))
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        let _ = std::process::Command::new(self.amux_path())
            .args(["server", "stop"])
            .env("AMUX_CONFIG", &self.config)
            .env_remove("AMUX_LOG")
            .output();
    }
}

/// The chat's items that say something, for the transcript.
fn spoken(chat: &[(String, String)]) -> Vec<&str> {
    texts(chat)
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect()
}

/// Whether some process holds the lock on the file at `path`.
fn lock_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

fn kill(pid: i32) {
    // SAFETY: a signal to a process this test started.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Three agents, one of each kind, are mid-turn when amux is killed. They
/// finish their turns with nobody reading, outlive their grace and exit.
/// amux comes back under its supervisor, a newer build is installed and
/// activated, and every finished turn is in the store; resuming the exited
/// agents loses nothing.
#[tokio::test(flavor = "multi_thread")]
async fn survive_daemon() {
    let running = node::version();
    let previous = version_stamp::previous(running).expect("a release version to go below");
    let channel = Channel::serve().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    let desk = Desk::new(&previous, channel.url(), gated_turn(&gate));
    let work = desk.work.to_string_lossy().into_owned();
    say(format!(
        "== survive-daemon: amux {previous} installed; the build under test, {running}, is the newer release"
    ));

    desk.run(&["server", "start"]).await;
    let mut agents: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (name, kind) in [
        ("terminal", "claude_pty"),
        ("headless", "claude_sdk"),
        ("codex", "codex"),
    ] {
        let created = desk
            .run(&[
                "create",
                kind,
                "--name",
                name,
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await;
        agents.insert(name, created_id(&created));
    }
    let mut chats = client(&desk.socket).await;
    for (name, id) in &agents {
        until(&format!("{name} is mid-turn"), async || {
            texts(&chat(&mut chats, id).await).contains(&"started")
        })
        .await;
    }
    let listing = desk.run(&["ls"]).await;
    for name in agents.keys() {
        assert!(line_of(&listing, name).contains("working"), "{listing}");
    }

    // amux dies: the supervisor and its daemon, with no chance to tell
    // anyone. The agents see their control sockets close and carry on.
    let supervisor = desk.supervisor_pid().expect("a supervisor runs");
    let daemon = desk.daemon_pid();
    kill(supervisor);
    kill(daemon);
    say(format!(
        "-- killed amux supervise (pid {supervisor}) and amux daemon (pid {daemon}) with SIGKILL, every agent mid-turn"
    ));
    until("the daemon is gone", async || !desk.daemon_running()).await;

    std::fs::write(&gate, b"").unwrap();
    for (name, id) in &agents {
        let dir = desk.agent_dir(id);
        until(
            &format!("{name} finishes its turn with no daemon"),
            async || turns_journaled(&dir) == 1,
        )
        .await;
    }
    say("-- every agent finished its turn into its journal with no daemon running");
    for (name, id) in &agents {
        let dir = desk.agent_dir(id);
        until_within(
            &format!("{name} drains and exits once its grace runs out"),
            Duration::from_secs(GRACE_SECS) + support::PATIENCE,
            async || !agent_dir::locked(&dir),
        )
        .await;
    }
    say(format!(
        "-- after {GRACE_SECS} s of grace every agent drained and exited"
    ));

    // Any amux command finds amux stopped and starts its supervisor.
    let listing = desk.run(&["ls"]).await;
    for name in agents.keys() {
        assert!(
            line_of(&listing, name).contains("exited while the daemon was away"),
            "{listing}"
        );
    }

    // The newer build is installed under the supervisor and activated.
    channel.publish(running, std::fs::read(amux_binary()).unwrap());
    say(format!("-- the stable channel now names {running}"));
    let updated = desk.run(&["update"]).await;
    assert!(
        updated.contains(&format!("amux {running} is running.")),
        "{updated}"
    );
    let installed = desk.run(&["--version"]).await;
    assert_eq!(installed.trim(), format!("amux {running}"));
    assert!(
        !desk.bin.join("amux.prev").exists(),
        "activated: prev is gone"
    );

    // Every finished turn is in the store.
    let mut chats = client(&desk.socket).await;
    let mut before = BTreeMap::new();
    for (name, id) in &agents {
        let found = chat(&mut chats, id).await;
        for said in ["first task", "started", "finished"] {
            assert_eq!(
                count(&found, said),
                1,
                "{name}'s first turn is whole in the store: {:?}",
                texts(&found)
            );
        }
        say(format!("{name} in the store: {:?}", spoken(&found)));
        before.insert(*name, found);
    }

    // Resuming the exited agents loses nothing.
    for name in agents.keys() {
        desk.run(&["resume", name, "second", "task"]).await;
    }
    for (name, id) in &agents {
        until(&format!("{name} finishes its second turn"), async || {
            count(&chat(&mut chats, id).await, "finished") == 2
        })
        .await;
        let after = chat(&mut chats, id).await;
        for item in &before[name] {
            assert!(after.contains(item), "{name}: {item:?} survives the resume");
        }
        assert!(texts(&after).contains(&"second task"));
        say(format!("{name} in the store: {:?}", spoken(&after)));
    }
    desk.run(&["ls"]).await;
    for name in agents.keys() {
        desk.run(&["stop", name, "--mode", "kill"]).await;
    }
    desk.run(&["server", "stop"]).await;
    say("== survive-daemon: every finished turn is in the store and every resume kept its history");
}
