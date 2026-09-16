//! The frame-coalesced event queue between a client's accounts and its screen.
//!
//! One loop owns the account on screen, the folds of the accounts that are
//! not, and every round trip a screen asks for. What it emits is batches of
//! [`Event`]s, no more often than the display's frame interval, and every
//! batch is complete: the connection state precedes the fleet whose
//! reconciliation it qualifies, and an operation's result follows the
//! projection that already reflects it.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use client::PendingPeer;
use model::{HostId, HostTrustStatus, RelayConnection};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use ui_runtime::MSGS_SCHEMA_VERSION;
use ui_state::{Msg, OpError, OpId, OpOutcome, ServerMsg};

use crate::command::{
    AccountsCommand, CommandDto, ConnectionCommand, CreationCommand, DevicesCommand,
    PairingCommand, SubscriptionCommand, creation,
};
use crate::projection::{
    AccountsOutcome, Cadence, ConnectionOutcome, CreationOutcome, DeviceIdentityDto,
    DevicesOutcome, Event, OpOutcomeDto, PairedDeviceDto, PairingOutcome, ProjectDto, Projection,
    SubscriptionOutcome,
};
use crate::session::Sessions;

/// A routing token the application obtained for one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub bearer: String,
    pub expires_at: Option<SystemTime>,
}

/// Why the application could not supply a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// Nobody is signed in to the account, or the request went unanswered.
    Unauthenticated,
    /// The application's account service said something went wrong.
    Provider(String),
}

/// A token the runtime is waiting on. Raised by whoever holds the account's
/// credentials; answered through [`Control::TokenReply`] with the same id.
pub struct TokenRequest {
    pub id: u64,
    /// Which account the application is being asked for. A client signed in
    /// twice has two rotating credentials, and a reply is worthless unless
    /// the request said which one it wanted.
    pub account: String,
    pub reply: oneshot::Sender<Result<Token, TokenError>>,
}

/// One-shot answers to a question asked of the loop from outside it.
pub type Reply = std::sync::mpsc::SyncSender<Option<String>>;

/// What the application can tell the loop.
pub enum Control {
    Stop,
    /// The shared reducer model, frozen as JSON.
    Snapshot(Reply),
    /// The recorder's checkpoint and message lines beside the embedded
    /// node's own dump: what a report bundle is made of.
    ReportSnapshot(Reply),
    /// What the active account's link has done: `{"attempts":N,"shortened":M}`.
    RelayAttempts(Reply),
    /// Report one result on the shell edge of the account most recently
    /// switched away from, as a task still running for it would; answers
    /// `{"reported":bool}`. There is no way to produce this from outside.
    LateFromPrevious(Reply),
    /// How many results belonging to an earlier account the runtime refused,
    /// and which edges they came from.
    LateResults(Reply),
    /// Pair with the machine a link payload names in one step. A harness
    /// affordance; a person pairs through screens that confirm.
    PairLinkNow {
        payload: String,
        reply: Reply,
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
        reply: Result<Token, TokenError>,
    },
}

/// Where each batch goes. Called on the loop's own task, never concurrently.
pub trait Sink {
    fn send(&self, events: &[Event]);
}

impl<F: Fn(&[Event])> Sink for F {
    fn send(&self, events: &[Event]) {
        self(events)
    }
}

/// A pairing step that finished on a task of its own, on its way back to the
/// loop that dispatched it.
struct PairingDone {
    op: OpId,
    outcome: PairingOutcome,
    /// An authenticated attempt to hold until it is answered. The capability
    /// the machine issued stays in this process; what crosses the boundary is
    /// the key to this map.
    hold: Option<(String, PendingPeer)>,
}

/// This device's identity and the machines it trusts, read off the account on
/// a task of its own because both are round trips.
struct DevicesRead {
    identity: DeviceIdentityDto,
    devices: Vec<PairedDeviceDto>,
}

/// Run the queue until told to stop or until the screen's runtime ends.
///
/// The remembered fleet belongs to the account on screen and comes from that
/// account's store. No fleet is projected until the runtime has installed
/// it: the application drew the same rows before starting, and an empty fleet
/// in the first frame would take them away again.
pub async fn run(
    sessions: &mut Sessions,
    frame_interval: Duration,
    mut commands: mpsc::UnboundedReceiver<Control>,
    mut token_requests: mpsc::Receiver<TokenRequest>,
    sink: &dyn Sink,
) -> Result<(), String> {
    let mut cadence = Cadence::new(frame_interval);
    cadence.emitted();
    // Every account that is not on screen folds its own subscription while the
    // app is in front of somebody, so the switcher can say which of them has
    // something waiting.
    let (counts, mut waiting) = mpsc::unbounded_channel();
    let mut watchers = sessions.watch_others(counts.clone());
    let mut watched: BTreeSet<String> = sessions.inactive_accounts();
    let mut attention: HashMap<String, usize> = HashMap::new();
    let mut pending: HashMap<u64, oneshot::Sender<Result<Token, TokenError>>> = HashMap::new();
    // Authenticated pairing attempts waiting for a person to say yes. Held
    // here rather than handed to the app: the capability the machine issued is
    // what commits trust, so it never crosses the boundary.
    let mut pending_peers: HashMap<String, PendingPeer> = HashMap::new();
    let (pairings, mut pairing_results) = mpsc::unbounded_channel::<PairingDone>();
    // This device's identity and its trusted machines, read off the account
    // rather than derived from the fleet: a machine that is away is still
    // trusted, and the fingerprint a person compares before revoking is not
    // something the inventory carries.
    let (devices_reads, mut devices_results) = mpsc::unbounded_channel::<DevicesRead>();
    let (revocations, mut revoked) = mpsc::unbounded_channel::<(OpId, DevicesOutcome)>();
    let (listings, mut listed) = mpsc::unbounded_channel::<(OpId, CreationOutcome)>();
    let mut devices: Option<DevicesRead> = None;
    let mut trusted: BTreeSet<HostId> = BTreeSet::new();
    read_devices(sessions, devices_reads.clone());
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
                    let _ = reply.send(serde_json::to_string(sessions.ui.model()).ok());
                }
                Some(Control::RelayAttempts(reply)) => {
                    let _ = reply.send(
                        serde_json::to_string(&json!({
                            "attempts": sessions.link.attempts(),
                            "shortened": sessions.link.shortened(),
                        }))
                        .ok(),
                    );
                }
                Some(Control::PairLinkNow { payload, reply }) => {
                    let admin = sessions.admin.clone();
                    tokio::spawn(async move {
                        let result = match admin.pair_link_now(payload).await {
                            Ok(host) => json!({"host": host}),
                            Err(error) => json!({"error": error}),
                        };
                        let _ = reply.send(serde_json::to_string(&result).ok());
                    });
                }
                Some(Control::ReportSnapshot(reply)) => {
                    let snapshot = sessions.ui.recorder_snapshot();
                    let client = sessions.client();
                    tokio::spawn(async move {
                        let (daemon, reason) = match tokio::time::timeout(
                            Duration::from_secs(3),
                            client.debug_dump(model::DebugFormat::Json),
                        )
                        .await
                        {
                            Ok(Ok(dump)) => (Some(dump), None),
                            Ok(Err(error)) => (None, Some(error.to_string())),
                            Err(_) => (None, Some("daemon dump timed out".to_owned())),
                        };
                        let result = json!({
                            "msgs": {"format_version": MSGS_SCHEMA_VERSION, "checkpoint": snapshot.checkpoint, "msgs": snapshot.msgs},
                            "daemon": daemon, "daemon_absent_reason": reason,
                        });
                        let _ = reply.send(serde_json::to_string(&result).ok());
                    });
                }
                Some(Control::LateFromPrevious(reply)) => {
                    // A result that was genuinely in flight for the account
                    // the user left, produced the only way one can be: on the
                    // shell edge that account's tasks still hold.
                    let edge = sessions.previous_edge.clone();
                    tokio::spawn(async move {
                        let reported = match edge {
                            Some(edge) => edge
                                .report(Msg::Server(ServerMsg::AgentUpserted {
                                    agent: late_agent(),
                                }))
                                .await
                                .is_ok(),
                            None => false,
                        };
                        let _ = reply.send(serde_json::to_string(&json!({"reported": reported})).ok());
                    });
                }
                Some(Control::LateResults(reply)) => {
                    let _ = reply.send(
                        serde_json::to_string(&json!({
                            "dropped": sessions.ui.discarded_late_results(),
                            "kinds": sessions
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
                Some(Control::Active(active)) => sessions.set_active(active),
                Some(Control::Dispatch { op, command }) => {
                    match command {
                        Ok(CommandDto::Shared(command)) => sessions.ui.dispatch_with_id(op, command),
                        Ok(CommandDto::Subscription(command)) => {
                            let outcome = match command {
                                SubscriptionCommand::Subscribe { agent } => {
                                    projection.subscribe(agent);
                                    sessions.ui.note_attached(agent);
                                    SubscriptionOutcome::Subscribed { agent }
                                }
                                SubscriptionCommand::Unsubscribe { agent } => {
                                    // The projection stops first, then the
                                    // runtime is told the interaction is over:
                                    // a client holds a stream only because a
                                    // conversation was open, and one that has
                                    // been closed must not come back after
                                    // every reconnection for the rest of the
                                    // session.
                                    projection.unsubscribe(agent);
                                    sessions.ui.note_detached(agent);
                                    SubscriptionOutcome::Unsubscribed { agent }
                                }
                            };
                            events.push(Event::OpResult { op, outcome: OpOutcomeDto::Subscription(outcome) });
                        }
                        Ok(CommandDto::Pairing(command)) => {
                            pair(op, command, sessions, &mut pending_peers, pairings.clone());
                        }
                        Ok(CommandDto::Devices(DevicesCommand::Revoke { host })) => {
                            revoke(op, host, sessions, revocations.clone(), devices_reads.clone());
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
                            sessions.ui.dispatch_with_id(op, command);
                        }
                        Ok(CommandDto::Creation(CreationCommand::ListRepositories {
                            host, query, limit,
                        })) => {
                            list_repositories(op, host, query, limit, sessions, listings.clone());
                        }
                        Ok(CommandDto::Accounts(AccountsCommand::Select { account })) => {
                            let outcome = match sessions.switch(&account) {
                                Ok(()) => {
                                    // The account now on screen is being read
                                    // rather than counted, and the one just
                                    // left is counted rather than read.
                                    watchers = sessions.watch_others(counts.clone());
                                    watched = sessions.inactive_accounts();
                                    attention.retain(|held, _| watched.contains(held));
                                    devices = None;
                                    trusted.clear();
                                    projection = Projection::default();
                                    read_devices(sessions, devices_reads.clone());
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
                            // decide (asking twice in a second is one ask),
                            // so what comes back says the request was made,
                            // not that a relay answered.
                            sessions.link.retry_now();
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
                let connection = sessions.relay.borrow_and_update().clone();
                if connection != last_connection {
                    events.push(Event::connection(&connection));
                    last_connection = connection.clone();
                }
                if !sessions.ui.remembered_fleet_pending() {
                    projection.collect(sessions.ui.model(), &connection, &mut events);
                }
                // Trust changed, so what This Device lists did too. Pairing and
                // revocation both land here, and so does a machine trusted
                // from somewhere else entirely.
                let now_trusted: BTreeSet<_> = sessions
                    .ui
                    .model()
                    .hosts()
                    .filter(|host| host.entry.trust_status == HostTrustStatus::Trusted)
                    .map(|host| host.entry.id)
                    .collect();
                if now_trusted != trusted {
                    trusted = now_trusted;
                    read_devices(sessions, devices_reads.clone());
                }
                if !events.is_empty() {
                    sink.send(&events);
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
            changed = sessions.relay.changed() => {
                if changed.is_err() { return Err("relay monitor closed".into()); }
                dirty = true;
            },
            active = sessions.ui.next_message() => {
                if !active { break; }
                dirty = true;
            },
            Some((account, count)) = waiting.recv() => {
                // Nothing from an account nobody is reading reaches a screen.
                // The one thing it can say is how many of its agents are
                // waiting, and a count from a fold that has already been put
                // down, because its account is now the one on screen, says
                // nothing about the account it names any more.
                if watched.contains(&account) && attention.get(&account) != Some(&count) {
                    attention.insert(account.clone(), count);
                    events.push(Event::Attention { account, waiting: count });
                    dirty = true;
                }
            },
        }
        projection.outcomes(sessions.ui.model(), &mut events);
    }
    drop(pending);
    drop(watchers);
    Ok(())
}

/// An agent nothing on the account being read has ever heard of. Folding it
/// would be visible in the fleet, which is what makes refusing it provable.
fn late_agent() -> model::Agent {
    model::Agent {
        id: uuid::Uuid::from_u128(0x1a7e),
        host_id: uuid::Uuid::from_u128(0x1a7f),
        name: Some("from-the-account-you-left".into()),
        command: "cat".into(),
        working_dir: PathBuf::from("/tmp"),
        kind: model::AgentKind::TestAgent,
        readonly: false,
        args: Vec::new(),
        created_at: chrono::Utc::now(),
        parent: None,
        working_on: None,
        summary: None,
        progress: None,
        inventory_revision: 0,
    }
}

/// Reads this device's identity and the machines it trusts, off the loop.
///
/// A read that fails says nothing rather than emptying the list: the trust
/// store is on this device and a momentary failure to read it is not a person
/// losing their machines, and a section that blanked itself would invite
/// pairing again with everything still paired.
fn read_devices(sessions: &Sessions, reads: mpsc::UnboundedSender<DevicesRead>) {
    let admin = sessions.admin.clone();
    tokio::spawn(async move {
        if let Some(read) = current_devices(&*admin).await {
            let _ = reads.send(read);
        }
    });
}

/// What the trust store holds right now, or nothing where it could not be read.
async fn current_devices(admin: &dyn crate::session::AccountAdmin) -> Option<DevicesRead> {
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
    host: HostId,
    sessions: &Sessions,
    results: mpsc::UnboundedSender<(OpId, DevicesOutcome)>,
    reads: mpsc::UnboundedSender<DevicesRead>,
) {
    let admin = sessions.admin.clone();
    tokio::spawn(async move {
        let outcome = match admin.unpair(host, "revoked from this device".into()).await {
            Ok(peer) => DevicesOutcome::Revoked {
                host: peer.host_id,
                name: peer.name,
            },
            Err(_) => DevicesOutcome::RevokeRefused,
        };
        let _ = results.send((op, outcome));
        if let Some(read) = current_devices(&*admin).await {
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
    host: HostId,
    query: Option<String>,
    limit: u32,
    sessions: &Sessions,
    results: mpsc::UnboundedSender<(OpId, CreationOutcome)>,
) {
    let client = sessions.client();
    tokio::spawn(async move {
        let listing = client
            .list_repositories(model::ListRepositoriesRequest { host, query, limit })
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

fn project(entry: &model::ProjectEntry) -> ProjectDto {
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
    sessions: &Sessions,
    holding: &mut HashMap<String, PendingPeer>,
    results: mpsc::UnboundedSender<PairingDone>,
) {
    let admin = sessions.admin.clone();
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
                began(op, admin.begin_pair_pin(host, pin).await)
            }
            PairingCommand::BeginPairLink { payload } => {
                began(op, admin.begin_pair_link(payload).await)
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
fn began(op: OpId, result: Result<PendingPeer, String>) -> PairingDone {
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
        // Mistyped, already used, expired, never issued, unreadable, or the
        // machine did not answer: one shape for all of them. Telling them
        // apart is exactly what somebody guessing codes would want.
        Err(_) => PairingDone {
            op,
            outcome: PairingOutcome::PairingRefused,
            hold: None,
        },
    }
}
