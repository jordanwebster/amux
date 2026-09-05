//! Native client bridge over the shared protocol and UI runtime.

mod runtime;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use amux::{AccessToken, AuthError, CredentialProvider, RelayConnection};
use runtime::{MobileRuntime, StartConfig, TokenSource};
use serde::Deserialize;
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
    fn send(&self, events: Value) {
        if let Ok(bytes) = CString::new(events.to_string()) {
            // The caller keeps its context alive until stop has joined this worker.
            unsafe { (self.function)(bytes.as_ptr(), self.context as *mut c_void) };
        }
    }
}

enum Control {
    Stop,
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
                        .send(json!([{"Connection": {"state":"disconnected", "reason":reason}}])),
                    Err(_) => {
                        callback.send(json!([{"Invariant": {"detail":"mobile worker panicked"}}]))
                    }
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
    let (requests, mut token_requests) = mpsc::channel(1);
    let credentials = Arc::new(Credentials {
        source: config.relay.token.clone(),
        requests,
        next_id: AtomicU64::new(1),
    });
    let mut runtime = MobileRuntime::open(&config, credentials).await?;
    let mut pending: HashMap<u64, oneshot::Sender<Result<AccessToken, AuthError>>> = HashMap::new();
    let mut last_fleet = Value::Null;
    let mut last_connection = RelayConnection::Connecting;
    callback.send(json!([{"Connection":{"state":"connecting", "reason":null}}]));
    loop {
        let mut events = Vec::new();
        tokio::select! {
            control = commands.recv() => match control {
                None | Some(Control::Stop) => break,
                Some(Control::TokenReply { request_id, reply }) => {
                    if let Some(waiter) = pending.remove(&request_id) { let _ = waiter.send(reply); }
                }
            },
            Some(request) = token_requests.recv() => {
                pending.retain(|_, sender| !sender.is_closed());
                pending.insert(request.id, request.reply);
                events.push(json!({"TokenRequest":{"request_id":request.id}}));
            },
            changed = runtime.relay.changed() => {
                if changed.is_err() { return Err("relay monitor closed".into()); }

            },
            active = runtime.ui.next() => { if !active { break; } },
        }
        // Observe relay state once for the whole batch, regardless of which
        // input woke us. Its Connection event must precede the Fleet it qualifies.
        let connection = runtime.relay.borrow_and_update().clone();
        if connection != last_connection {
            let (state, reason) = match &connection {
                RelayConnection::Connecting => ("connecting", None),
                RelayConnection::Connected => ("connected", None),
                RelayConnection::Disconnected { reason } => ("disconnected", Some(reason)),
            };
            events.push(json!({"Connection":{"state":state, "reason":reason}}));
            last_connection = connection.clone();
        }
        let model = runtime.ui.model();
        let fleet = json!({"Fleet": {
            "epoch":model.epoch(), "agents":model.agents().collect::<Vec<_>>(),
            "hosts":model.hosts().collect::<Vec<_>>(),
            "reconciled":model.is_synchronized() && connection == RelayConnection::Connected,
        }});
        if fleet != last_fleet {
            last_fleet = fleet.clone();
            events.push(fleet);
        }
        if !events.is_empty() {
            callback.send(Value::Array(events));
        }
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
