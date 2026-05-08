use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::Host;
use crate::protocol::route::Route;

/// Peer-scoped routing stream events.
///
/// These are carried in protobuf `PeerFrame` stream items for
/// `RoutingService.SubscribeRoutingEvents`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingEvent {
    SnapshotComplete,
    AgentUp {
        agent_id: Uuid,
        host_id: Uuid,
        name: Option<String>,
        command: String,
        working_dir: PathBuf,
        agent_type: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        io_protocols: Vec<String>,
        readonly: bool,
        args: Vec<String>,
        created_at: DateTime<Utc>,
    },
    AgentDown {
        agent_id: Uuid,
    },
    HostUp {
        host: Host,
        route: Route,
    },
    HostDown {
        id: Uuid,
        route: Route,
    },
    #[serde(other)]
    Unknown,
}

impl RoutingEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::SnapshotComplete => "Peer::SnapshotComplete",
            Self::AgentUp { .. } => "Peer::AgentUp",
            Self::AgentDown { .. } => "Peer::AgentDown",
            Self::HostUp { .. } => "Peer::HostUp",
            Self::HostDown { .. } => "Peer::HostDown",
            Self::Unknown => "Peer::Unknown",
        }
    }
}
