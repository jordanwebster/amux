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
use wire::{BlobRef, Kind, Presence};

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
        };
        wire::Attachment { of: Some(of) }
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
