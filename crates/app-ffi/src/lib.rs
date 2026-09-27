//! The C ABI the phone calls.
//!
//! One [`AmuxRuntime`] hosts the embedded profile runtime and the app
//! runtime's fleet on a Rust thread pool of its own; each open chat is an
//! [`AmuxChat`]. View values cross as JSON whose Swift mirrors are generated
//! from the Rust definitions (`cargo run -p xtask -- swift-types`), so the
//! two sides share one definition of every value.
//!
//! Rows are handed out by item key. A chat's id sequence changes only at
//! its two edges, newer keys above the newest the host holds and older ones
//! below its oldest, until a change batch says `reloaded`; there is no slot
//! identity to keep in step.
//!
//! Threads: every function may be called from the main thread. Reads return
//! at once. Acts that wait on the agent take a callback, called once on a
//! worker thread with a JSON result the callback borrows until it returns.
//! The wake is called on a worker thread whenever the fleet (chat id 0) or a
//! chat moved, at most once until the host takes that one's changes; the
//! host schedules the take on its main thread's next turn and returns.
//!
//! Every returned string is the caller's to free with `amux_string_free`,
//! and every byte buffer with `amux_bytes_free`. A null return means the
//! call failed; the reason goes to the log.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use app_embedded::{EdgeOverrides, EmbeddedRuntime, PairRequest, StartConfig};
use app_runtime::values::{ActOutcome, AgentAct, Draft, Found, NewAgent, RowOptions};
use app_runtime::{AppRuntime, Chat, Wake};
use model::{AgentKey, Key};
use node::SourcePolicy;
use serde::Serialize;
use serde::de::DeserializeOwned;
use ui_view::Pick;

// The tests drive real agent processes, which run on Unix.
#[cfg(all(test, unix))]
mod tests;

/// Called with a chat's id when it moved, or 0 when the fleet did.
pub type AmuxWake = extern "C" fn(context: *mut c_void, chat: u64);

/// Called once with an act's JSON result, borrowed until it returns.
pub type AmuxCallback = extern "C" fn(context: *mut c_void, json: *const c_char);

/// Bytes the caller frees with `amux_bytes_free`.
#[repr(C)]
pub struct AmuxBytes {
    pub data: *mut u8,
    pub len: usize,
}

/// The embedded runtime and its fleet.
pub struct AmuxRuntime {
    // Dropped last: every task below runs on it.
    tokio: Option<tokio::runtime::Runtime>,
    embedded: Option<Arc<EmbeddedRuntime>>,
    app: Option<Arc<AppRuntime>>,
    tail: u32,
}

/// One open chat.
pub struct AmuxChat {
    chat: Arc<Chat>,
    handle: tokio::runtime::Handle,
}

/// A host context pointer handed back on another thread.
#[derive(Clone, Copy)]
struct Context(*mut c_void);

// SAFETY: the pointer is the host's and only ever handed back to the
// host's own callback, which the host wrote to be called from any thread.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl AmuxRuntime {
    fn handle(&self) -> tokio::runtime::Handle {
        self.tokio.as_ref().expect("running").handle().clone()
    }

    fn app(&self) -> &Arc<AppRuntime> {
        self.app.as_ref().expect("running")
    }

    fn embedded(&self) -> &Arc<EmbeddedRuntime> {
        self.embedded.as_ref().expect("running")
    }

    /// Runs `act` on the pool and hands its JSON result to the callback.
    fn spawn<T, F>(&self, callback: AmuxCallback, context: *mut c_void, act: F)
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        spawn_on(&self.handle(), callback, context, act);
    }
}

fn spawn_on<T, F>(
    handle: &tokio::runtime::Handle,
    callback: AmuxCallback,
    context: *mut c_void,
    act: F,
) where
    T: Serialize,
    F: Future<Output = T> + Send + 'static,
{
    let context = Context(context);
    handle.spawn(async move {
        let result = act.await;
        let text = json(&result).unwrap_or_else(|| CString::new("null").unwrap());
        let context = context;
        callback(context.0, text.as_ptr());
    });
}

/// Starts the runtime with a pool of its own; blocks until the store is
/// open and the fleet has caught up with it.
pub fn start(
    config: &StartConfig,
    overrides: EdgeOverrides,
    wake: AmuxWake,
    context: *mut c_void,
) -> Result<Box<AmuxRuntime>, String> {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("amux")
        .enable_all()
        .build()
        .map_err(|error| format!("starting the runtime's threads: {error}"))?;
    let context = Context(context);
    let (embedded, app) = tokio.block_on(async {
        let embedded =
            EmbeddedRuntime::start_with(config, overrides, Arc::new(client::SystemClock))
                .await
                .map_err(|error| error.to_string())?;
        let host_wake = Arc::new(move |moved: Wake| {
            let context = context;
            let chat = match moved {
                Wake::Fleet => 0,
                Wake::Chat(id) => id,
            };
            wake(context.0, chat);
        });
        let app = AppRuntime::open(
            embedded.client(),
            embedded.clock(),
            embedded.host_id(),
            host_wake,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok::<_, String>((embedded, app))
    })?;
    Ok(Box::new(AmuxRuntime {
        tokio: Some(tokio),
        embedded: Some(Arc::new(embedded)),
        app: Some(Arc::new(app)),
        tail: config.tail,
    }))
}

fn overrides(config: &StartConfig) -> EdgeOverrides {
    let mut overrides = EdgeOverrides::default();
    // A driving build may keep direct links on loopback, so a simulator
    // run never listens on the machine's network.
    if cfg!(feature = "debug-tools")
        && let Some(bind) = config.lan_bind
    {
        overrides.lan_bind = bind;
    }
    overrides
}

// --- strings and JSON ------------------------------------------------------

fn json<T: Serialize>(value: &T) -> Option<CString> {
    let text = serde_json::to_string(value).ok()?;
    CString::new(text).ok()
}

fn owned<T: Serialize>(value: &T) -> *mut c_char {
    json(value).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Reads a NUL-terminated UTF-8 string.
///
/// # Safety
/// `text` is null or points to a NUL-terminated string that lives for the
/// call.
unsafe fn text<'a>(text: *const c_char) -> Option<&'a str> {
    if text.is_null() {
        return None;
    }
    // SAFETY: the caller's contract above.
    unsafe { CStr::from_ptr(text) }.to_str().ok()
}

/// # Safety
/// As for [`text`].
unsafe fn parse<T: DeserializeOwned>(value: *const c_char) -> Option<T> {
    // SAFETY: the caller's contract.
    serde_json::from_str(unsafe { text(value) }?).ok()
}

/// Runs a call, turning a panic into the failure value rather than letting
/// it unwind into the host.
fn guard<T>(failed: T, call: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => value,
        Err(_) => {
            #[cfg(test)]
            CAUGHT.with(|caught| caught.set(caught.get() + 1));
            failed
        }
    }
}

// A shipping build aborts on a panic instead of unwinding, so a panic the
// guard turns into a failure in a test is a crash on the phone: the tests
// count them on the calling thread.
#[cfg(test)]
thread_local! {
    static CAUGHT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// # Safety
/// `runtime` is null or a live pointer from `amux_runtime_start`.
unsafe fn live_runtime<'a>(runtime: *const AmuxRuntime) -> Option<&'a AmuxRuntime> {
    // SAFETY: the caller's contract.
    unsafe { runtime.as_ref() }
}

/// # Safety
/// `chat` is null or a live pointer from `amux_session_open`.
unsafe fn held_chat<'a>(chat: *const AmuxChat) -> Option<&'a AmuxChat> {
    // SAFETY: the caller's contract.
    unsafe { chat.as_ref() }
}

/// Runs a read on a chat inside its runtime, so a read that starts a fetch
/// can spawn it.
///
/// # Safety
/// As for [`held_chat`].
unsafe fn read<T: Serialize>(chat: *const AmuxChat, read: impl FnOnce(&Chat) -> T) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return std::ptr::null_mut();
        };
        let _entered = open.handle.enter();
        owned(&read(&open.chat))
    })
}

// --- the runtime -----------------------------------------------------------

/// This build's version, as a static string.
#[unsafe(no_mangle)]
pub extern "C" fn amux_version() -> *const c_char {
    static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let mut version = node::version().to_owned();
            if cfg!(feature = "debug-tools") {
                version.push_str("+debug-tools");
            }
            CString::new(version).unwrap()
        })
        .as_ptr()
}

/// Starts the runtime from a `StartConfig` as JSON. Blocks until the store
/// is open and the fleet has caught up. Null on failure, with the reason in
/// `error` when it is not null, which the caller frees.
///
/// # Safety
/// `config` is a NUL-terminated string; `error` is null or writable. The
/// wake may be called from any thread until `amux_runtime_stop` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_start(
    config: *const c_char,
    wake: AmuxWake,
    context: *mut c_void,
    error: *mut *mut c_char,
) -> *mut AmuxRuntime {
    let started = guard(
        Err("the runtime panicked while starting".to_owned()),
        || {
            // SAFETY: the caller's contract.
            let config: StartConfig = unsafe { parse(config) }
                .ok_or_else(|| "the start configuration is not a StartConfig".to_owned())?;
            start(&config, overrides(&config), wake, context)
        },
    );
    match started {
        Ok(runtime) => Box::into_raw(runtime),
        Err(reason) => {
            if !error.is_null() {
                // SAFETY: the caller's contract.
                unsafe {
                    *error = CString::new(reason).map_or(std::ptr::null_mut(), CString::into_raw)
                };
            }
            std::ptr::null_mut()
        }
    }
}

/// Stops the runtime: flushes the store and joins its threads. Close every
/// chat first.
///
/// # Safety
/// `runtime` is null or from `amux_runtime_start`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_stop(runtime: *mut AmuxRuntime) {
    if runtime.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    let mut runtime = unsafe { Box::from_raw(runtime) };
    guard((), || {
        let tokio = runtime.tokio.take().expect("running");
        tokio.block_on(async {
            drop(runtime.app.take());
            // An act still in flight holds the embedded runtime; dropped
            // with it, the store is left for the next start's recovery.
            if let Some(embedded) = runtime.embedded.take().and_then(Arc::into_inner) {
                let _ = embedded.shutdown().await;
            }
        });
        tokio.shutdown_timeout(std::time::Duration::from_secs(5));
    });
}

/// `listed` in the foreground: every listed agent keeps a source. Not
/// `listed` when a push woke the app in the background: only the chats
/// that open keep one.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_set_source_policy(runtime: *const AmuxRuntime, listed: bool) {
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(runtime) = unsafe { live_runtime(runtime) } {
            // Switching opens and closes sources, which run on the pool.
            let _entered = runtime.handle().enter();
            runtime.embedded().set_source_policy(if listed {
                SourcePolicy::Listed
            } else {
                SourcePolicy::OnDemand
            });
        }
    });
}

#[derive(Serialize)]
enum Answered<T> {
    Ok(T),
    Err(String),
}

impl<T, E: std::fmt::Display> From<Result<T, E>> for Answered<T> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Answered::Ok(value),
            Err(error) => Answered::Err(error.to_string()),
        }
    }
}

/// Writes a dump of the profile with the fleet's and every open chat's
/// part; the callback gets `{"Ok": "<bundle directory>"}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `reason` is a NUL-terminated
/// string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_dump(
    runtime: *const AmuxRuntime,
    reason: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let reason = unsafe { text(reason) }.unwrap_or_default().to_owned();
        let app = runtime.app().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(app.dump(&reason).await)
        });
    });
}

/// Reaches and authenticates a machine by a `PairRequest` as JSON; the
/// callback gets `{"Ok": PendingPair}` or `{"Err": ..}`. Nothing is trusted
/// until `amux_runtime_confirm_pair`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `request` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_begin_pair(
    runtime: *const AmuxRuntime,
    request: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let request: Option<PairRequest> = unsafe { parse(request) };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            let Some(request) = request else {
                return Answered::Err("the request is not a PairRequest".into());
            };
            Answered::from(embedded.begin_pair(&request).await)
        });
    });
}

/// Trusts the machine a pending pairing reached, named by its token as a
/// JSON byte array; the callback gets `{"Ok": {"host_id": [..], "name":
/// ..}}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_runtime_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_confirm_pair(
    runtime: *const AmuxRuntime,
    token: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    #[derive(Serialize)]
    struct Paired {
        host_id: Vec<u8>,
        name: String,
    }
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.confirm_pair(&token).await.map(|peer| Paired {
                host_id: peer.host_id,
                name: peer.name,
            }))
        });
    });
}

/// Turns away the machine a pending pairing reached.
///
/// # Safety
/// As for `amux_runtime_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_abandon_pair(
    runtime: *const AmuxRuntime,
    token: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.abandon_pair(&token).await)
        });
    });
}

/// This device and the machines it trusts; the callback gets
/// `{"Ok": Roster}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_roster(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.roster().await)
        });
    });
}

/// Hands over the whole set the phone's browser found, as `[Found]`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `found` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_discovered(
    runtime: *const AmuxRuntime,
    found: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        if let Some(found) = unsafe { parse::<Vec<Found>>(found) } {
            let _entered = runtime.handle().enter();
            runtime.embedded().discovered(found);
        }
    });
}

/// The account the profile is bound to; the callback gets
/// `{"Ok": AccountView}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_account(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.account().await)
        });
    });
}

/// A bearer for the account service, which the profile refreshes; the
/// callback gets `{"Ok": Bearer}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_access_token(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.access_token().await)
        });
    });
}

/// Starts an agent from a `NewAgent` as JSON; the callback gets
/// `{"Ok": AgentKey}` or `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `agent` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_create_agent(
    runtime: *const AmuxRuntime,
    agent: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let agent: Option<NewAgent> = unsafe { parse(agent) };
        let app = runtime.app().clone();
        runtime.spawn(callback, context, async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not a NewAgent".into());
            };
            Answered::from(app.create_agent(&agent).await)
        });
    });
}

/// Where a host, by its id as a JSON byte array, offers to start an agent,
/// matching `query` (or everything when it is null or empty), at most
/// `limit` of each; the callback gets `{"Ok": Directories}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; the strings are NUL-terminated
/// or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_directories(
    runtime: *const AmuxRuntime,
    host_id: *const c_char,
    query: *const c_char,
    limit: u32,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let query = unsafe { text(query) }.unwrap_or_default().to_owned();
        let app = runtime.app().clone();
        runtime.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.directories(&host_id, &query, limit).await)
        });
    });
}

/// Renames, stops or deletes an agent, named by its `AgentKey`, with an
/// `AgentAct`, both as JSON; the callback gets `{"Ok": null}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_agent_act(
    runtime: *const AmuxRuntime,
    agent: *const c_char,
    act: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let agent: Option<AgentKey> = unsafe { parse(agent) };
        // SAFETY: the caller's contract.
        let act: Option<AgentAct> = unsafe { parse(act) };
        let app = runtime.app().clone();
        runtime.spawn(callback, context, async move {
            let (Some(agent), Some(act)) = (agent, act) else {
                return Answered::Err("the agent or the act is not readable".into());
            };
            Answered::from(app.agent_act(&agent, &act).await)
        });
    });
}

/// Stops trusting a paired machine, by its host id as a JSON byte array.
///
/// # Safety
/// As for `amux_runtime_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_unpair(
    runtime: *const AmuxRuntime,
    host_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.unpair(&host_id).await)
        });
    });
}

/// Binds the profile to an account with the refresh token the app's
/// sign-in obtained as the OAuth client `client_id`; the relay link comes
/// up from there. The profile alone spends the token from then on.
///
/// # Safety
/// As for `amux_runtime_pair`; both strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_sign_in(
    runtime: *const AmuxRuntime,
    cloud_url: *const c_char,
    client_id: *const c_char,
    refresh_token: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        // SAFETY: the caller's contract.
        let url = unsafe { text(cloud_url) }.unwrap_or_default().to_owned();
        // SAFETY: the caller's contract.
        let client = unsafe { text(client_id) }.unwrap_or_default().to_owned();
        // SAFETY: the caller's contract.
        let token = unsafe { text(refresh_token) }
            .unwrap_or_default()
            .to_owned();
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.sign_in(&url, &client, &token).await.map(|_| ()))
        });
    });
}

/// Signs the profile out of its account.
///
/// # Safety
/// As for `amux_runtime_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_runtime_sign_out(
    runtime: *const AmuxRuntime,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return;
        };
        let embedded = runtime.embedded().clone();
        runtime.spawn(callback, context, async move {
            Answered::from(embedded.sign_out().await.map(|_| ()))
        });
    });
}

// --- the fleet -------------------------------------------------------------

/// # Safety
/// As for [`live_runtime`].
unsafe fn fleet_read<T: Serialize>(
    runtime: *const AmuxRuntime,
    read: impl FnOnce(&AppRuntime) -> T,
) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract.
        let Some(runtime) = (unsafe { live_runtime(runtime) }) else {
            return std::ptr::null_mut();
        };
        owned(&read(runtime.app()))
    })
}

/// The fleet as `[FleetRow]`, expanded under the roots whose agent ids
/// `expand` lists (a JSON array of byte arrays, or null).
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `expand` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_rows(
    runtime: *const AmuxRuntime,
    expand: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let expand: Vec<Vec<u8>> = unsafe { parse(expand) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { fleet_read(runtime, |app| app.fleet_rows(&expand)) }
}

/// One agent's `FleetCard`, or null JSON when it is not listed.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `agent` is an `AgentKey` as a
/// NUL-terminated JSON string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_card(
    runtime: *const AmuxRuntime,
    agent: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(agent) = (unsafe { parse::<AgentKey>(agent) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's contract.
    unsafe { fleet_read(runtime, |app| app.fleet_card(&agent)) }
}

/// The `FamilyHeader` over an agent's chat, or null JSON.
///
/// # Safety
/// As for `amux_fleet_card`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_family(
    runtime: *const AmuxRuntime,
    agent: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(agent) = (unsafe { parse::<AgentKey>(agent) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's contract.
    unsafe { fleet_read(runtime, |app| app.family_header(&agent)) }
}

/// Every host in the fleet as `[HostView]`, this device first.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_hosts(runtime: *const AmuxRuntime) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { fleet_read(runtime, AppRuntime::hosts) }
}

/// The fleet's `FleetChanges` since the last take; the next change wakes
/// the host again.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_fleet_take_changes(runtime: *const AmuxRuntime) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { fleet_read(runtime, AppRuntime::take_fleet_changes) }
}

// --- a chat ----------------------------------------------------------------

/// Opens a chat on a fleet agent, named by its `AgentKey` as JSON, with
/// `tail` rows (the configured tail when zero). Blocks until the snapshot
/// and the rows this device holds are applied, so the first read is
/// correct even with the agent's host away. Null on failure, with the
/// reason in `error` when it is not null.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `agent` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open(
    runtime: *const AmuxRuntime,
    agent: *const c_char,
    tail: u32,
    error: *mut *mut c_char,
) -> *mut AmuxChat {
    let opened = guard(Err("opening the chat panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let runtime = unsafe { live_runtime(runtime) }.ok_or("no runtime")?;
        // SAFETY: the caller's contract.
        let agent: AgentKey = unsafe { parse(agent) }.ok_or("the agent is not an AgentKey")?;
        let tail = if tail == 0 { runtime.tail } else { tail };
        let handle = runtime.handle();
        let chat = handle
            .block_on(runtime.app().open_chat(&agent, tail))
            .map_err(|error| error.to_string())?;
        Ok(AmuxChat { chat, handle })
    });
    match opened {
        Ok(chat) => Box::into_raw(Box::new(chat)),
        Err(reason) => {
            if !error.is_null() {
                // SAFETY: the caller's contract.
                unsafe {
                    *error = CString::new(reason).map_or(std::ptr::null_mut(), CString::into_raw)
                };
            }
            std::ptr::null_mut()
        }
    }
}

/// The id the wake names this chat by.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_id(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { held_chat(chat) }.map_or(0, |open| open.chat.id())
}

/// Closes a chat. Its wake stops; a callback already running still runs.
///
/// # Safety
/// `chat` is null or from `amux_session_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_close(chat: *mut AmuxChat) {
    if !chat.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(chat) };
        let _entered = open.handle.enter();
        drop(open);
    }
}

/// The held window's keys, oldest first: `[String]`.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_keys(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::keys) }
}

/// Keys newer than `newest`, oldest first; null JSON when `newest` is not
/// held, and the host reads every key again.
///
/// # Safety
/// `chat` is from `amux_session_open`; `newest` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_new_keys_above(
    chat: *const AmuxChat,
    newest: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let newest = unsafe { text(newest) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.keys_above(&newest)) }
}

/// Keys older than `oldest`, oldest first; null JSON when `oldest` is not
/// held.
///
/// # Safety
/// As for `amux_session_new_keys_above`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_new_keys_below(
    chat: *const AmuxChat,
    oldest: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let oldest = unsafe { text(oldest) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.keys_below(&oldest)) }
}

/// `[Row]` for these keys (a JSON array of strings), in order, skipping
/// keys not held; `options` is a `RowOptions` as JSON, or null.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_rows_for(
    chat: *const AmuxChat,
    keys: *const c_char,
    options: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let keys: Vec<Key> = unsafe { parse(keys) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    let options: RowOptions = unsafe { parse(options) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.rows_for(&keys, &options)) }
}

/// The head `AskCard`, or null JSON.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_ask_card(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::ask_card) }
}

/// The session `Strip`.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_strip(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::strip) }
}

/// The `ChatFrame`: phase, composer, activity, queue and outbox.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_frame(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::frame) }
}

/// The chat's `ChatChanges` since the last take; the next change wakes the
/// host again.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_take_changes(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::take_changes) }
}

/// Runs an act on the chat's pool and hands its JSON result back.
///
/// # Safety
/// `chat` is from `amux_session_open`.
unsafe fn act<T, F, Fut>(
    chat: *const AmuxChat,
    callback: AmuxCallback,
    context: *mut c_void,
    act: F,
) where
    T: Serialize,
    F: FnOnce(Arc<Chat>) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return;
        };
        spawn_on(&open.handle, callback, context, act(open.chat.clone()));
    });
}

/// Sends a `Draft` as JSON; the callback gets a `SendOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send(
    chat: *const AmuxChat,
    draft: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            match draft {
                Some(draft) => Answered::Ok(chat.send(&draft).await),
                None => Answered::Err("the draft is not a Draft".into()),
            }
        })
    }
}

/// Answers the head ask, named by its key, with the choice at `index` on
/// its card; `note` goes back with a choice that takes one. The callback
/// gets an `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    index: u32,
    note: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let note = unsafe { text(note) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            chat.answer_choice(&ask_key, index as usize, &note).await
        })
    }
}

/// Answers the head question ask with one `Pick` per question, as a JSON
/// array, and the optional note. The callback gets an `ActOutcome`.
///
/// # Safety
/// As for `amux_session_answer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_answer_questions(
    chat: *const AmuxChat,
    ask_key: *const c_char,
    picks: *const c_char,
    note: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let ask_key = unsafe { text(ask_key) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let picks: Option<Vec<Pick>> = unsafe { parse(picks) };
    // SAFETY: the caller's contract.
    let note = unsafe { text(note) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            match picks {
                Some(picks) => chat.answer_questions(&ask_key, &picks, &note).await,
                None => ActOutcome::Rejected("the picks are not a list of Pick".into()),
            }
        })
    }
}

/// An input id as a JSON byte array.
///
/// # Safety
/// `id` is null or a NUL-terminated string.
unsafe fn input_id(id: *const c_char) -> Vec<u8> {
    // SAFETY: the caller's contract.
    unsafe { parse(id) }.unwrap_or_default()
}

/// Takes a queued prompt back out of the queue. The callback gets an
/// `ActOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated
/// JSON byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_withdraw(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.withdraw(&id).await
        })
    }
}

/// Steers a queued prompt into the running turn.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_send_now(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.send_now(&id).await
        })
    }
}

/// Sends a not-confirmed input again under a new id; the callback gets a
/// `SendOutcome`, or null JSON when the input is not held.
///
/// # Safety
/// As for `amux_session_withdraw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resend(
    chat: *const AmuxChat,
    input_id: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.resend(&id).await
        })
    }
}

/// Forgets a not-confirmed input.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_discard(chat: *const AmuxChat, input_id: *const c_char) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.discard(&id)) };
}

/// Stops the agent's turn; the agent stays.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_interrupt(
    chat: *const AmuxChat,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            chat.interrupt().await
        })
    }
}

/// The exited composer's one tap: resumes the agent with a `Draft` as its
/// first prompt. The callback gets an `ActOutcome`; on anything but Done
/// the draft stays the composer's.
///
/// # Safety
/// As for `amux_session_send`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_resume(
    chat: *const AmuxChat,
    draft: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    let draft: Option<Draft> = unsafe { parse(draft) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, |chat| async move {
            match draft {
                Some(draft) => chat.resume_with(&draft).await,
                None => ActOutcome::Rejected("the draft is not a Draft".into()),
            }
        })
    }
}

/// Asks for up to `n` rows older than the oldest held; the callback gets a
/// `PageOutcome`, and the new keys arrive below the oldest.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_page_older(
    chat: *const AmuxChat,
    n: u32,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            chat.page_older(n).await
        })
    }
}

/// Stores bytes to attach to a prompt; the callback gets
/// `{"Ok": BlobRef}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `data` points to `len` bytes; the
/// strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_put_blob(
    chat: *const AmuxChat,
    data: *const u8,
    len: usize,
    name: *const c_char,
    mime: *const c_char,
    callback: AmuxCallback,
    context: *mut c_void,
) {
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    // SAFETY: the caller's contract.
    let name = unsafe { text(name) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let mime = unsafe { text(mime) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, callback, context, move |chat| async move {
            Answered::from(chat.put_blob(&name, &mime, bytes).await)
        })
    }
}

/// An attachment's bytes, by its hash as a JSON byte array, once fetched.
/// Empty until then: asking starts the fetch, and the rows that show it
/// change when it lands.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_blob(
    chat: *const AmuxChat,
    hash: *const c_char,
) -> AmuxBytes {
    let empty = AmuxBytes {
        data: std::ptr::null_mut(),
        len: 0,
    };
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard(empty, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return AmuxBytes {
                data: std::ptr::null_mut(),
                len: 0,
            };
        };
        let _entered = open.handle.enter();
        match open.chat.blob(&hash) {
            Some(bytes) => {
                let boxed: Box<[u8]> = bytes.to_vec().into_boxed_slice();
                let len = boxed.len();
                AmuxBytes {
                    data: Box::into_raw(boxed).cast(),
                    len,
                }
            }
            None => AmuxBytes {
                data: std::ptr::null_mut(),
                len: 0,
            },
        }
    })
}

// --- freeing ---------------------------------------------------------------

/// # Safety
/// `text` is null or a string this library returned, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_string_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { CString::from_raw(text) });
    }
}

/// # Safety
/// `bytes` came from this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_bytes_free(bytes: AmuxBytes) {
    if !bytes.data.is_null() {
        // SAFETY: the caller's contract: this is the boxed slice we leaked.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes.data, bytes.len)) });
    }
}
