use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    pub experimental_api: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opt_out_notification_methods: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<InitializeCapabilities>,
}

/// Data returned by the Codex initialize handshake.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializationResult {
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub codex_home: PathBuf,
    #[serde(default)]
    pub platform_family: String,
    #[serde(default)]
    pub platform_os: String,
}
