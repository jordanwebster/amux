//! The values the phone bridge carries that are not views of one model:
//! what a host asks for and what an act came to. Like the views, they cross
//! as JSON and their Swift mirrors are generated from these definitions.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;

use model::{AgentKey, Connection, InputState, Key, PhaseView, Waiting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui_view::{Away, ComposerView, OutboxRow, QueuedRow, ToolRows};
use wire::{BlobRef, HostVia, Kind, Presence};

/// What an embedded runtime starts from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartConfig {
    /// The installation's directory: identity, trust, the store and reports.
    pub data_dir: PathBuf,
    /// What this device calls itself to the machines it pairs with.
    pub device_name: String,
    /// The runtime's own log, which a dump includes.
    #[serde(default)]
    pub log_path: Option<PathBuf>,
    /// Discovery candidates are listed only when their scope equals this.
    #[serde(default)]
    pub discovery_scope: String,
    /// Listen for and dial direct links on the local network.
    #[serde(default = "yes")]
    pub lan: bool,
    /// Where direct links listen, instead of every interface. Only a
    /// driving build reads it, to keep a simulator run on loopback.
    #[serde(default)]
    pub lan_bind: Option<SocketAddr>,
    /// Dial the relay's TCP carrier here in plaintext, instead of where the
    /// account service names it. Only a driving build reads it, to reach a
    /// served test relay.
    #[serde(default)]
    pub relay_tcp: Option<SocketAddr>,
    /// How many rows a chat opens with.
    #[serde(default = "default_tail")]
    pub tail: u32,
}

fn yes() -> bool {
    true
}

fn default_tail() -> u32 {
    crate::DEFAULT_TAIL
}

/// What changed in a chat since the host last took its changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatChanges {
    /// Rows to fetch again, each once.
    pub keys: Vec<Key>,
    /// Every row id may be new: read the keys again.
    pub reloaded: bool,
    /// Something beside the rows moved: the frame, the strip or the ask.
    pub session: bool,
}

/// Everything around a chat's rows that a screen draws at once.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ChatFrame {
    pub agent: AgentKey,
    pub name: String,
    pub kind: Kind,
    pub phase: PhaseView,
    pub composer: ComposerView,
    pub waiting: Option<Waiting>,
    pub connection: Connection,
    /// The rows are current with the agent's host.
    pub caught_up: bool,
    /// Older rows exist below the held window.
    pub has_older: bool,
    pub queue: Vec<QueuedRow>,
    pub outbox: Vec<OutboxRow>,
    /// The input answering the head ask, which a card that was not
    /// confirmed resends or discards.
    pub ask_input: Option<Vec<u8>>,
    /// The runtime no longer serves this chat: the agent is gone.
    pub ended: Option<String>,
}

/// How rows are asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RowOptions {
    pub tools: ToolRowsOption,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ToolRowsOption {
    #[default]
    ShowAll,
    Hide,
    /// A run collapses into its newest member unless one of its keys is
    /// here.
    CollapseRuns {
        expanded: Vec<Key>,
    },
}

impl RowOptions {
    /// The expanded set a collapsing view borrows, when there is one.
    pub fn expanded(&self) -> HashSet<Key> {
        match &self.tools {
            ToolRowsOption::CollapseRuns { expanded } => expanded.iter().cloned().collect(),
            ToolRowsOption::ShowAll | ToolRowsOption::Hide => HashSet::new(),
        }
    }

    pub fn tool_rows<'a>(&self, expanded: &'a HashSet<Key>) -> ToolRows<'a> {
        match &self.tools {
            ToolRowsOption::ShowAll => ToolRows::ShowAll,
            ToolRowsOption::Hide => ToolRows::Hide,
            ToolRowsOption::CollapseRuns { .. } => ToolRows::CollapseRuns { expanded },
        }
    }
}

/// A prompt as the composer holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Draft {
    pub text: String,
    /// In the order their placeholders appear in the text.
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DraftAttachment {
    /// Stored with put_blob first.
    Image(BlobRef),
    File(BlobRef),
    Text {
        name: String,
        text: String,
    },
    /// Comments on a frozen diff, sent with the diff that names its patch.
    Review {
        diff: wire::Diff,
        comments: Vec<wire::ReviewComment>,
    },
}

impl DraftAttachment {
    pub fn to_wire(&self) -> wire::Attachment {
        use wire::attachment::Of;
        let of = match self {
            DraftAttachment::Image(blob) => Of::Image(blob.clone()),
            DraftAttachment::File(blob) => Of::File(blob.clone()),
            DraftAttachment::Text { name, text } => Of::Text(wire::InlineText {
                name: name.clone(),
                text: text.clone(),
            }),
            DraftAttachment::Review { diff, comments } => Of::Review(wire::Review {
                diff: Some(diff.clone()),
                comments: comments.clone(),
            }),
        };
        wire::Attachment { of: Some(of) }
    }
}

/// An agent's working-tree diff as its host froze it for a review page, and
/// the patch it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrozenReview {
    pub diff: wire::Diff,
    pub patch: String,
}

impl FrozenReview {
    /// The page's document, with `comments` placed on their lines.
    pub fn doc(&self, comments: &[wire::ReviewComment]) -> ui_view::ReviewDoc {
        ui_view::review_doc(&self.diff, &self.patch, comments)
    }
}

/// What became of a prompt: its id, and its state as the chat shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendOutcome {
    pub input_id: Vec<u8>,
    pub state: InputState,
}

/// What an act on a chat came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActOutcome {
    Done,
    Rejected(String),
    /// The connection dropped before the agent answered.
    NotConfirmed,
    /// The call did not reach the agent.
    Failed(String),
}

/// What asking for older rows came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PageOutcome {
    Arrived(u32),
    /// Older history is held only by the agent's host, which cannot be
    /// reached; never an empty page.
    OriginUnreachable,
    Failed(String),
}

/// A host in the fleet as a screen lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HostView {
    pub host_id: Vec<u8>,
    pub name: String,
    /// This device.
    pub local: bool,
    pub trusted: bool,
    /// Found nearby and not paired.
    pub candidate: bool,
    pub presence: Presence,
    pub away: Away,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub last_dial_error: Option<String>,
    /// Where discovery found it; what pairing dials.
    pub addrs: Vec<String>,
    /// The route a live link runs over.
    pub via: HostVia,
    /// For this device: whether it is signed in to the account its profile
    /// is bound to; None while it was never bound.
    pub signed_in: Option<bool>,
}

/// What changed in the fleet since the host last took its changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FleetChanges {
    /// Agents whose card or family attention may differ.
    pub agents: Vec<AgentKey>,
    /// The host list moved.
    pub hosts: bool,
}

/// Pairing with a machine: the PIN its person reads out, or the link its
/// QR code carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PairRequest {
    Pin {
        /// Empty when pairing by address alone.
        #[serde(default)]
        host_id: Vec<u8>,
        pin: String,
        /// Where to dial it.
        addrs: Vec<String>,
    },
    Link(String),
}

/// An agent to start on a host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewAgent {
    pub host_id: Vec<u8>,
    pub kind: Kind,
    pub cwd: String,
    pub name: String,
    /// The provider's model; the host's default when absent.
    #[serde(default)]
    pub model: Option<String>,
}

/// A directory a host offers to start an agent in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Directory {
    pub path: String,
    pub name: String,
    pub last_used_ms: Option<i64>,
}

/// What a host offers to start an agent in: where agents ran lately and the
/// repositories under its roots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Directories {
    pub recent: Vec<Directory>,
    pub repositories: Vec<Directory>,
    pub roots: Vec<String>,
}

/// A change to one agent from outside its chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AgentAct {
    Rename(String),
    Stop,
    Delete,
}

/// A machine a pairing has reached and authenticated, before this device
/// trusts it: what the person compares and then accepts or turns away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingPair {
    /// Names this attempt to confirm or abandon it.
    pub token: Vec<u8>,
    pub host_id: Vec<u8>,
    pub name: String,
    /// The machine's key, as hex of its SHA-256.
    pub fingerprint: String,
    /// When the machine stops holding this attempt open.
    pub expires_at_ms: i64,
    pub via: HostVia,
}

/// This device as the machines it pairs with know it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Identity {
    pub host_id: Vec<u8>,
    pub name: String,
    pub fingerprint: String,
}

/// A machine this device trusts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairedPeer {
    pub host_id: Vec<u8>,
    pub name: String,
    pub fingerprint: String,
    pub paired_at_ms: i64,
}

/// This device and the machines it trusts, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Roster {
    pub identity: Identity,
    pub peers: Vec<PairedPeer>,
}

/// A machine the phone's own browser found on the local network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Found {
    pub host_id: Vec<u8>,
    pub name: String,
    pub version: u32,
    /// Resolved addresses as `ip:port`, IPv6 in brackets.
    pub addrs: Vec<String>,
    pub scope: String,
}

/// The account this device's profile is bound to, as the account screens
/// show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountView {
    pub binding: AccountBinding,
    pub email: String,
    pub name: String,
    /// Whether the account buys the relay, as the relay link last heard.
    pub pro: Option<bool>,
    pub relay: RelayLink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AccountBinding {
    /// Never signed in: this device works on its own network.
    Unbound,
    SignedIn,
    SignedOut,
    /// Signed in, with the relay link turned off.
    Paused,
}

/// The relay link, as far as a person needs to know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RelayLink {
    Off,
    Connecting,
    Connected,
    Retrying,
    /// The account service wants the person to sign in again.
    SignInAgain,
    /// The account service refuses this build as too old.
    UpdateRequired,
    Failed,
}

/// A bearer for the account service, borrowed from the profile, which alone
/// spends the refresh token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bearer {
    pub bearer: String,
    pub expires_at_ms: Option<i64>,
}
