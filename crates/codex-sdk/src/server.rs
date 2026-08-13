use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalHandler;
use crate::config::{self, CodexConfig, ThreadConfig, TurnInput};
use crate::dispatch::ServerInner;
use crate::error::Error;
use crate::init::{ClientInfo, InitializationResult, InitializeCapabilities, InitializeParams};
use crate::notification::{ServerNotification, ThreadEvent};
use crate::thread::Thread;
use crate::transport;
use crate::types::{
    AccountReadParams, AccountReadResponse, ConfigReadParams, ExecCommandParams,
    ExecCommandResizeParams, ExecCommandResult, ExecCommandTerminateParams, ExecCommandWriteParams,
    ListThreadsParams, Model, ModelListParams, ThreadInfo, ThreadListResponse, ThreadReadResponse,
    ThreadSessionInfo, Turn,
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
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let (global_notif_tx, global_notif_rx) = mpsc::channel::<ServerNotification>(64);

        let inner = Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(std::collections::HashMap::new()),
            thread_channels: Mutex::new(std::collections::HashMap::new()),
            approval_handler: config
                .approval_handler
                .clone()
                .map(|h| h as Arc<dyn ApprovalHandler>),
            global_notif_tx,
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: cancel.clone(),
        });

        // Spawn background tasks
        transport::spawn_reader_task(stdout, inner.clone(), cancel.clone());
        transport::spawn_writer_task(stdin, stdin_rx, cancel.clone());
        transport::spawn_stderr_task(stderr, "codex app-server stdio");
        transport::spawn_child_waiter(child, cancel);

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
                codex.inner.cancel.cancel();
                return Err(error);
            }
        };
        if let Err(error) = codex.inner.notify_no_params("initialized").await {
            codex.inner.cancel.cancel();
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
    ) -> Result<Self, Error> {
        let cancel = CancellationToken::new();
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let (global_notif_tx, global_notif_rx) = mpsc::channel::<ServerNotification>(64);

        let inner = Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(std::collections::HashMap::new()),
            thread_channels: Mutex::new(std::collections::HashMap::new()),
            approval_handler: None,
            global_notif_tx,
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: cancel.clone(),
        });

        transport::spawn_reader_task(reader, inner.clone(), cancel.clone());
        transport::spawn_writer_task(writer, stdin_rx, cancel);

        let codex = Self {
            inner,
            global_notif_rx: Arc::new(Mutex::new(Some(global_notif_rx))),
        };

        // Initialize handshake with default values
        let init: InitializationResult = match codex
            .inner
            .request(
                "initialize",
                InitializeParams {
                    client_info: ClientInfo {
                        name: "codex-rust-sdk".into(),
                        title: None,
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                    capabilities: Some(InitializeCapabilities {
                        experimental_api: true,
                        opt_out_notification_methods: None,
                    }),
                },
            )
            .await
        {
            Ok(init) => init,
            Err(error) => {
                codex.inner.cancel.cancel();
                return Err(error);
            }
        };
        if let Err(error) = codex.inner.notify_no_params("initialized").await {
            codex.inner.cancel.cancel();
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
        self.request_thread("thread/start", config::thread_config_to_params(&config))
            .await
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
        self.request_thread("thread/resume", params).await
    }

    /// Fork an existing thread.
    pub async fn fork_thread(&self, thread_id: &str) -> Result<Thread, Error> {
        self.fork_thread_with(thread_id, ThreadConfig::default())
            .await
    }

    /// Fork an existing thread with explicit config overrides.
    pub async fn fork_thread_with(
        &self,
        thread_id: &str,
        config: ThreadConfig,
    ) -> Result<Thread, Error> {
        let mut params = config::thread_config_to_params(&config);
        if let serde_json::Value::Object(ref mut m) = params {
            m.insert("threadId".into(), serde_json::json!(thread_id));
        }
        self.request_thread("thread/fork", params).await
    }

    /// List threads.
    pub async fn list_threads(
        &self,
        params: ListThreadsParams,
    ) -> Result<ThreadListResponse, Error> {
        self.inner.request("thread/list", params).await
    }

    /// Read a thread's info (and optionally its turns).
    pub async fn read_thread(
        &self,
        thread_id: &str,
        include_turns: bool,
    ) -> Result<ThreadReadResponse, Error> {
        self.inner
            .request(
                "thread/read",
                serde_json::json!({
                    "threadId": thread_id,
                    "includeTurns": include_turns,
                }),
            )
            .await
    }

    /// Archive a thread.
    pub async fn archive_thread(&self, thread_id: &str) -> Result<(), Error> {
        self.inner
            .request_unit(
                "thread/archive",
                serde_json::json!({ "threadId": thread_id }),
            )
            .await
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

    /// Unarchive a thread.
    pub async fn unarchive_thread(&self, thread_id: &str) -> Result<ThreadInfo, Error> {
        let response: crate::types::ThreadReadResponse = self
            .inner
            .request(
                "thread/unarchive",
                serde_json::json!({ "threadId": thread_id }),
            )
            .await?;
        Ok(response.thread)
    }

    // ── One-shot convenience ─────────────────────────────────────

    /// Start a thread, run one turn, collect the completed turn, close the thread.
    pub async fn prompt(
        &self,
        input: impl Into<TurnInput>,
        thread_config: ThreadConfig,
    ) -> Result<Turn, Error> {
        let thread = self.start_thread(thread_config).await?;
        let mut stream = thread.turn(input).await?;
        while stream.next().await.is_some() {}
        stream
            .completed_turn()
            .cloned()
            .ok_or_else(|| Error::Internal(anyhow::anyhow!("turn did not complete")))
    }

    // ── Non-thread operations ────────────────────────────────────

    /// List available models.
    pub async fn list_models(&self) -> Result<Vec<Model>, Error> {
        self.list_models_with(ModelListParams::default()).await
    }

    /// List available models with protocol-aligned pagination params.
    pub async fn list_models_with(&self, params: ModelListParams) -> Result<Vec<Model>, Error> {
        let resp: crate::types::ModelListResponse =
            self.inner.request("model/list", params).await?;
        Ok(resp.data)
    }

    /// Read the server's active configuration.
    pub async fn read_config(&self) -> Result<serde_json::Value, Error> {
        self.inner
            .request("config/read", ConfigReadParams::default())
            .await
    }

    /// Read the currently authenticated account, optionally refreshing its token first.
    pub async fn read_account(
        &self,
        params: AccountReadParams,
    ) -> Result<AccountReadResponse, Error> {
        self.inner.request("account/read", params).await
    }

    /// Execute a command on the server.
    pub async fn exec_command(
        &self,
        params: ExecCommandParams,
    ) -> Result<ExecCommandResult, Error> {
        self.inner.request("command/exec", params).await
    }

    /// Write stdin bytes to a running standalone command execution.
    pub async fn exec_command_write(&self, params: ExecCommandWriteParams) -> Result<(), Error> {
        self.inner.request_unit("command/exec/write", params).await
    }

    /// Resize a PTY-backed standalone command execution.
    pub async fn exec_command_resize(&self, params: ExecCommandResizeParams) -> Result<(), Error> {
        self.inner.request_unit("command/exec/resize", params).await
    }

    /// Terminate a running standalone command execution.
    pub async fn exec_command_terminate(
        &self,
        params: ExecCommandTerminateParams,
    ) -> Result<(), Error> {
        self.inner
            .request_unit("command/exec/terminate", params)
            .await
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
        self.inner.cancel.cancel();
    }

    // ── Internal ─────────────────────────────────────────────────

    async fn register_thread(&self, session: ThreadSessionInfo) -> Thread {
        let (event_tx, event_rx) = mpsc::channel::<ThreadEvent>(256);
        self.inner
            .register_thread(&session.thread.id, event_tx)
            .await;
        Thread::new(self.inner.clone(), session, event_rx)
    }

    async fn request_thread<P: Serialize>(&self, method: &str, params: P) -> Result<Thread, Error> {
        let session: ThreadSessionInfo = self.inner.request(method, params).await?;
        Ok(self.register_thread(session).await)
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

    #[tokio::test]
    async fn config_read_sends_object_params() {
        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(client_reader, client_writer));

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
        let initialized = read_json_line(&mut server_reader).await;
        assert_eq!(initialized["method"], "initialized");

        let read_config = tokio::spawn(async move {
            let result = codex.read_config().await;
            codex.close().await;
            result
        });
        let request = read_json_line(&mut server_reader).await;
        assert_eq!(request["method"], "config/read");
        assert_eq!(request["params"], serde_json::json!({}));
        write_json_line(
            &mut server_writer,
            serde_json::json!({"id": request["id"], "result": {"config": {}}}),
        )
        .await;

        assert!(read_config.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn initialize_error_cancels_io_tasks() {
        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(client_reader, client_writer));

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

        let codex = Codex::from_io(BufReader::new(client_read), client_write)
            .await
            .unwrap();
        let init = codex.initialization_result().unwrap();
        assert_eq!(init.user_agent, "ua");
        assert_eq!(init.codex_home, std::path::Path::new("/tmp/codex"));
        assert_eq!(init.platform_family, "unix");
        assert_eq!(init.platform_os, "macos");

        server_task.await.unwrap();
    }
}
