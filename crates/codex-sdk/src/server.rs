use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};

use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::config::{self, CodexConfig, ThreadConfig};
use crate::dispatch::ServerInner;
use crate::error::Error;
use crate::init::{ClientInfo, InitializationResult, InitializeCapabilities, InitializeParams};
use crate::notification::ServerNotification;
use crate::thread::Thread;
use crate::transport;
use crate::types::{
    AccountReadParams, AccountReadResponse, ListThreadsParams, ThreadListResponse,
    ThreadSessionInfo,
};

// ── Codex ────────────────────────────────────────────────────────

/// Handle to a running codex app-server subprocess.
///
/// Created via [`connect()`](crate::connect). All methods take `&self`.
/// The handle is backed by `Arc<ServerInner>` and is cheap to clone.
#[derive(Clone)]
pub struct Codex {
    pub(crate) inner: Arc<ServerInner>,
    global_notif_rx: Arc<Mutex<Option<mpsc::Receiver<ServerNotification>>>>,
}

impl Codex {
    /// Connect to a codex app-server, performing the initialize handshake.
    pub(crate) async fn connect(config: CodexConfig) -> Result<Self, Error> {
        let (child, stdin, stdout, stderr) =
            transport::spawn_process(&config).map_err(|e| Error::Process(format!("{e:#}")))?;

        let cancel = CancellationToken::new();
        let recorder = config
            .record_io
            .as_deref()
            .map(transport::WireRecorder::new)
            .transpose()?;
        let child_waiter = transport::spawn_child_waiter(child, cancel.clone());
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let (global_notif_tx, global_notif_rx) = mpsc::channel::<ServerNotification>(64);

        let inner = Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(std::collections::HashMap::new()),
            thread_channels: Mutex::new(std::collections::HashMap::new()),
            global_notif_tx,
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: cancel.clone(),
            child_waiter: Mutex::new(Some(child_waiter)),
        });

        // Spawn background tasks
        transport::spawn_reader_task(stdout, inner.clone(), cancel.clone(), recorder.clone());
        transport::spawn_writer_task(stdin, stdin_rx, cancel.clone(), recorder);
        transport::spawn_stderr_task(stderr, "codex app-server stdio");

        let codex = Self {
            inner,
            global_notif_rx: Arc::new(Mutex::new(Some(global_notif_rx))),
        };

        // Initialize handshake
        let init: InitializationResult = match codex
            .inner
            .request("initialize", initialize_params(&config))
            .await
        {
            Ok(init) => init,
            Err(error) => {
                codex.inner.shutdown().await;
                return Err(error);
            }
        };
        if let Err(error) = codex.inner.notify_no_params("initialized").await {
            codex.inner.shutdown().await;
            return Err(error);
        }
        let _ = codex.inner.init_result.set(init);

        Ok(codex)
    }

    /// Create a Codex handle from raw IO handles (for replay/testing).
    /// Performs the initialize handshake via the provided IO.
    pub async fn from_io(
        reader: impl AsyncBufRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        config: CodexConfig,
    ) -> Result<Self, Error> {
        let cancel = CancellationToken::new();
        let recorder = config
            .record_io
            .as_deref()
            .map(transport::WireRecorder::new)
            .transpose()?;
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let (global_notif_tx, global_notif_rx) = mpsc::channel::<ServerNotification>(64);

        let inner = Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(std::collections::HashMap::new()),
            thread_channels: Mutex::new(std::collections::HashMap::new()),
            global_notif_tx,
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: cancel.clone(),
            child_waiter: Mutex::new(None),
        });

        transport::spawn_reader_task(reader, inner.clone(), cancel.clone(), recorder.clone());
        transport::spawn_writer_task(writer, stdin_rx, cancel, recorder);

        let codex = Self {
            inner,
            global_notif_rx: Arc::new(Mutex::new(Some(global_notif_rx))),
        };

        // Initialize handshake with the caller's identity and capabilities.
        let init: InitializationResult = match codex
            .inner
            .request("initialize", initialize_params(&config))
            .await
        {
            Ok(init) => init,
            Err(error) => {
                codex.inner.shutdown().await;
                return Err(error);
            }
        };
        if let Err(error) = codex.inner.notify_no_params("initialized").await {
            codex.inner.shutdown().await;
            return Err(error);
        }
        let _ = codex.inner.init_result.set(init);

        Ok(codex)
    }

    /// Return the cached initialize response, if available.
    pub fn initialization_result(&self) -> Option<&InitializationResult> {
        self.inner.init_result.get()
    }

    // ── Thread management ────────────────────────────────────────

    /// Start a new thread.
    pub async fn start_thread(&self, config: ThreadConfig) -> Result<Thread, Error> {
        let session: ThreadSessionInfo = self
            .inner
            .request("thread/start", config::thread_config_to_params(&config))
            .await?;
        let registration = self.inner.register_thread(&session.thread.id).await;
        Ok(Thread::new(self.inner.clone(), session, registration))
    }

    /// Resume an existing thread by ID.
    pub async fn resume_thread(
        &self,
        thread_id: &str,
        config: ThreadConfig,
    ) -> Result<Thread, Error> {
        let mut params = config::thread_config_to_params(&config);
        if let serde_json::Value::Object(ref mut m) = params {
            m.insert("threadId".into(), serde_json::json!(thread_id));
        }
        // Install a fresh registration before the RPC. The app-server may emit
        // replay/history notifications before returning the response.
        let registration = self.inner.reregister_thread_for_resume(thread_id).await;
        let session: ThreadSessionInfo = self.inner.request("thread/resume", params).await?;
        registration.finish_staging().await;
        Ok(Thread::new(self.inner.clone(), session, registration))
    }

    /// List threads.
    pub async fn list_threads(
        &self,
        params: ListThreadsParams,
    ) -> Result<ThreadListResponse, Error> {
        self.inner.request("thread/list", params).await
    }

    /// Rename a thread.
    pub async fn rename_thread(&self, thread_id: &str, name: &str) -> Result<(), Error> {
        self.inner
            .request_unit(
                "thread/name/set",
                serde_json::json!({ "threadId": thread_id, "name": name }),
            )
            .await
    }

    // ── Non-thread operations ────────────────────────────────────

    /// Read the currently authenticated account, optionally refreshing its token first.
    pub async fn read_account(
        &self,
        params: AccountReadParams,
    ) -> Result<AccountReadResponse, Error> {
        self.inner.request("account/read", params).await
    }

    // ── Global notifications ─────────────────────────────────────

    /// Take the global (non-thread-scoped) notification receiver.
    /// Returns `None` if already taken.
    pub fn take_notifications(&self) -> Option<mpsc::Receiver<ServerNotification>> {
        self.global_notif_rx.try_lock().ok()?.take()
    }

    // ── Shutdown ─────────────────────────────────────────────────

    /// Shut down the codex app-server subprocess.
    pub async fn close(self) {
        self.inner.shutdown().await;
    }

    /// Whether the underlying transport reader has terminated.
    pub fn is_closed(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }
}

fn initialize_params(config: &CodexConfig) -> InitializeParams {
    InitializeParams {
        client_info: ClientInfo {
            name: config.client_name.clone(),
            title: config.client_title.clone(),
            version: config.client_version.clone(),
        },
        capabilities: Some(InitializeCapabilities {
            experimental_api: config.experimental_api,
            opt_out_notification_methods: if config.opt_out_notification_methods.is_empty() {
                None
            } else {
                Some(config.opt_out_notification_methods.clone())
            },
        }),
    }
}

#[cfg(test)]
mod remediation_tests {
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    use super::*;

    fn test_io() -> (
        BufReader<DuplexStream>,
        DuplexStream,
        BufReader<DuplexStream>,
        DuplexStream,
    ) {
        let (client_reader, server_writer) = tokio::io::duplex(4096);
        let (client_writer, server_reader) = tokio::io::duplex(4096);
        (
            BufReader::new(client_reader),
            client_writer,
            BufReader::new(server_reader),
            server_writer,
        )
    }

    async fn read_json_line(reader: &mut BufReader<DuplexStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write_json_line(writer: &mut DuplexStream, value: serde_json::Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    fn thread_session(thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "thread": {
                "id": thread_id,
                "path": null,
                "agentNickname": null,
                "agentRole": null,
                "gitInfo": null,
                "name": null
            },
            "model": "test",
            "modelProvider": "openai",
            "serviceTier": null,
            "cwd": "/tmp",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandbox": {"type": "readOnly", "networkAccess": false},
            "reasoningEffort": null
        })
    }

    #[tokio::test]
    async fn initialize_error_cancels_io_tasks() {
        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(
            client_reader,
            client_writer,
            CodexConfig::default(),
        ));

        let initialize = read_json_line(&mut server_reader).await;
        write_json_line(
            &mut server_writer,
            serde_json::json!({
                "id": initialize["id"],
                "error": {"code": -32602, "message": "bad initialize", "data": null}
            }),
        )
        .await;

        assert!(matches!(
            connect.await.unwrap(),
            Err(Error::Rpc { code: -32602, .. })
        ));
        let mut trailing = String::new();
        let bytes = tokio::time::timeout(
            Duration::from_secs(1),
            server_reader.read_line(&mut trailing),
        )
        .await
        .expect("writer task remained alive after initialize failed")
        .unwrap();
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn resume_stages_more_than_channel_capacity_without_overflow_or_retry_loop() {
        const REPLAY_EVENTS: usize = 300;

        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(
            client_reader,
            client_writer,
            CodexConfig::default(),
        ));

        let initialize = read_json_line(&mut server_reader).await;
        write_json_line(
            &mut server_writer,
            serde_json::json!({
                "id": initialize["id"],
                "result": {
                    "userAgent": "test",
                    "codexHome": "/tmp",
                    "platformFamily": "unix",
                    "platformOs": "test"
                }
            }),
        )
        .await;
        let codex = connect.await.unwrap().unwrap();
        assert_eq!(
            read_json_line(&mut server_reader).await["method"],
            "initialized"
        );

        let resume_client = codex.clone();
        let resume = tokio::spawn(async move {
            resume_client
                .resume_thread("thread-long", ThreadConfig::default())
                .await
        });
        let request = read_json_line(&mut server_reader).await;
        assert_eq!(request["method"], "thread/resume");

        // Replay is deliberately sent before the response, when resume_thread's
        // caller cannot yet take and drain the returned event stream.
        for index in 0..REPLAY_EVENTS {
            write_json_line(
                &mut server_writer,
                serde_json::json!({
                    "method": "warning",
                    "params": {
                        "threadId": "thread-long",
                        "turnId": "turn-old",
                        "message": index.to_string()
                    }
                }),
            )
            .await;
        }
        write_json_line(
            &mut server_writer,
            serde_json::json!({"id": request["id"], "result": thread_session("thread-long")}),
        )
        .await;

        let thread = tokio::time::timeout(Duration::from_secs(2), resume)
            .await
            .expect("resume deadlocked while replaying history")
            .unwrap()
            .unwrap();
        let mut events = thread.events().await.unwrap();
        for index in 0..REPLAY_EVENTS {
            let event = events.next().await.unwrap().expect("staged replay event");
            assert_eq!(event.params["message"], index.to_string());
        }
        assert_eq!(
            thread.inner.registration.state(),
            crate::dispatch::ThreadChannelState::Open,
            "the single resume must not poison its registration and trigger another resume"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), events.next())
                .await
                .is_err(),
            "the replay ended cleanly without an immediate overflow error"
        );

        codex.close().await;
    }

    #[cfg(unix)]
    async fn assert_subprocess_waiter_awaited(initialize_error: bool) {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake-codex");
        let marker_path = temp.path().join("stopped");
        let response = if initialize_error {
            serde_json::json!({
                "id": 1,
                "error": {"code": -32602, "message": "bad initialize", "data": null}
            })
        } else {
            serde_json::json!({
                "id": 1,
                "result": {
                    "userAgent": "fake",
                    "codexHome": "/tmp",
                    "platformFamily": "unix",
                    "platformOs": "test"
                }
            })
        };
        let script = format!(
            "#!/bin/sh\ntrap 'sleep 0.1; printf stopped > \"$MARKER\"; exit 0' TERM\nIFS= read -r request\nprintf '%s\\n' '{}'\nwhile :; do sleep 1; done\n",
            response
        );
        std::fs::write(&script_path, script).unwrap();
        let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();

        let mut config = CodexConfig {
            codex_path: Some(script_path),
            ..CodexConfig::default()
        };
        config.env = Some(std::collections::HashMap::from([(
            "MARKER".into(),
            marker_path.to_string_lossy().into_owned(),
        )]));

        let result = tokio::time::timeout(Duration::from_secs(5), Codex::connect(config))
            .await
            .expect("connect/shutdown timed out");
        if initialize_error {
            assert!(matches!(result, Err(Error::Rpc { code: -32602, .. })));
        } else {
            result.unwrap().close().await;
        }
        assert_eq!(std::fs::read_to_string(marker_path).unwrap(), "stopped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initialize_error_awaits_subprocess_waiter() {
        assert_subprocess_waiter_awaited(true).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_awaits_subprocess_waiter() {
        assert_subprocess_waiter_awaited(false).await;
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::*;

    #[tokio::test]
    async fn from_io_sends_initialized_after_initialize() {
        let (client_side, server_side) = duplex(4096);
        let (client_read, client_write) = split(client_side);
        let (server_read, mut server_write) = split(server_side);

        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();

            reader.read_line(&mut line).await.unwrap();
            let initialize: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(initialize["method"], "initialize");

            server_write
                .write_all(
                    br#"{"id":1,"result":{"userAgent":"ua","codexHome":"/tmp/codex","platformFamily":"unix","platformOs":"macos"}}"#,
                )
                .await
                .unwrap();
            server_write.write_all(b"\n").await.unwrap();
            server_write.flush().await.unwrap();

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let initialized: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(initialized["method"], "initialized");
            assert!(initialized.get("params").is_none());
        });

        let codex = Codex::from_io(
            BufReader::new(client_read),
            client_write,
            CodexConfig::default(),
        )
        .await
        .unwrap();
        let init = codex.initialization_result().unwrap();
        assert_eq!(init.user_agent, "ua");
        assert_eq!(init.codex_home, std::path::Path::new("/tmp/codex"));
        assert_eq!(init.platform_family, "unix");
        assert_eq!(init.platform_os, "macos");

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn record_io_tees_both_wire_directions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("io.jsonl");
        let (client_side, server_side) = duplex(4096);
        let (client_read, client_write) = split(client_side);
        let (server_read, mut server_write) = split(server_side);
        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            server_write
                .write_all(br#"{"id":1,"result":{"userAgent":"ua","codexHome":"/tmp/codex","platformFamily":"unix","platformOs":"macos"}}
"#)
                .await
                .unwrap();
            server_write.flush().await.unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
        });
        let codex = Codex::from_io(
            BufReader::new(client_read),
            client_write,
            CodexConfig {
                record_io: Some(path.clone()),
                ..CodexConfig::default()
            },
        )
        .await
        .unwrap();
        server_task.await.unwrap();
        codex.close().await;

        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["dir"], "stdin");
        assert_eq!(rows[1]["dir"], "stdout");
        assert_eq!(rows[2]["dir"], "stdin");
        assert!(rows.iter().all(|row| row["line"].is_string()));
    }
}
