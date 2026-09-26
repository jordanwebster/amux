//! fake-claude-pty driven through a real terminal with the keys the
//! claude-2.1 keymap types: what it reports lands in the transcript and on
//! the hook commands, and every row and payload has a recorded shape.

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use provider_fakes::Kind;
use provider_fakes::shape::Classifier;
use serde_json::{Value, json};
use support::{corpus, script_file};

const DEADLINE: Duration = Duration::from_secs(20);
const SESSION: &str = "5e55e55e-0000-4000-8000-0000000000aa";

struct Terminal {
    handle: pty_host::PtyHandle,
    exit: pty_host::ExitMonitor,
    /// Holds the session's files until the test ends.
    _dir: tempfile::TempDir,
    transcript: PathBuf,
    hooks: PathBuf,
    socket: PathBuf,
    token: PathBuf,
}

impl Terminal {
    fn start(script: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = root.join("config");
        let hooks = root.join("hooks.jsonl");
        let recorder = format!("cat >> '{0}' && printf '\\n' >> '{0}'", hooks.display());
        let events = [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
        ];
        let token = root.join("token");
        let mut hooks_settings: serde_json::Map<String, Value> = events
            .iter()
            .map(|event| {
                let command = if *event == "SessionStart" {
                    format!(
                        "{recorder}; printf %s \"$CLAUDE_CODE_MESSAGING_TOKEN\" > '{}'",
                        token.display()
                    )
                } else {
                    recorder.clone()
                };
                (
                    event.to_string(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                )
            })
            .collect();
        let settings = json!({ "hooks": std::mem::take(&mut hooks_settings) });
        // Unix socket paths are short; keep this one under /tmp.
        let socket_dir = tempfile::Builder::new()
            .prefix("fp")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = socket_dir.keep().join("m.sock");
        let script = script_file(&root, script);
        let spawned = pty_host::spawn(pty_host::PtySpawn {
            command: env!("CARGO_BIN_EXE_fake-claude-pty").into(),
            args: vec![
                "--session-id".into(),
                SESSION.into(),
                "--settings".into(),
                settings.to_string(),
                "--messaging-socket-path".into(),
                socket.display().to_string(),
            ],
            cwd: cwd.clone(),
            env: vec![
                (OsString::from("CLAUDE_CONFIG_DIR"), config.clone().into()),
                (OsString::from(provider_fakes::SCRIPT_ENV), script.into()),
            ],
            env_remove: Vec::new(),
            size: pty_host::PtySize::default(),
        })
        .unwrap();
        let transcript = provider_fakes::playback::transcript_path(&config, &cwd, SESSION);
        Self {
            handle: spawned.handle,
            exit: spawned.exit,
            _dir: dir,
            transcript,
            hooks,
            socket,
            token,
        }
    }

    /// Start and wait for the session to announce itself.
    async fn started(script: Value) -> Self {
        let terminal = Self::start(script);
        // Nothing reads the screen; drain it so the fake never blocks on it.
        let mut output = terminal.handle.output();
        tokio::spawn(async move { while output.recv().await.is_some() {} });
        terminal.hook("SessionStart").await;
        terminal
    }

    async fn keys(&self, bytes: &[u8]) {
        self.handle.write(bytes).await.unwrap();
    }

    async fn prompt(&self, text: &str) {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        self.keys(&bytes).await;
        self.keys(b"\r").await;
    }

    fn rows(&self) -> Vec<Value> {
        lines(&self.transcript)
    }

    fn hooks(&self) -> Vec<Value> {
        lines(&self.hooks)
    }

    /// Wait until `count` payloads of `event` have reached the hook command.
    async fn hooks_of(&self, event: &str, count: usize) -> Vec<Value> {
        let started = Instant::now();
        loop {
            let found: Vec<Value> = self
                .hooks()
                .into_iter()
                .filter(|hook| hook["hook_event_name"] == event)
                .collect();
            if found.len() >= count {
                return found;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "no {count} {event} hooks: {:?}",
                self.hooks()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn hook(&self, event: &str) -> Value {
        self.hooks_of(event, 1).await.remove(0)
    }

    /// Every row and payload so far has a recorded shape.
    fn check_shapes(&self) {
        let corpus = corpus(Kind::ClaudePty);
        let mut classifier = Classifier::default();
        for frame in self.rows().iter().chain(&self.hooks()) {
            let group = classifier.provider(Kind::ClaudePty, frame);
            if let Err(drift) = corpus.check(Kind::ClaudePty, &group, frame) {
                panic!("{drift}");
            }
        }
    }

    async fn exit_code(mut self) -> u32 {
        self.handle
            .signal_process_group(pty_host::ProcessGroupSignal::Terminate)
            .ok();
        tokio::time::timeout(DEADLINE, self.exit.wait())
            .await
            .unwrap()
            .exit_code()
    }
}

impl Drop for Terminal {
    // A failed assertion must not leave the fake holding the terminal open.
    fn drop(&mut self) {
        let _ = self
            .handle
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
    }
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn contents(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter(|row| row["type"] == "user" || row["type"] == "assistant")
        .map(|row| {
            let content = &row["message"]["content"];
            match content {
                Value::String(text) => format!("{} {text}", row["type"].as_str().unwrap()),
                _ => {
                    let block = &content[0];
                    let kind = block["type"].as_str().unwrap();
                    let detail = match kind {
                        "text" => block["text"].as_str().unwrap().to_owned(),
                        "tool_use" => block["name"].as_str().unwrap().to_owned(),
                        "tool_result" => format!("error={}", block["is_error"]),
                        _ => String::new(),
                    };
                    format!("{} {kind} {detail}", row["type"].as_str().unwrap())
                }
            }
        })
        .collect()
}

#[tokio::test]
async fn a_typed_prompt_runs_a_turn_reported_in_rows_and_hooks() {
    let terminal = Terminal::started(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "Exit code 1", "error": true}}},
        {"text": {"chunks": ["Do", "ne"]}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("Look around").await;
    let stop = terminal.hook("Stop").await;
    assert_eq!(stop["last_assistant_message"], "Done");
    assert_eq!(
        contents(&terminal.rows()),
        [
            "user Look around",
            "assistant thinking ",
            "assistant tool_use Read",
            "user tool_result error=false",
            "assistant tool_use Bash",
            "user tool_result error=true",
            "assistant text Done",
        ]
    );
    let pre = terminal.hooks_of("PreToolUse", 2).await;
    assert_eq!(pre[0]["tool_name"], "Read");
    // A failed call gets no PostToolUse.
    assert_eq!(terminal.hooks_of("PostToolUse", 1).await.len(), 1);
    let start = terminal.hook("SessionStart").await;
    assert_eq!(
        start["transcript_path"],
        terminal.transcript.display().to_string()
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn menus_answer_by_digit_and_a_deny_ends_the_turn() {
    let terminal = Terminal::started(json!({"steps": [
        {"ask": {"permission": {"class": "consequential", "input": {"command": "touch x"},
                                "outcome": {"output": "ok"}}}},
        {"ask": {"plan": {"markdown": "1. Do it"}}},
        {"ask": {"plan": {"markdown": "2. Again"}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
        {"text": {"chunks": ["next turn"]}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("Ask").await;
    terminal.hooks_of("PermissionRequest", 1).await;
    terminal.keys(b"1").await;
    terminal.hooks_of("PermissionRequest", 2).await;
    terminal.keys(b"3").await;
    terminal.keys(b"Not yet\r").await;
    terminal.hooks_of("PermissionRequest", 3).await;
    terminal.keys(b"2").await;
    terminal.hooks_of("PermissionRequest", 4).await;
    terminal.keys(b"3").await;
    terminal.hook("Stop").await;
    let rows = contents(&terminal.rows());
    assert_eq!(
        rows,
        [
            "user Ask",
            "assistant tool_use Bash",
            "user tool_result error=false",
            "assistant tool_use ExitPlanMode",
            "user tool_result error=true",
            "assistant tool_use ExitPlanMode",
            "user tool_result error=false",
            "assistant tool_use Bash",
            "user tool_result error=true",
            "user text [Request interrupted by user for tool use]",
        ]
    );
    let feedback = terminal.rows().into_iter().find(|row| {
        row["message"]["content"][0]["content"]
            .as_str()
            .is_some_and(|text| text.contains("Not yet"))
    });
    assert!(feedback.is_some());
    terminal.prompt("Next").await;
    terminal.hooks_of("Stop", 2).await;
    assert!(contents(&terminal.rows()).contains(&"assistant text next turn".to_owned()));
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_question_form_takes_digits_arrows_space_tab_and_enter() {
    let terminal = Terminal::started(json!({"steps": [
        {"ask": {"question": {"questions": [
            {"question": "Color?", "header": "Color", "options": ["Red", "Blue"]},
            {"question": "Sizes?", "header": "Size", "options": ["S", "M", "L"], "multi_select": true},
        ]}}},
        "turn_end",
    ]})).await;
    terminal.prompt("Choose").await;
    terminal.hook("PermissionRequest").await;
    // Question one: the digit picks Blue. Question two: down to M, toggle,
    // down to the other row, type an answer, toggle it, tab; then the
    // review page's two enters.
    terminal.keys(b"2").await;
    terminal.keys(b"\x1b[B").await;
    terminal.keys(b" ").await;
    terminal.keys(b"\x1b[B\x1b[B").await;
    terminal.keys(b"\r").await;
    terminal.keys(b"XL\r").await;
    terminal.keys(b" ").await;
    terminal.keys(b"\t").await;
    terminal.keys(b"\r").await;
    terminal.keys(b"\r").await;
    terminal.hook("Stop").await;
    let result = terminal
        .rows()
        .into_iter()
        .find(|row| row["toolUseResult"]["answers"].is_object())
        .unwrap();
    assert_eq!(
        result["toolUseResult"]["answers"],
        json!({"Color?": "Blue", "Sizes?": "M, XL"})
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_prompt_typed_mid_turn_folds_at_the_next_tool_and_escape_interrupts() {
    let probe = tempfile::tempdir().unwrap();
    let gate = probe.path().join("gate");
    let never = probe.path().join("never");
    let terminal = Terminal::started(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"tool": {"class": "exploration"}},
        {"text": {"chunks": ["folded"]}},
        "turn_end",
        {"wait_for": {"path": never}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("First").await;
    terminal.prompt("Also this").await;
    let started = Instant::now();
    while !terminal
        .rows()
        .iter()
        .any(|row| row["type"] == "queue-operation")
    {
        assert!(started.elapsed() < DEADLINE);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::write(&gate, "").unwrap();
    terminal.hook("Stop").await;
    let rows = terminal.rows();
    let folded = rows
        .iter()
        .find(|row| row["attachment"]["type"] == "queued_command")
        .unwrap();
    assert_eq!(folded["attachment"]["prompt"], "Also this");
    terminal.prompt("Wait forever").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    terminal.keys(b"\x1b").await;
    terminal.hooks_of("Stop", 2).await;
    assert!(
        contents(&terminal.rows()).contains(&"user text [Request interrupted by user]".to_owned())
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_messaging_socket_message_runs_a_turn_and_hooks_get_its_credentials() {
    use tokio::io::AsyncWriteExt;
    let terminal = Terminal::started(json!({"steps": [
        {"text": {"chunks": ["from a peer"]}},
        "turn_end",
    ]}))
    .await;
    // The token reaches hook commands through their environment only.
    let token = std::fs::read_to_string(&terminal.token).unwrap();
    assert_eq!(token.len(), 32);
    assert!(!terminal.hooks()[0].to_string().contains(&token));
    let mut stream = tokio::net::UnixStream::connect(&terminal.socket)
        .await
        .unwrap();
    // Without the right token the message is dropped.
    stream
        .write_all(b"{\"type\":\"auth\",\"token\":\"wrong\"}\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"dropped\"}}\n")
        .await
        .unwrap();
    drop(stream);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !contents(&terminal.rows())
            .iter()
            .any(|row| row.contains("dropped"))
    );
    let mut stream = tokio::net::UnixStream::connect(&terminal.socket)
        .await
        .unwrap();
    let message = format!(
        "{{\"type\":\"auth\",\"token\":\"{token}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello from a peer\"}}}}\n"
    );
    stream.write_all(message.as_bytes()).await.unwrap();
    drop(stream);
    terminal.hook("Stop").await;
    let rows = terminal.rows();
    let user = rows.iter().find(|row| row["type"] == "user").unwrap();
    assert_eq!(user["message"]["content"], "hello from a peer");
    assert_eq!(user["origin"]["kind"], "peer");
    assert!(contents(&rows).contains(&"assistant text from a peer".to_owned()));
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn exit_ends_the_process_with_its_code() {
    let terminal = Terminal::started(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 5}},
    ]}))
    .await;
    terminal.prompt("Leave").await;
    let mut exit = terminal.exit.clone();
    let status = tokio::time::timeout(DEADLINE, exit.wait()).await.unwrap();
    assert_eq!(status.exit_code(), 5);
}
