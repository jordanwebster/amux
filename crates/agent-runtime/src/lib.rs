#![allow(clippy::result_large_err)]

pub mod agent_tools;
mod agents;
mod debug;
mod events;
mod host;
mod suspend;

pub use host::{AgentRuntime, AgentRuntimeFactory};

pub use model::{
    Agent, AgentEvent, AgentKind, AgentType, ArtifactRef, CreateAgentRequest, Protocol,
};

#[derive(Clone)]
struct Config {
    path: Option<std::path::PathBuf>,
    socket_path: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = std::env::temp_dir().join("amux-agent-runtime-test");
        Self {
            path: None,
            socket_path: data_dir.join("amux.sock"),
            data_dir,
        }
    }
}

mod config {
    pub(crate) use super::Config;
}

fn keymap_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("keymaps")
}
