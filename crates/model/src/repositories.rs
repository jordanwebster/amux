//! Project discovery: what a host tells a client about the directories an
//! agent can be started in.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::HostId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRepositoriesRequest {
    pub host: HostId,
    pub query: Option<String>,
    /// Maximum total entries, with recent projects first. Zero returns no entries.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRepositoriesResponse {
    pub recent: Vec<ProjectEntry>,
    pub repositories: Vec<ProjectEntry>,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub path: PathBuf,
    pub name: String,
    pub last_used: Option<DateTime<Utc>>,
}
