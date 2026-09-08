//! Native client bridge over the shared protocol and UI runtime.

mod cache;
pub mod projection;
mod runtime;

use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use amux::{AccessToken, AuthError, CredentialProvider, RelayConnection};
use amux_ui::{AgentId, Command, OpError, OpId, OpOutcome};
use projection::{
    AccountsOutcome, Cadence, ConnectionOutcome, CreationOutcome, DeviceIdentityDto, DevicesOutcome,
    Event, OpOutcomeDto, PairedDeviceDto, PairingOutcome, ProjectDto, Projection,
    SubscriptionOutcome,
};
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
    RelayAttempts(std::sync::mpsc::SyncSender<Option<String>>),
    #[cfg(feature = "debug-tools")]
    ReportSnapshot(std::sync::mpsc::SyncSender<Option<String>>),
    #[cfg(all(debug_assertions, feature = "debug-tools"))]
    LateFromPrevious(std::sync::mpsc::SyncSender<Option<String>>),
    #[cfg(all(debug_assertions, feature = "debug-tools"))]
    LateResults(std::sync::mpsc::SyncSender<Option<String>>),
    #[cfg(feature = "debug-tools")]
    PairQr {
        payload: String,
        reply: std::sync::mpsc::SyncSender<Option<String>>,
    },
    FrameInterval(Duration),
    /// Whether the app is in front of somebody.
    Active(bool),
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
    /// Which account the app is being asked for. A phone signed in twice has
    /// two rotating credentials, and a reply is worthless unless the request
    /// said which one it wanted.
    account: String,
    reply: oneshot::Sender<Result<AccessToken, AuthError>>,
}
struct Credentials {
    account: String,
    source: TokenSource,
    requests: mpsc::Sender<TokenRequest>,
    next_id: Arc<AtomicU64>,
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
                    .send(TokenRequest {
                        id,
                        account: self.account.clone(),
                        reply,
                    })
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

/// Returns the fleet one account last displayed on this device, as an owned
/// JSON array of one Fleet event, or NULL when the directory holds nothing
/// readable for it. Free it with amux_mobile_free.
///
/// The application draws this before it has a connection, so the answer is the
/// same one the running library delivers first: every card marked as awaiting
/// its machine, and the fleet as a whole unreconciled. Reading it needs no
/// runtime and no network, so a cold launch can put rows on screen in its first
/// frame and start the connection afterwards.
///
/// The account has to be named because what a phone remembers belongs to the
/// account that saw it: a launch that opens on a second account must draw that
/// account's machines and not the ones the first account left behind.
///
/// # Safety
/// cache_dir and account must be readable NUL-terminated UTF-8 strings for
/// this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_cached_fleet(
    cache_dir: *const c_char,
    account: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let directory = unsafe { read_string(cache_dir) }?;
        let account = unsafe { read_string(account) }?;
        let fleet = cache::FleetCache::open(std::path::Path::new(directory), account).initial();
        Some(
            CString::new(serde_json::to_string(&[fleet]).ok()?)
                .ok()?
                .into_raw(),
        )
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
                    // The worker is gone, so the phone is offline and stays
                    // that way. The screen is told which kind of offline that
                    // is; the error itself goes out as a diagnostic, because
                    // it is a sentence for a log and not for a home screen.
                    Ok(Err(detail)) => callback.send(&[
                        Event::Invariant { detail },
                        Event::connection(&RelayConnection::Disconnected {
                            reason: amux::DisconnectReason::Stopped,
                        }),
                    ]),
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
    Pairing(PairingCommand),
    Connection(ConnectionCommand),
    Devices(DevicesCommand),
    Creation(CreationCommand),
    Accounts(AccountsCommand),
    Shared(Command),
}

/// Something asked of the set of accounts this phone is signed in to.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum AccountsCommand {
    /// Read a different account. Every screen starts again from that account's
    /// own machines; nothing the previous one showed is carried across.
    Select { account: String },
}

/// Starting an agent on a machine, and finding somewhere to start it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum CreationCommand {
    /// What a machine has to offer as a working directory: the projects it was
    /// used in recently, the repositories under its roots, and the roots.
    ListRepositories {
        host: amux::HostId,
        #[serde(default)]
        query: Option<String>,
        /// A combined maximum, recent first. The host caps it; zero asks for
        /// the roots alone.
        limit: u32,
    },
    /// Start an agent on a machine, in a directory, under a named layer.
    CreateAgent {
        host: amux::HostId,
        directory: PathBuf,
        name: String,
        agent: NewAgent,
    },
}

/// Which layer a new agent runs under, said in full.
///
/// Claude's driver is a required field with no default anywhere on the way in.
/// This device drives Claude through the SDK, and the failure a default would
/// allow is silent: a request that left the driver unsaid would start a PTY
/// session that looks like every other agent until somebody tries to do
/// something only the SDK can do. Naming it costs one word and removes the
/// whole class.
#[derive(Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
enum NewAgent {
    Claude {
        driver: amux::ClaudeDriver,
    },
    Codex {
        #[serde(default)]
        model: Option<String>,
    },
}

impl NewAgent {
    fn agent_type(self) -> amux::AgentType {
        match self {
            NewAgent::Claude { driver } => amux::AgentType::Claude { driver },
            NewAgent::Codex { model } => amux::AgentType::Codex {
                model,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            },
        }
    }
}

/// The shared command a bridge creation asks for, so what the runtime hands
/// the client can be read without a running runtime.
fn creation(command: CreationCommand) -> Option<Command> {
    match command {
        CreationCommand::CreateAgent {
            host,
            directory,
            name,
            agent,
        } => Some(Command::CreateAgent {
            host: Some(host),
            name,
            agent_type: agent.agent_type(),
            working_dir: directory,
        }),
        CreationCommand::ListRepositories { .. } => None,
    }
}

/// Something asked of this device's own trust store.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum DevicesCommand {
    /// Stop trusting a machine. Every link this device holds to it is closed
    /// before the answer comes back, so nothing that was already open outlives
    /// the revocation.
    Revoke { host: amux::HostId },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum SubscriptionCommand {
    Subscribe { agent: AgentId },
    Unsubscribe { agent: AgentId },
}

/// Something asked of the phone's own link to the relay, rather than of a
/// machine on the other side of it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ConnectionCommand {
    /// Stop waiting out the backoff and dial the relay now.
    RetryNow,
}

/// One step of pairing this device with a machine.
///
/// Two phases, never one. Beginning authenticates the secret and answers with
/// the machine's own account of itself; nothing is trusted until a separate
/// confirmation naming that attempt arrives. A caller that begins and never
/// answers has paired with nobody — the attempt expires on the machine.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum PairingCommand {
    /// A six-digit code, against the one machine that issued it.
    BeginPairPin { host: amux::HostId, pin: String },
    /// The payload an `amux://pair` link carries, which names its own machine.
    BeginPairLink { payload: String },
    Confirm { pending: String },
    Abandon { pending: String },
}

/// A pairing step that finished on a task of its own, on its way back to the
/// event loop that dispatched it.
struct PairingDone {
    op: OpId,
    outcome: PairingOutcome,
    /// An authenticated attempt to hold until it is answered. The capability
    /// the machine issued stays in this process; what crosses the boundary is
    /// the key to this map.
    hold: Option<(String, amux::PendingPeer)>,
}

/// This device's identity and the machines it trusts, read off the runtime on
/// a task of its own because both are round trips.
struct DevicesRead {
    identity: DeviceIdentityDto,
    devices: Vec<PairedDeviceDto>,
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

/// Says whether the app is in front of somebody.
///
/// Going away severs this phone's link to the relay at once and stops it
/// dialling: a phone in a pocket is not a client with a network problem, and
/// leaving the socket for the system to freeze would leave every machine it
/// was watching holding a link nobody is reading. Coming back dials
/// immediately and the ordinary reconciliation follows.
///
/// # Safety
/// handle must be live for this call and may not race stop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_set_active(handle: *mut Handle, active: bool) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(handle) = unsafe { handle.as_ref() } {
            let _ = handle.commands.send(Control::Active(active));
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

/// What this phone's link to the relay has done, as owned JSON
/// `{"attempts":N,"shortened":M}`; free it with amux_mobile_free. NULL means
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
pub unsafe extern "C" fn amux_mobile_relay_attempts(handle: *mut Handle) -> *mut c_char {
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
pub unsafe extern "C" fn amux_mobile_late_from_previous(handle: *mut Handle) -> *mut c_char {
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
pub unsafe extern "C" fn amux_mobile_late_results(handle: *mut Handle) -> *mut c_char {
    unsafe { snapshot(handle, Control::LateResults) }
}

/// An agent nothing on the account being read has ever heard of. Folding it
/// would be visible in the fleet, which is what makes refusing it provable.
#[cfg(all(debug_assertions, feature = "debug-tools"))]
fn late_agent() -> amux::Agent {
    amux::Agent {
        id: uuid::Uuid::from_u128(0x1a7e),
        host_id: uuid::Uuid::from_u128(0x1a7f),
        name: Some("from-the-account-you-left".into()),
        command: "cat".into(),
        working_dir: PathBuf::from("/tmp"),
        kind: amux::AgentKind::TestAgent,
        readonly: false,
        args: Vec::new(),
        created_at: chrono::Utc::now(),
        parent: None,
        working_on: None,
    }
}

/// Pairs this device with the host a QR pairing payload names, over the relay
/// this runtime is already connected to. Returns owned JSON `{"host":"…"}` for
/// a peer now trusted, or `{"error":"…"}`; free it with amux_mobile_free. NULL
/// means the handle or the worker was unavailable, or the handshake did not
/// finish inside a minute.
///
/// Debug-tools builds only, and a harness affordance rather than the product
/// path: a person pairs a phone by reading a code or following a link, and the
/// screens that do that carry their own confirmation step. A driver proving
/// what a paired phone shows needs the trust without the screens, and needs it
/// before those screens exist.
///
/// # Safety
/// handle must be live and payload readable and NUL-terminated for this call.
/// Neither may race stop. Never call from an event callback.
#[cfg(feature = "debug-tools")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_pair_qr(
    handle: *mut Handle,
    payload: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { handle.as_ref() }?;
        let payload = unsafe { read_string(payload) }?.to_owned();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        handle
            .commands
            .send(Control::PairQr {
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

/// What a review page hands the composer: the frozen document, the artifact it
/// is, and the comments written on it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewToken {
    diff: amux_ui::ArtifactId,
    document: amux_ui::review::ReviewDocument,
    comments: Vec<amux_ui::review::ReviewComment>,
}

/// Turns a written review into the token a composer holds: the canonical
/// attachment element to put in the message, and the artifact reference that
/// pins its frozen patch for whoever reads it.
///
/// Returns owned JSON `{"element":"…","attachment":{…}}`; free it with
/// amux_mobile_free. NULL means the request was not the document, artifact and
/// comments this needs.
///
/// The element is formatted here rather than on the client for the same reason
/// the projection is: the review body frames its comment text by byte length
/// and escapes what would otherwise close the element, and a second spelling
/// of that would be a second thing to keep right.
///
/// # Safety
/// review_json must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_review_element(review_json: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let json = unsafe { read_string(review_json) }?;
        let token: ReviewToken = serde_json::from_str(json).ok()?;
        let review = amux_ui::review::Review::with_comments(
            token.document,
            token.diff,
            token.comments,
        );
        let (mention, attachment) = amux_ui::review_mention(&review);
        let reply = serde_json::json!({
            "element": amux_ui::format_mention(&mention),
            "attachment": attachment,
        });
        Some(
            CString::new(serde_json::to_string(&reply).ok()?)
                .ok()?
                .into_raw(),
        )
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// What a composer is attaching: an artifact already stored, named by its
/// identity, kind, display name and size.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentRequest {
    id: amux_ui::ArtifactId,
    kind: amux_ui::ArtifactKind,
    name: String,
    size: u64,
}

/// What the phone knows about a file it just picked, before anything is stored.
/// Its identity is not among them: content identity is computed here from the
/// bytes, so a client can never name an artifact by an identity it made up.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PickedAttachment {
    agent: AgentId,
    kind: amux_ui::ArtifactKind,
    name: String,
    mime: String,
}

/// The command a picked file becomes, or nothing where the description of it
/// was not readable.
///
/// Separate from the FFI entry so the bytes a command carries can be held
/// against what was picked without a live handle.
fn picked_command(request_json: &str, bytes: Vec<u8>) -> Option<amux_ui::Command> {
    let picked: PickedAttachment = serde_json::from_str(request_json).ok()?;
    Some(amux_ui::Command::PutAttachment {
        agent: picked.agent,
        attachment: amux_ui::DraftAttachment::from_bytes(
            picked.kind,
            picked.name,
            picked.mime,
            bytes,
        ),
    })
}

/// Stores a picked file's bytes on the agent's host, returning an owned
/// operation UUID string; free it with amux_mobile_free. NULL means the
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
pub unsafe extern "C" fn amux_mobile_attach(
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
        let op = OpId(uuid::Uuid::new_v4());
        let result = CString::new(op.0.to_string()).ok()?;
        handle
            .commands
            .send(Control::Dispatch {
                op,
                command: Ok(CommandDto::Shared(command)),
            })
            .ok()?;
        Some(result.into_raw())
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Spells one artifact attachment as the canonical element a message carries
/// it in, as owned JSON `{"element":"…"}`; free it with amux_mobile_free.
/// NULL means the request was not an artifact identity, kind, name and size.
///
/// Formatted here rather than on the client for the same reason a review is:
/// the element escapes what would close it early, and a second speller would
/// be a second thing to keep right.
///
/// # Safety
/// request_json must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_attachment_element(
    request_json: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let json = unsafe { read_string(request_json) }?;
        let request: AttachmentRequest = serde_json::from_str(json).ok()?;
        let kind = match request.kind {
            amux_ui::ArtifactKind::Image => amux_ui::MentionKind::Image { id: request.id },
            _ => amux_ui::MentionKind::File { id: request.id },
        };
        let element = amux_ui::format_mention(&amux_ui::Mention {
            kind,
            name: request.name,
            size: Some(request.size),
            path: None,
        });
        let reply = serde_json::json!({ "element": element });
        Some(
            CString::new(serde_json::to_string(&reply).ok()?)
                .ok()?
                .into_raw(),
        )
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Routes a paste by size, as owned JSON; free it with amux_mobile_free.
/// `{"prose":"…"}` is text short enough to read in place; `{"element":"…",
/// "lines":n,"name":"…"}` is a paste long enough to bury the sentence around
/// it, which becomes one atomic Text attachment. NULL means the text was not
/// readable.
///
/// Where the line sits is the shared library's answer, not the client's, so a
/// paragraph that becomes a token in the terminal becomes one on the phone.
///
/// # Safety
/// text must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_paste(text: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let text = unsafe { read_string(text) }?;
        let reply = match amux_ui::paste(text) {
            amux_ui::Pasted::Prose(body) => serde_json::json!({ "prose": body }),
            amux_ui::Pasted::Text { body, lines } => serde_json::json!({
                "element": amux_ui::format_mention(&amux_ui::text_mention(body, lines)),
                "lines": lines,
                "name": amux_ui::PASTED_NAME,
            }),
        };
        Some(
            CString::new(serde_json::to_string(&reply).ok()?)
                .ok()?
                .into_raw(),
        )
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Splits message text into prose and the attachment elements it carries, as
/// owned JSON; free it with amux_mobile_free. NULL means the text was not
/// readable.
///
/// This is the parser the whole system reads attachments with. A composer asks
/// it what its own draft says so that what is drawn as a token is what will be
/// sent — anything it does not accept stays ordinary prose, which is what a
/// reader will see too.
///
/// # Safety
/// text must be a readable NUL-terminated UTF-8 string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_mobile_attachments(text: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let text = unsafe { read_string(text) }?;
        let segments = amux_ui::split_mentions(text);
        Some(
            CString::new(serde_json::to_string(&segments).ok()?)
                .ok()?
                .into_raw(),
        )
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
    // The remembered fleet belongs to the account on screen. Switching opens
    // the account moved to, so nothing the account left behind can be carried
    // into a fleet drawn under the new one's name.
    let mut cache = cache::FleetCache::open(&config.cache_dir, &config.active);
    callback.send(&[cache.initial()]);
    let mut cadence = Cadence::new(Duration::from_nanos(config.frame_interval_ns));
    cadence.emitted();
    // One request channel for every account, and one counter, so a request
    // identifier means the same thing whichever account raised it and a reply
    // can be routed by that identifier alone.
    let (requests, mut token_requests) = mpsc::channel(1);
    let next_id = Arc::new(AtomicU64::new(1));
    let mut runtime = MobileRuntime::open(&config, |account| {
        Arc::new(Credentials {
            account: account.id.clone(),
            source: account.token.clone(),
            requests: requests.clone(),
            next_id: next_id.clone(),
        })
    })
    .await?;
    // Every account that is not on screen folds its own subscription while the
    // app is in front of somebody, so the switcher can say which of them has
    // something waiting.
    let (counts, mut waiting) = mpsc::unbounded_channel();
    let mut watchers = runtime.watch_others(counts.clone());
    let mut watched: BTreeSet<String> = runtime.inactive_accounts();
    let mut attention: HashMap<String, usize> = HashMap::new();
    let mut pending: HashMap<u64, oneshot::Sender<Result<AccessToken, AuthError>>> = HashMap::new();
    // Authenticated pairing attempts waiting for a person to say yes. Held
    // here rather than handed to the app: the capability the machine issued is
    // what commits trust, so it never crosses the boundary.
    let mut pending_peers: HashMap<String, amux::PendingPeer> = HashMap::new();
    let (pairings, mut pairing_results) = mpsc::unbounded_channel::<PairingDone>();
    // This device's identity and its trusted machines, read off the runtime
    // rather than derived from the fleet: a machine that is away is still
    // trusted, and the fingerprint a person compares before revoking is not
    // something the inventory carries.
    let (devices_reads, mut devices_results) = mpsc::unbounded_channel::<DevicesRead>();
    let (revocations, mut revoked) = mpsc::unbounded_channel::<(OpId, DevicesOutcome)>();
    let (listings, mut listed) = mpsc::unbounded_channel::<(OpId, CreationOutcome)>();
    let mut devices: Option<DevicesRead> = None;
    let mut trusted: BTreeSet<amux::HostId> = BTreeSet::new();
    read_devices(&runtime, devices_reads.clone());
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
                Some(Control::RelayAttempts(reply)) => {
                    let _ = reply.send(
                        serde_json::to_string(&serde_json::json!({
                            "attempts": runtime.retry.attempts(),
                            "shortened": runtime.retry.shortened(),
                        }))
                        .ok(),
                    );
                }
                #[cfg(feature = "debug-tools")]
                Some(Control::PairQr { payload, reply }) => {
                    let admin = runtime.admin.clone();
                    tokio::spawn(async move {
                        let result = match amux::parse_qr_pairing_payload(&payload) {
                            Ok(payload) => admin
                                .pair_qr_cloud_peer(payload.host_id, payload.secret)
                                .await
                                .map(|peer| serde_json::json!({"host": peer.name}))
                                .unwrap_or_else(|error| {
                                    serde_json::json!({"error": error.to_string()})
                                }),
                            Err(error) => serde_json::json!({"error": error.to_string()}),
                        };
                        let _ = reply.send(serde_json::to_string(&result).ok());
                    });
                }
                #[cfg(feature = "debug-tools")]
                Some(Control::ReportSnapshot(reply)) => {
                    let snapshot = runtime.ui.recorder_snapshot();
                    let client = runtime.client();
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
                #[cfg(all(debug_assertions, feature = "debug-tools"))]
                Some(Control::LateFromPrevious(reply)) => {
                    // A result that was genuinely in flight for the account
                    // the user left, produced the only way one can be: on the
                    // shell edge that account's tasks still hold.
                    let edge = runtime.previous_edge.clone();
                    tokio::spawn(async move {
                        let reported = match edge {
                            Some(edge) => edge
                                .report(amux_ui::Msg::Server(amux_ui::ServerMsg::AgentUpserted {
                                    agent: late_agent(),
                                }))
                                .await
                                .is_ok(),
                            None => false,
                        };
                        let _ = reply.send(
                            serde_json::to_string(&serde_json::json!({"reported": reported})).ok(),
                        );
                    });
                }
                #[cfg(all(debug_assertions, feature = "debug-tools"))]
                Some(Control::LateResults(reply)) => {
                    let _ = reply.send(
                        serde_json::to_string(&serde_json::json!({
                            "dropped": runtime.ui.discarded_late_results(),
                            "kinds": runtime
                                .ui
                                .discarded_late_kinds()
                                .iter()
                                .map(|kind| format!("{kind:?}"))
                                .collect::<Vec<_>>(),
                        }))
                        .ok(),
                    );
                }
                Some(Control::FrameInterval(interval)) => cadence.set_interval(interval),
                Some(Control::Active(active)) => runtime.set_active(active),
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
                                    // The projection stops first, then the
                                    // runtime is told the interaction is over:
                                    // the phone holds a stream only because a
                                    // conversation was open, and one that has
                                    // been closed must not come back after
                                    // every reconnection for the rest of the
                                    // session.
                                    projection.unsubscribe(agent);
                                    runtime.ui.note_detached(agent);
                                    SubscriptionOutcome::Unsubscribed { agent }
                                }
                            };
                            events.push(Event::OpResult { op, outcome: OpOutcomeDto::Subscription(outcome) });
                        }
                        Ok(CommandDto::Pairing(command)) => {
                            pair(op, command, &runtime, &mut pending_peers, pairings.clone());
                        }
                        Ok(CommandDto::Devices(DevicesCommand::Revoke { host })) => {
                            revoke(op, host, &runtime, revocations.clone(), devices_reads.clone());
                        }
                        // Starting an agent is an ordinary shared command once
                        // the layer has been named, so it goes down the same
                        // path every other write does and its answer arrives
                        // as the same AgentCreated. What this arm adds is that
                        // the layer was named at all.
                        Ok(CommandDto::Creation(CreationCommand::CreateAgent {
                            host, directory, name, agent,
                        })) => {
                            let command = creation(CreationCommand::CreateAgent {
                                host, directory, name, agent,
                            })
                            .expect("a create names a shared command");
                            runtime.ui.dispatch_with_id(op, command);
                        }
                        Ok(CommandDto::Creation(CreationCommand::ListRepositories {
                            host, query, limit,
                        })) => {
                            list_repositories(op, host, query, limit, &runtime, listings.clone());
                        }
                        Ok(CommandDto::Accounts(AccountsCommand::Select { account })) => {
                            let outcome = match runtime.switch(&account) {
                                Ok(()) => {
                                    // The account now on screen is being read
                                    // rather than counted, and the one just
                                    // left is counted rather than read.
                                    watchers = runtime.watch_others(counts.clone());
                                    watched = runtime.inactive_accounts();
                                    attention.retain(|held, _| watched.contains(held));
                                    cache = cache::FleetCache::open(
                                        &config.cache_dir,
                                        &account,
                                    );
                                    devices = None;
                                    trusted.clear();
                                    projection = Projection::default();
                                    read_devices(&runtime, devices_reads.clone());
                                    AccountsOutcome::Selected { account }
                                }
                                Err(_) => AccountsOutcome::Unknown { account },
                            };
                            events.push(Event::OpResult {
                                op,
                                outcome: OpOutcomeDto::Accounts(outcome),
                            });
                        }
                        Ok(CommandDto::Connection(ConnectionCommand::RetryNow)) => {
                            // The connection is a loop of its own; this only
                            // interrupts the wait it is in. Whether that wait
                            // was actually shortened is the connection's to
                            // decide — asking twice in a second is one ask —
                            // so what comes back says the request was made,
                            // not that a relay answered.
                            runtime.retry.now();
                            events.push(Event::OpResult {
                                op,
                                outcome: OpOutcomeDto::Connection(ConnectionOutcome::RetryRequested),
                            });
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
                // Trust changed, so what This Phone lists did too. Pairing and
                // revocation both land here, and so does a machine trusted
                // from somewhere else entirely.
                let now_trusted: BTreeSet<_> = runtime
                    .ui
                    .model()
                    .hosts()
                    .filter(|host| host.entry.trust_status == amux::HostTrustStatus::Trusted)
                    .map(|host| host.entry.id)
                    .collect();
                if now_trusted != trusted {
                    trusted = now_trusted;
                    read_devices(&runtime, devices_reads.clone());
                }
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
            Some((op, outcome)) = revoked.recv() => {
                events.push(Event::OpResult { op, outcome: OpOutcomeDto::Devices(outcome) });
                dirty = true;
            },
            Some((op, outcome)) = listed.recv() => {
                events.push(Event::OpResult { op, outcome: OpOutcomeDto::Creation(outcome) });
                dirty = true;
            },
            Some(read) = devices_results.recv() => {
                if devices.as_ref().is_none_or(|held| {
                    held.identity != read.identity || held.devices != read.devices
                }) {
                    events.push(Event::Devices {
                        identity: read.identity.clone(),
                        devices: read.devices.clone(),
                    });
                    devices = Some(read);
                    dirty = true;
                }
            },
            Some(done) = pairing_results.recv() => {
                if let Some((id, peer)) = done.hold { pending_peers.insert(id, peer); }
                events.push(Event::OpResult { op: done.op, outcome: OpOutcomeDto::Pairing(done.outcome) });
                dirty = true;
            },
            Some(request) = token_requests.recv() => {
                pending.retain(|_, sender| !sender.is_closed());
                pending.insert(request.id, request.reply);
                events.push(Event::TokenRequest {
                    request_id: request.id,
                    account: request.account.clone(),
                });
            },
            changed = runtime.relay.changed() => {
                if changed.is_err() { return Err("relay monitor closed".into()); }
                dirty = true;
            },
            active = runtime.ui.next_message() => {
                if !active { break; }
                dirty = true;
            },
            Some((account, count)) = waiting.recv() => {
                // Nothing from an account nobody is reading reaches a screen.
                // The one thing it can say is how many of its agents are
                // waiting — and a count from a fold that has already been put
                // down, because its account is now the one on screen, says
                // nothing about the account it names any more.
                if watched.contains(&account) && attention.get(&account) != Some(&count) {
                    attention.insert(account.clone(), count);
                    events.push(Event::Attention { account, waiting: count });
                    dirty = true;
                }
            },
        }
        projection.outcomes(runtime.ui.model(), &mut events);
    }
    drop(pending);
    // Dropping the executor after this future cancels and drains all owned
    // transport tasks, including in-flight connection and token work.
    drop(watchers);
    let _ = tokio::time::timeout(Duration::from_secs(1), runtime.shutdown()).await;
    Ok(())
}

/// Reads this device's identity and the machines it trusts, off the loop.
///
/// A read that fails says nothing rather than emptying the list: the trust
/// store is on this device and a momentary failure to read it is not a person
/// losing their machines, and a section that blanked itself would invite
/// pairing again with everything still paired.
fn read_devices(runtime: &MobileRuntime, reads: mpsc::UnboundedSender<DevicesRead>) {
    let admin = runtime.admin.clone();
    tokio::spawn(async move {
        if let Some(read) = current_devices(&admin).await {
            let _ = reads.send(read);
        }
    });
}

/// What the trust store holds right now, or nothing where it could not be read.
async fn current_devices(admin: &amux::ProfileAdmin) -> Option<DevicesRead> {
    let identity = admin.device_identity().await.ok()?;
    let mut devices: Vec<_> = admin
        .list_peers()
        .await
        .ok()?
        .into_iter()
        .map(|peer| PairedDeviceDto {
            host: peer.host_id,
            name: peer.name,
            fingerprint: peer.fingerprint,
            paired_at: peer.paired_at,
        })
        .collect();
    // The store's order is its own; the list a person reads is theirs.
    devices.sort_by_key(|device| device.name.to_lowercase());
    Some(DevicesRead {
        identity: DeviceIdentityDto {
            host: identity.host_id,
            name: identity.name,
            fingerprint: identity.fingerprint,
        },
        devices,
    })
}

/// Withdraws trust from one machine, off the loop, and re-reads what is left.
///
/// The re-read is not an optimisation. Revoking closes the links to that
/// machine, which takes it out of the inventory as well, but the list this
/// screen shows is the trust store rather than the inventory, and only the
/// store knows the moment it stopped holding a key.
fn revoke(
    op: OpId,
    host: amux::HostId,
    runtime: &MobileRuntime,
    results: mpsc::UnboundedSender<(OpId, DevicesOutcome)>,
    reads: mpsc::UnboundedSender<DevicesRead>,
) {
    let admin = runtime.admin.clone();
    tokio::spawn(async move {
        let outcome = match admin.unpair(host, "revoked from the phone").await {
            Ok(peer) => DevicesOutcome::Revoked {
                host: peer.host_id,
                name: peer.name,
            },
            Err(_) => DevicesOutcome::RevokeRefused,
        };
        let _ = results.send((op, outcome));
        if let Some(read) = current_devices(&admin).await {
            let _ = reads.send(read);
        }
    });
}

/// Asks a machine what it has to offer as a working directory, off the loop.
///
/// Off the loop because it is a round trip to a machine that may be slow, and
/// because somebody is looking at a screen that has to keep drawing while they
/// type into its search box.
fn list_repositories(
    op: OpId,
    host: amux::HostId,
    query: Option<String>,
    limit: u32,
    runtime: &MobileRuntime,
    results: mpsc::UnboundedSender<(OpId, CreationOutcome)>,
) {
    let client = runtime.client();
    tokio::spawn(async move {
        let listing = client
            .list_repositories(amux::ListRepositoriesRequest { host, query, limit })
            .await;
        let outcome = match listing {
            Ok(listing) => CreationOutcome::Repositories {
                host,
                recent: listing.recent.iter().map(project).collect(),
                repositories: listing.repositories.iter().map(project).collect(),
                roots: listing
                    .roots
                    .iter()
                    .map(|root| root.to_string_lossy().into_owned())
                    .collect(),
            },
            Err(_) => CreationOutcome::RepositoriesUnavailable { host },
        };
        let _ = results.send((op, outcome));
    });
}

fn project(entry: &amux::ProjectEntry) -> ProjectDto {
    ProjectDto {
        path: entry.path.to_string_lossy().into_owned(),
        name: entry.name.clone(),
        last_used: entry.last_used,
    }
}

/// Runs one pairing step off the event loop and reports it back.
///
/// Off the loop because every step is a round trip to a machine that may be
/// slow or gone, and the loop is what keeps every other screen drawing.
fn pair(
    op: OpId,
    command: PairingCommand,
    runtime: &MobileRuntime,
    holding: &mut HashMap<String, amux::PendingPeer>,
    results: mpsc::UnboundedSender<PairingDone>,
) {
    let admin = runtime.admin.clone();
    // Confirming and abandoning both consume the attempt, so it leaves the map
    // before the round trip: a second tap on either has nothing to answer with
    // and says so rather than spending the capability twice.
    let held = match &command {
        PairingCommand::Confirm { pending } | PairingCommand::Abandon { pending } => {
            match holding.remove(pending) {
                Some(peer) => Some(peer),
                None => {
                    let _ = results.send(PairingDone {
                        op,
                        outcome: PairingOutcome::PairingLost,
                        hold: None,
                    });
                    return;
                }
            }
        }
        _ => None,
    };
    tokio::spawn(async move {
        let done = match command {
            PairingCommand::BeginPairPin { host, pin } => {
                began(op, admin.begin_pair_pin(host, &pin).await)
            }
            PairingCommand::BeginPairLink { payload } => {
                match amux::parse_qr_pairing_payload(&payload) {
                    Ok(payload) => began(op, admin.begin_pair_qr(&payload).await),
                    // A link that will not parse is refused in the same words
                    // a wrong code is: what an unreadable link proves about the
                    // machine that issued it is nothing.
                    Err(_) => PairingDone {
                        op,
                        outcome: PairingOutcome::PairingRefused,
                        hold: None,
                    },
                }
            }
            PairingCommand::Confirm { .. } => {
                let peer = held.expect("a confirm reaching here holds its attempt");
                let outcome = match admin.confirm_pair(peer).await {
                    Ok(peer) => PairingOutcome::Paired {
                        host: peer.host_id,
                        name: peer.name,
                    },
                    Err(_) => PairingOutcome::PairingRefused,
                };
                PairingDone {
                    op,
                    outcome,
                    hold: None,
                }
            }
            PairingCommand::Abandon { .. } => {
                let peer = held.expect("an abandon reaching here holds its attempt");
                // The machine is told, and the answer it gives is not a reason
                // to keep the attempt: this device wrote nothing either way.
                let _ = admin.abandon_pair(peer).await;
                PairingDone {
                    op,
                    outcome: PairingOutcome::PairingAbandoned,
                    hold: None,
                }
            }
        };
        let _ = results.send(done);
    });
}

/// The first phase's answer: the machine as it describes itself, kept under a
/// handle, or the one refusal every wrong secret shares.
fn began(op: OpId, result: Result<amux::PendingPeer, amux::PairingError>) -> PairingDone {
    match result {
        Ok(peer) => {
            let id = uuid::Uuid::new_v4().to_string();
            PairingDone {
                op,
                outcome: PairingOutcome::PairingPending {
                    pending: id.clone(),
                    host: peer.host_id,
                    name: peer.name.clone(),
                    fingerprint: peer.fingerprint.clone(),
                    expires_at: peer.expires_at,
                },
                hold: Some((id, peer)),
            }
        }
        // Mistyped, already used, expired, never issued, or the machine did
        // not answer: one shape for all of them. Telling them apart is exactly
        // what somebody guessing codes would want.
        Err(_) => PairingDone {
            op,
            outcome: PairingOutcome::PairingRefused,
            hold: None,
        },
    }
}

#[cfg(test)]
mod tests;
