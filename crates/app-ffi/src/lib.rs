//! The C ABI over the shared app runtime and the embedded node owner.
//!
//! Everything here is pointer lifetime rules, JSON in and out, and one worker
//! thread. What the runtime does with a command and how the node is started
//! belong to the two crates underneath; nothing in either of them knows a C
//! type exists.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use app_embedded::{Embedded, StartConfig};
use app_runtime::cache::FleetCache;
use app_runtime::command::CommandDto;
use app_runtime::projection::Event;
use app_runtime::{
    Control, DisconnectReason, OpId, RelayConnection, Sink, Token, TokenError, compose,
};
use serde::Deserialize;
use tokio::sync::mpsc;

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

impl Sink for Callback {
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

/// Returns the bridge version as a NUL-terminated UTF-8 string.
/// The pointer remains valid for the process lifetime; do not free or modify it.
#[unsafe(no_mangle)]
pub extern "C" fn amux_app_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Returns the build of this library as a NUL-terminated UTF-8 string: the
/// version alone, or the version with `+debug-tools` when the library was
/// built with the driving tools compiled in. The suffix is a literal only the
/// debug-tools build contains, so an application binary can be inspected for
/// it to prove which of the two libraries it linked.
/// The pointer remains valid for the process lifetime; do not free or modify it.
#[unsafe(no_mangle)]
pub extern "C" fn amux_app_build() -> *const c_char {
    static BUILD: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    BUILD
        .get_or_init(|| CString::new(app_embedded::build()).expect("a version has no NUL"))
        .as_ptr()
}

/// Returns the fleet one account last displayed on this device, as an owned
/// JSON array of one Fleet event, or NULL when the directory holds nothing
/// readable for it. Free it with amux_app_free.
///
/// The application draws this before it has a connection, so the answer is the
/// same one the running library delivers first: every card marked as awaiting
/// its machine, and the fleet as a whole unreconciled. Reading it needs no
/// runtime and no network, so a cold launch can put rows on screen in its first
/// frame and start the connection afterwards.
///
/// The account has to be named because what a device remembers belongs to the
/// account that saw it: a launch that opens on a second account must draw that
/// account's machines and not the ones the first account left behind. An empty
/// account asks for what this device remembers with nobody signed in.
///
/// # Safety
/// cache_dir and account must be readable NUL-terminated UTF-8 strings for
/// this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_cached_fleet(
    cache_dir: *const c_char,
    account: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let directory = std::path::Path::new(unsafe { read_string(cache_dir) }?);
        let account = unsafe { read_string(account) }?;
        // A fleet is filed under the profile that saw it, and the profile's
        // identifier is the installation's to make, so the account is resolved
        // through what the last run recorded rather than used as a file name.
        let profile = app_runtime::cache::remembered_profile(
            directory,
            Some(account).filter(|account| !account.is_empty()),
        )?;
        owned(&[FleetCache::open(directory, profile).initial()])
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Starts asynchronously, returning NULL for invalid configuration or failure
/// to create the worker. Later failures arrive as Connection events.
///
/// # Safety
/// config_json must be a readable NUL-terminated UTF-8 string for this call.
/// on_events and ctx must remain valid until amux_app_stop returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_start(
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
            .name("amux-app".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let executor = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())?;
                    executor.block_on(serve(config, receive, &callback))
                }));
                match result {
                    Ok(Ok(())) => {}
                    // The worker is gone, so the device is offline and stays
                    // that way. The screen is told which kind of offline that
                    // is; the error itself goes out as a diagnostic, because
                    // it is a sentence for a log and not for a home screen.
                    Ok(Err(detail)) => callback.send(&[
                        Event::Invariant { detail },
                        Event::connection(&RelayConnection::Disconnected {
                            reason: DisconnectReason::Stopped,
                        }),
                    ]),
                    Err(_) => callback.send(&[Event::Invariant {
                        detail: "app worker panicked".into(),
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

/// The worker: open the embedded node, run the queue until told to stop, and
/// stop only what this worker started.
async fn serve(
    config: StartConfig,
    commands: mpsc::UnboundedReceiver<Control>,
    callback: &Callback,
) -> Result<(), String> {
    // One request channel for every account, and one counter, so a request
    // identifier means the same thing whichever account raised it and a reply
    // can be routed by that identifier alone.
    let (requests, token_requests) = mpsc::channel(1);
    let mut embedded = Embedded::open(&config, requests).await?;
    // Opening is what deletes a removed account's profile and caches, so this
    // is where the application learns which removals it may stop asking for.
    // One that would not delete is not named, and the next start is asked for
    // it again.
    let forgotten = std::mem::take(&mut embedded.forgotten);
    if !forgotten.is_empty() {
        callback.send(&[Event::Forgotten {
            accounts: forgotten,
        }]);
    }
    let served = app_runtime::run(
        &mut embedded.sessions,
        config.cache_dir.clone(),
        config.frame_interval(),
        commands,
        token_requests,
        callback,
    )
    .await;
    // Dropping the executor after this future cancels and drains all owned
    // transport tasks, including in-flight connection and token work.
    let _ = tokio::time::timeout(Duration::from_secs(1), embedded.shutdown()).await;
    served
}

/// Stops network work, cancels outstanding token requests, and joins the worker.
/// No callbacks can occur after return. NULL is accepted.
///
/// # Safety
/// The handle must be a live pointer returned by start, used once here, with
/// no concurrent calls. This function must not run from an event callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_stop(handle: *mut Handle) {
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

/// Enqueues a shared UI command or {"command":"subscribe","agent":"UUID"}
/// (also "unsubscribe"). Returns an owned operation UUID string; free it with
/// amux_app_free. Invalid JSON produces an asynchronous OpResult error.
/// NULL means the handle, string pointer, or worker is unavailable.
///
/// # Safety
/// handle must be live and command_json readable and NUL-terminated for this
/// call. Neither pointer may race stop. The input bytes are copied before return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_dispatch(
    handle: *mut Handle,
    command_json: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let json = unsafe { read_string(command_json) }?;
        let command = serde_json::from_str(json).map_err(|e| format!("invalid command: {e}"));
        unsafe { dispatch(handle, command) }
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Sends one command to the worker under a fresh operation id and returns
/// that id as an owned string.
unsafe fn dispatch(handle: &Handle, command: Result<CommandDto, String>) -> Option<*mut c_char> {
    let op = OpId(uuid::Uuid::new_v4());
    let result = CString::new(op.0.to_string()).ok()?;
    handle
        .commands
        .send(Control::Dispatch { op, command })
        .ok()?;
    Some(result.into_raw())
}

/// Updates callback cadence to the display's requested interval in nanoseconds.
/// Zero or intervals above one second are ignored. Changes take effect relative
/// to the last callback, including when a batch is already pending.
///
/// # Safety
/// handle must be live for this call and may not race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_set_frame_interval(handle: *mut Handle, interval_ns: u64) {
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

/// Says whether the app is in front of somebody.
///
/// Going away severs this device's link to the relay at once and stops it
/// dialling: a phone in a pocket is not a client with a network problem, and
/// leaving the socket for the system to freeze would leave every machine it
/// was watching holding a link nobody is reading. Coming back dials
/// immediately and the ordinary reconciliation follows.
///
/// # Safety
/// handle must be live for this call and may not race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_set_active(handle: *mut Handle, active: bool) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(handle) = unsafe { handle.as_ref() } {
            let _ = handle.commands.send(Control::Active(active));
        }
    }));
}

/// Hands the bridge every machine the platform's browser has resolved on this
/// network, as a JSON array of `{"host":UUID,"name":…,"version":N,"addrs":[…]}`.
///
/// The whole set each time, not a change to it: a browser reports what it can
/// currently see, and a machine that has gone is a machine missing from the
/// set rather than an event of its own. Handing over an empty array is how the
/// app says it can see nothing — the browser stopped, or the person refused
/// the local network — and the machines found earlier stop being offered.
///
/// Only the phone browses. Nothing in this library asks the system for the
/// network, because on iOS only the system may, so what this device has found
/// is exactly what was last handed to it.
///
/// # Safety
/// handle must be live and found_json readable and NUL-terminated for this
/// call. Neither pointer may race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_discovered(handle: *mut Handle, found_json: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let json = unsafe { read_string(found_json) }?;
        let found: Vec<FoundHostDto> = serde_json::from_str(json).ok()?;
        let found = found
            .into_iter()
            .filter_map(|host| host.into_found())
            .collect();
        handle.commands.send(Control::Discovered(found)).ok()
    }));
}

/// One machine as the platform's browser resolved it.
#[derive(serde::Deserialize)]
struct FoundHostDto {
    host: String,
    name: String,
    /// The protocol version the advertisement's own record claims.
    version: u32,
    addrs: Vec<String>,
}

impl FoundHostDto {
    /// The advertisement, or nothing where what was resolved cannot be dialled:
    /// an identity that is not a host id, or no address at all.
    fn into_found(self) -> Option<app_runtime::FoundHost> {
        let addrs: Vec<std::net::SocketAddr> = self
            .addrs
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect();
        if addrs.is_empty() {
            return None;
        }
        Some(app_runtime::FoundHost {
            host: self.host.parse().ok()?,
            name: self.name,
            version: self.version,
            addrs,
        })
    }
}

/// Freezes the shared reducer model as owned JSON; free with amux_app_free.
/// Returns NULL for an unavailable worker or a five-second timeout.
///
/// # Safety
/// handle must be live and may not race stop. Do not call from an event callback:
/// this function waits for the worker that delivers callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_snapshot(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::Snapshot) }
}

/// Freezes recorder checkpoint/message lines and obtains the embedded daemon's
/// JSON dump. The result has msgs, daemon and daemon_absent_reason fields.
/// A failed or timed-out dump is null with its reason; msgs remains available.
/// Free the owned result with amux_app_free.
///
/// In every build, not only the one with the driving tools: a person reporting
/// a problem from an installed app sends these records to their own account,
/// and the recorder they come from runs in every build anyway, because a
/// release panic report is written from it. It reads what this device already
/// holds and changes nothing, so it is no way to drive the app.
///
/// # Safety
/// handle must be live and may not race stop. Never call from an event callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_report_snapshot(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::ReportSnapshot) }
}

/// What this device's link to the relay has done, as owned JSON
/// `{"attempts":N,"shortened":M}`; free it with amux_app_free. NULL means
/// the handle or the worker was unavailable.
///
/// `attempts` is every dial since the runtime started. `shortened` is how many
/// of them happened early because somebody asked, which is the only
/// unambiguous evidence a Retry Now reached the connection: a dial at a relay
/// that is not there arrives nowhere to be counted, and the connection dials
/// on its own schedule anyway, so an attempt alone cannot tell a press apart
/// from the backoff coming round.
///
/// Debug-tools builds only.
///
/// # Safety
/// handle must be live and may not race stop. Never call from an event callback.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_relay_attempts(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::RelayAttempts) }
}

/// Reports one result on the shell edge of the account most recently switched
/// away from, as a task that was still running for it would. Returns owned
/// JSON `{"reported":true}`, or false when no account has been left yet.
///
/// Debug-tools builds only. Switching accounts cannot recall work already in
/// flight, and what a driver has to be able to prove is that such work is
/// refused rather than folded into the account the person moved to. There is
/// no way to produce it from outside the process.
///
/// # Safety
/// handle must be live and may not race stop. Never call from an event callback.
#[cfg(all(debug_assertions, feature = "debug-tools"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_late_from_previous(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::LateFromPrevious) }
}

/// How many results belonging to an earlier account this runtime has refused,
/// and which edges of the shell they came from, as owned JSON
/// `{"dropped":n,"kinds":["Inventory",…]}`. Debug-tools builds only.
///
/// # Safety
/// handle must be live and may not race stop. Never call from an event callback.
#[cfg(all(debug_assertions, feature = "debug-tools"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_late_results(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::LateResults) }
}

/// Pairs this device with the host a QR pairing payload names, over the relay
/// this runtime is already connected to. Returns owned JSON `{"host":"…"}` for
/// a peer now trusted, or `{"error":"…"}`; free it with amux_app_free. NULL
/// means the handle or the worker was unavailable, or the handshake did not
/// finish inside a minute.
///
/// Debug-tools builds only, and a harness affordance rather than the product
/// path: a person pairs a device by reading a code or following a link, and the
/// screens that do that carry their own confirmation step. A driver proving
/// what a paired device shows needs the trust without the screens, and needs it
/// before those screens exist.
///
/// # Safety
/// handle must be live and payload readable and NUL-terminated for this call.
/// Neither may race stop. Never call from an event callback.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_pair_qr(
    handle: *mut Handle,
    payload: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let payload = unsafe { read_string(payload) }?.to_owned();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        handle
            .commands
            .send(Control::PairLinkNow {
                payload,
                reply: send,
            })
            .ok()?;
        let json = receive.recv_timeout(Duration::from_secs(60)).ok()??;
        Some(CString::new(json).ok()?.into_raw())
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Folds a report's `msgs.jsonl` into the model it recorded and projects that
/// model as the event batch a running runtime would have delivered. Returns
/// owned JSON `{"events":[…]}`, or `{"error":"…"}` when the file cannot be
/// read or replayed; free it with amux_app_free.
///
/// Nothing is connected, nothing is started and no effect the recording asked
/// for is carried out. Debug-tools builds only.
///
/// # Safety
/// path must be a readable NUL-terminated UTF-8 string for this call.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_replay_report(path: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let path = unsafe { read_string(path) }?;
        let json = match compose::replay_report(std::path::Path::new(path)) {
            Ok(events) => serde_json::json!({ "events": events }),
            Err(error) => serde_json::json!({ "error": error }),
        };
        owned(&json)
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

/// Turns a written review into the token a composer holds: the canonical
/// attachment element to put in the message, and the artifact reference that
/// pins its frozen patch for whoever reads it.
///
/// Returns owned JSON `{"element":"…","attachment":{…}}`; free it with
/// amux_app_free. NULL means the request was not the document, artifact and
/// comments this needs.
///
/// # Safety
/// review_json must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_review_element(review_json: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let json = unsafe { read_string(review_json) }?;
        let token: compose::ReviewToken = serde_json::from_str(json).ok()?;
        owned(&compose::review_element(token))
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// The command a picked file becomes, or nothing where the description of it
/// was not readable.
///
/// Separate from the FFI entry so the bytes a command carries can be held
/// against what was picked without a live handle.
fn picked_command(request_json: &str, bytes: Vec<u8>) -> Option<app_runtime::Command> {
    let picked: compose::PickedAttachment = serde_json::from_str(request_json).ok()?;
    Some(compose::picked_command(picked, bytes))
}

/// Stores a picked file's bytes on the agent's host, returning an owned
/// operation UUID string; free it with amux_app_free. NULL means the
/// handle, the JSON, or the worker was unavailable.
///
/// The bytes travel here rather than inside a dispatched command for two
/// reasons: a command is JSON, and a photograph spelled as a JSON array of
/// numbers is four times its own size; and every dispatched command is written
/// into the local replay recording, which is not a place for somebody's
/// photograph. What the recording keeps is the artifact, never its contents.
///
/// # Safety
/// handle must be live and request_json readable and NUL-terminated for this
/// call; bytes must point at len readable bytes. No pointer may race stop.
/// Everything is copied before return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_attach(
    handle: *mut Handle,
    request_json: *const c_char,
    bytes: *const u8,
    len: usize,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let json = unsafe { read_string(request_json) }?;
        let bytes = match len {
            0 => Vec::new(),
            _ => {
                if bytes.is_null() {
                    return None;
                }
                unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec()
            }
        };
        let command = picked_command(json, bytes)?;
        unsafe { dispatch(handle, Ok(CommandDto::Shared(command))) }
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Spells one artifact attachment as the canonical element a message carries
/// it in, as owned JSON `{"element":"…"}`; free it with amux_app_free.
/// NULL means the request was not an artifact identity, kind, name and size.
///
/// # Safety
/// request_json must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_attachment_element(request_json: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let json = unsafe { read_string(request_json) }?;
        let request: compose::AttachmentRequest = serde_json::from_str(json).ok()?;
        owned(&serde_json::json!({ "element": compose::attachment_element(request) }))
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Routes a paste by size, as owned JSON; free it with amux_app_free.
/// `{"prose":"…"}` is text short enough to read in place; `{"element":"…",
/// "lines":n,"name":"…"}` is a paste long enough to bury the sentence around
/// it, which becomes one atomic Text attachment. NULL means the text was not
/// readable.
///
/// # Safety
/// text must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_paste(text: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let text = unsafe { read_string(text) }?;
        owned(&compose::paste(text))
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Splits message text into prose and the attachment elements it carries, as
/// owned JSON; free it with amux_app_free. NULL means the text was not
/// readable.
///
/// # Safety
/// text must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_attachments(text: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let text = unsafe { read_string(text) }?;
        owned(&compose::attachments(text))
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
pub unsafe extern "C" fn amux_app_free(string: *mut c_char) {
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
    /// What the account service said this account buys, where the reply that
    /// carried the token said. The bearer is opaque to this library, so this
    /// is the only place a tier can come from; absent is treated as free.
    tier: Option<app_runtime::Tier>,
    error: Option<String>,
}

/// Answers one TokenRequest with {"token":"…","expires_at":unix_seconds,
/// "tier":"free"|"pro"} (expiry and tier are optional) or {"error":"…"}.
/// Malformed replies fail that request; unknown, duplicate and expired request
/// IDs are ignored.
///
/// # Safety
/// handle must be live and token_json must be readable and NUL-terminated for
/// this call. The bytes are copied before return. Neither pointer may race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_app_token_reply(
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
                tier,
                error: None,
            }) if !bearer.is_empty() => {
                let expiry = expires_at
                    .map(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
                match expiry {
                    Some(None) => Err(TokenError::Provider("invalid token expiry".into())),
                    _ => Ok(Token {
                        bearer,
                        expires_at: expiry.flatten(),
                        tier,
                    }),
                }
            }
            Some(TokenReply {
                error: Some(error), ..
            }) => Err(TokenError::Provider(error)),
            _ => Err(TokenError::Provider("invalid token reply".into())),
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

/// A value as an owned C string the caller frees with amux_app_free.
fn owned<T: serde::Serialize>(value: &T) -> Option<*mut c_char> {
    Some(
        CString::new(serde_json::to_string(value).ok()?)
            .ok()?
            .into_raw(),
    )
}

#[cfg(test)]
mod tests;
