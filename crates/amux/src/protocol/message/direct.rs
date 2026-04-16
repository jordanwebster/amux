use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::ProtocolError;
use crate::protocol::route::Route;

/// Messages that are handled directly by the receiving server (no routing).
/// Used for peer-to-peer protocol messages after handshake.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DirectMessage {
    Reauth {
        token: String,
    },
    ReauthResult {
        error: Option<ProtocolError>,
    },
    Heartbeat,
    HeartbeatAck,
    /// Marks the end of the initial host/agent discovery snapshot for a connection.
    InitialSyncComplete,
    /// Advertise or refresh agent metadata for a known UUID.
    AnnounceAgent {
        agent_id: Uuid,
        host_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        agent_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_protocol: Option<String>,
        readonly: bool,
        args: Vec<String>,
        created_at: DateTime<Utc>,
    },
    WithdrawAgent {
        agent_id: Uuid,
    },
    AnnounceHost {
        id: Uuid,
        name: String,
        route: Route,
        version: String,
    },
    WithdrawHost {
        id: Uuid,
        route: Route,
    },
    #[serde(other)]
    Unknown,
}
