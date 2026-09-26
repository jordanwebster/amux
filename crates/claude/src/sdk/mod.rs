//! Claude Code's bidirectional stream-JSON protocol: the frames, control
//! requests and option types as Claude reads and writes them.

pub mod control;
pub mod error;
pub mod init;
pub mod message;
pub mod options;
pub mod types;

pub use control::{
    BackgroundTaskSummary, InterruptResult, McpPermissionMode, McpPermissionModeOverrideResult,
    McpServerStatus, McpSetServersResult, PluginInfo, ReloadPluginsResult, ReloadSkillsResult,
    RewindFilesResult,
};
pub use error::{Error, ProtocolError};
pub use init::InitializationResult;
pub use message::*;
pub use options::*;
pub use types::*;
