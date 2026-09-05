//! Native client bridge over the shared protocol and UI runtime.

mod cache;
pub mod projection;
mod runtime;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use amux::{AccessToken, AuthError, CredentialProvider, RelayConnection};
use amux_ui::{AgentId, Command, OpError, OpId, OpOutcome};
use projection::{Cadence, Event, OpOutcomeDto, Projection, SubscriptionOutcome};
use runtime::{MobileRuntime, StartConfig, TokenSource};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// A borrowed, NUL-terminated UTF-8 JSON array, valid only during the callback.
/// Callbacks run serially on a Rust worker, may precede start's return, and
/// must return promptly. Copy the bytes before scheduling UI work. Do not stop
/// the runtime from its callback; stop joins this worker.
pub type EventCallback = unsafe extern "C" fn(events_json: *const c_char, ctx: *mut c_void);

/// Opaque runtime ownership. Every call using a handle must finish before stop.
pub struct Handle {
    commands: mpsc::UnboundedSender<Control>,
    worker: Option<JoinHandle<()>>,
}

struct Callback {
    function: EventCallback,
    context: usize,
}
impl Callback {
    fn send(&self, events: &[Event]) {
        if let Some(bytes) = serde_json::to_string(events)
            .ok()
            .and_then(|s| CString::new(s).ok())
        {
            // The caller keeps its context alive until stop has joined this worker.
            unsafe { (self.function)(bytes.as_ptr(), self.context as *mut c_void) };
        }
    }
}

enum Control {
    Stop,
    Snapshot(std::sync::mpsc::SyncSender<Option<String>>),
    #[cfg(feature = "debug-tools")]
    ReportSnapshot(std::sync::mpsc::SyncSender<Option<String>>),
    FrameInterval(Duration),
    Dispatch {
        op: OpId,
        command: Result<CommandDto, String>,
    },
    TokenReply {
        request_id: u64,
        reply: Result<AccessToken, AuthError>,
    },
}
struct TokenRequest {
    id: u64,
    reply: oneshot::Sender<Result<AccessToken, AuthError>>,
}
struct Credentials {
    source: TokenSource,
    requests: mpsc::Sender<TokenRequest>,
    next_id: AtomicU64,
}

#[async_trait::async_trait]
impl CredentialProvider for Credentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        match &self.source {
            TokenSource::Static(bearer) => Ok(AccessToken {
                bearer: bearer.clone(),
                expires_at: None,
            }),
            TokenSource::Callback => {
                let (reply, receive) = oneshot::channel();
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                self.requests
                    .send(TokenRequest { id, reply })
                    .await
                    .map_err(|_| AuthError::Unauthenticated)?;
                tokio::time::timeout(Duration::from_secs(30), receive)
                    .await
                    .map_err(|_| AuthError::Provider("token request timed out".into()))?
                    .map_err(|_| AuthError::Unauthenticated)?
            }
        }
    }
    fn invalidate(&self, _token: &AccessToken) {}
}

/// Returns the bridge version as a NUL-terminated UTF-8 string.
/// The pointer remains valid for the process lifetime; do not free or modify it.
#[unsafe(no_mangle)]
pub extern "C" fn amux_mobile_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Returns the build of this library as a NUL-terminated UTF-8 string: the
/// version alone, or the version with `+debug-tools` when the library was
/// built with the driving tools compiled in. The suffix is a literal only the
/// debug-tools build contains, so an application binary can be inspected for
/// it to prove which of the two libraries it linked.
/// The pointer remains valid for the process lifetime; do not free or modify it.
#[unsafe(no_mangle)]
pub extern "C" fn amux_mobile_build() -> *const c_char {
    #[cfg(feature = "debug-tools")]
    {
        concat!(env!("CARGO_PKG_VERSION"), "+debug-tools\0")
            .as_ptr()
            .cast()
    }
    #[cfg(not(feature = "debug-tools"))]
    {
        concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
    }
}

/// Starts asynchronously, returning NULL for invalid configuration or failure
/// to create the worker. Later failures arrive as Connection events.
///
/// # Safety
/// config_json must be a readable NUL-terminated UTF-8 string for this call.
/// on_events and ctx must remain valid until amux_mobile_stop returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_start(
    config_json: *const c_char,
    on_events: EventCallback,
    ctx: *mut c_void,
) -> *mut Handle {
    catch_unwind(AssertUnwindSafe(|| {
        let config: StartConfig =
            serde_json::from_str(unsafe { read_string(config_json) }?).ok()?;
        config.endpoint().ok()?;
        let (commands, receive) = mpsc::unbounded_channel();
        let callback = Callback {
            function: on_events,
            context: ctx as usize,
        };
        let worker = std::thread::Builder::new()
            .name("amux-mobile".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let executor = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())?;
                    executor.block_on(run(config, receive, &callback))
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(reason)) => callback
                        .send(&[Event::connection(&RelayConnection::Disconnected { reason })]),
                    Err(_) => callback.send(&[Event::Invariant {
                        detail: "mobile worker panicked".into(),
                    }]),
                }
            })
            .ok()?;
        Some(Box::into_raw(Box::new(Handle {
            commands,
            worker: Some(worker),
        })))
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Stops network work, cancels outstanding token requests, and joins the worker.
/// No callbacks can occur after return. NULL is accepted.
///
/// # Safety
/// The handle must be a live pointer returned by start, used once here, with
/// no concurrent calls. This function must not run from an event callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_stop(handle: *mut Handle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return;
        }
        let mut handle = unsafe { Box::from_raw(handle) };
        let _ = handle.commands.send(Control::Stop);
        if let Some(worker) = handle.worker.take() {
            let _ = worker.join();
        }
    }));
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum CommandDto {
    Subscription(SubscriptionCommand),
    Shared(Command),
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum SubscriptionCommand {
    Subscribe { agent: AgentId },
    Unsubscribe { agent: AgentId },
}

/// Enqueues a shared UI command or {"command":"subscribe","agent":"UUID"}
/// (also "unsubscribe"). Returns an owned operation UUID string; free it with
/// amux_mobile_free. Invalid JSON produces an asynchronous OpResult error.
/// NULL means the handle, string pointer, or worker is unavailable.
///
/// # Safety
/// handle must be live and command_json readable and NUL-terminated for this
/// call. Neither pointer may race stop. The input bytes are copied before return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_dispatch(
    handle: *mut Handle,
    command_json: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let json = unsafe { read_string(command_json) }?;
        let command = serde_json::from_str(json).map_err(|e| format!("invalid command: {e}"));
        let op = OpId(uuid::Uuid::new_v4());
        let result = CString::new(op.0.to_string()).ok()?;
        handle
            .commands
            .send(Control::Dispatch { op, command })
            .ok()?;
        Some(result.into_raw())
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Updates callback cadence to the display's requested interval in nanoseconds.
/// Zero or intervals above one second are ignored. Changes take effect relative
/// to the last callback, including when a batch is already pending.
///
/// # Safety
/// handle must be live for this call and may not race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_set_frame_interval(handle: *mut Handle, interval_ns: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if (1..=1_000_000_000).contains(&interval_ns)
            && let Some(handle) = unsafe { handle.as_ref() }
        {
            let _ = handle
                .commands
                .send(Control::FrameInterval(Duration::from_nanos(interval_ns)));
        }
    }));
}

/// Freezes the shared reducer model as owned JSON; free with amux_mobile_free.
/// Returns NULL for an unavailable worker or a five-second timeout.
///
/// # Safety
/// handle must be live and may not race stop. Do not call from an event callback:
/// this function waits for the worker that delivers callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_snapshot(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::Snapshot) }
}

/// Freezes recorder checkpoint/message lines and obtains the embedded daemon's
/// JSON dump. The result has msgs, daemon and daemon_absent_reason fields.
/// A failed or timed-out dump is null with its reason; msgs remains available.
/// Free the owned result with amux_mobile_free. Debug-tools builds only.
///
/// # Safety
/// handle must be live and may not race stop. Never call from an event callback.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_report_snapshot(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::ReportSnapshot) }
}

/// Folds a report's `msgs.jsonl` into the model it recorded and projects that
/// model as the event batch a running runtime would have delivered. Returns
/// owned JSON `{"events":[…]}`, or `{"error":"…"}` when the file cannot be
/// read or replayed; free it with amux_mobile_free.
///
/// Nothing is connected, nothing is started and no effect the recording asked
/// for is carried out: the reducer folds the recorded messages and the
/// projection reads the result. The connection is not part of a recording, so
/// the projection is told the relay is connected and reconciliation follows
/// what the recorded model itself synchronized to. Every agent in the model is
/// subscribed, so a replay carries every conversation the recording held
/// rather than only the fleet. Debug-tools builds only.
///
/// # Safety
/// path must be a readable NUL-terminated UTF-8 string for this call.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_replay_report(path: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if path.is_null() {
            return None;
        }
        let path = unsafe { CStr::from_ptr(path) }.to_str().ok()?;
        let json = match amux_ui::replay_msgs(std::path::Path::new(path)) {
            Ok(model) => {
                let mut projection = Projection::default();
                for card in model.agents() {
                    projection.subscribe(card.agent.id);
                }
                let mut events = Vec::new();
                projection.outcomes(&model, &mut events);
                projection.collect(&model, &RelayConnection::Connected, &mut events);
                serde_json::json!({ "events": events })
            }
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        };
        Some(
            CString::new(serde_json::to_string(&json).ok()?)
                .ok()?
                .into_raw(),
        )
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

unsafe fn snapshot(
    handle: *mut Handle,
    control: impl FnOnce(std::sync::mpsc::SyncSender<Option<String>>) -> Control,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        handle.commands.send(control(send)).ok()?;
        let json = receive.recv_timeout(Duration::from_secs(5)).ok()??;
        Some(CString::new(json).ok()?.into_raw())
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Releases a string returned by this library. NULL is accepted.
///
/// # Safety
/// The pointer must be an owned string returned by this library, freed once,
/// and not the borrowed version or callback string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_free(string: *mut c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !string.is_null() {
            drop(unsafe { CString::from_raw(string) });
        }
    }));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenReply {
    token: Option<String>,
    expires_at: Option<u64>,
    error: Option<String>,
}

/// Answers one TokenRequest with {"token":"…","expires_at":unix_seconds}
/// (expiry is optional) or {"error":"…"}. Malformed replies fail that request;
/// unknown, duplicate and expired request IDs are ignored.
///
/// # Safety
/// handle must be live and token_json must be readable and NUL-terminated for
/// this call. The bytes are copied before return. Neither pointer may race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_token_reply(
    handle: *mut Handle,
    request_id: u64,
    token_json: *const c_char,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return;
        };
        let reply = unsafe { read_string(token_json) }
            .and_then(|s| serde_json::from_str::<TokenReply>(s).ok());
        let reply = match reply {
            Some(TokenReply {
                token: Some(bearer),
                expires_at,
                error: None,
            }) if !bearer.is_empty() => {
                let expiry = expires_at
                    .map(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
                match expiry {
                    Some(None) => Err(AuthError::Provider("invalid token expiry".into())),
                    _ => Ok(AccessToken {
                        bearer,
                        expires_at: expiry.flatten(),
                    }),
                }
            }
            Some(TokenReply {
                error: Some(error), ..
            }) => Err(AuthError::Provider(error)),
            _ => Err(AuthError::Provider("invalid token reply".into())),
        };
        let _ = handle
            .commands
            .send(Control::TokenReply { request_id, reply });
    }));
}

unsafe fn read_string<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

async fn run(
    config: StartConfig,
    mut commands: mpsc::UnboundedReceiver<Control>,
    callback: &Callback,
) -> Result<(), String> {
    let mut cache = cache::FleetCache::open(&config.cache_dir);
    callback.send(&[cache.initial()]);
    let mut cadence = Cadence::new(Duration::from_nanos(config.frame_interval_ns));
    cadence.emitted();
    let (requests, mut token_requests) = mpsc::channel(1);
    let credentials = Arc::new(Credentials {
        source: config.relay.token.clone(),
        requests,
        next_id: AtomicU64::new(1),
    });
    let mut runtime = MobileRuntime::open(&config, credentials).await?;
    let mut pending: HashMap<u64, oneshot::Sender<Result<AccessToken, AuthError>>> = HashMap::new();
    let mut projection = Projection::default();
    let mut last_connection = RelayConnection::Connecting;
    let mut events = vec![Event::connection(&last_connection)];
    let mut dirty = true;
    loop {
        tokio::select! {
            // Service a due frame before draining another high-rate input.
            biased;
            control = commands.recv() => match control {
                None | Some(Control::Stop) => break,
                Some(Control::Snapshot(reply)) => {
                    let _ = reply.send(serde_json::to_string(runtime.ui.model()).ok());
                }
                #[cfg(feature = "debug-tools")]
                Some(Control::ReportSnapshot(reply)) => {
                    let snapshot = runtime.ui.recorder_snapshot();
                    let client = runtime.client.clone();
                    tokio::spawn(async move {
                        let (daemon, reason) = match tokio::time::timeout(Duration::from_secs(3), client.debug_dump(amux::DebugFormat::Json)).await {
                            Ok(Ok(dump)) => (Some(dump), None),
                            Ok(Err(error)) => (None, Some(error.to_string())),
                            Err(_) => (None, Some("daemon dump timed out".to_owned())),
                        };
                        let result = serde_json::json!({
                            "msgs": {"format_version": amux_ui::MSGS_SCHEMA_VERSION, "checkpoint": snapshot.checkpoint, "msgs": snapshot.msgs},
                            "daemon": daemon, "daemon_absent_reason": reason,
                        });
                        let _ = reply.send(serde_json::to_string(&result).ok());
                    });
                }
                Some(Control::FrameInterval(interval)) => cadence.set_interval(interval),
                Some(Control::Dispatch { op, command }) => {
                    match command {
                        Ok(CommandDto::Shared(command)) => runtime.ui.dispatch_with_id(op, command),
                        Ok(CommandDto::Subscription(command)) => {
                            let outcome = match command {
                                SubscriptionCommand::Subscribe { agent } => {
                                    projection.subscribe(agent);
                                    runtime.ui.note_attached(agent);
                                    SubscriptionOutcome::Subscribed { agent }
                                }
                                SubscriptionCommand::Unsubscribe { agent } => {
                                    projection.unsubscribe(agent);
                                    SubscriptionOutcome::Unsubscribed { agent }
                                }
                            };
                            events.push(Event::OpResult { op, outcome: OpOutcomeDto::Subscription(outcome) });
                        }
                        Err(message) => events.push(Event::OpResult { op, outcome: OpOutcomeDto::Shared(Box::new(OpOutcome::Error {
                            error: OpError::general(message),
                        })) }),
                    }
                    dirty = true;
                }
                Some(Control::TokenReply { request_id, reply }) => {
                    if let Some(waiter) = pending.remove(&request_id) { let _ = waiter.send(reply); }
                }
            },
            _ = tokio::time::sleep_until(cadence.deadline()), if dirty || !events.is_empty() => {
                // Observe relay state once for the whole batch. Connection must
                // precede the Fleet whose reconciliation it qualifies.
                let connection = runtime.relay.borrow_and_update().clone();
                if connection != last_connection {
                    events.push(Event::connection(&connection));
                    last_connection = connection.clone();
                }
                projection.collect(runtime.ui.model(), &connection, &mut events);
                let mut cache_errors = Vec::new();
                for event in &mut events {
                    if let Err(error) = cache.update(event, runtime.ui.model()) {
                        cache_errors.push(Event::Invariant { detail: format!("fleet cache write failed: {error}") });
                    }
                }
                events.extend(cache_errors);
                if !events.is_empty() {
                    callback.send(&events);
                    cadence.emitted();
                    events.clear();
                }
                dirty = false;
            },
            Some(request) = token_requests.recv() => {
                pending.retain(|_, sender| !sender.is_closed());
                pending.insert(request.id, request.reply);
                events.push(Event::TokenRequest { request_id: request.id });
            },
            changed = runtime.relay.changed() => {
                if changed.is_err() { return Err("relay monitor closed".into()); }
                dirty = true;
            },
            active = runtime.ui.next_message() => {
                if !active { break; }
                dirty = true;
            },
        }
        projection.outcomes(runtime.ui.model(), &mut events);
    }
    drop(pending);
    // Dropping the executor after this future cancels and drains all owned
    // transport tasks, including in-flight connection and token work.
    let _ = tokio::time::timeout(Duration::from_secs(1), runtime.client.shutdown()).await;
    drop(runtime);
    Ok(())
}

#[cfg(test)]
mod tests;
