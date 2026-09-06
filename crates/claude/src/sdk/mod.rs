//! Claude Code's bidirectional stream-JSON protocol.

pub mod abort;
pub mod control;
pub(crate) mod dispatch;
pub mod error;
pub mod init;
pub mod mcp;
pub mod message;
pub mod options;
mod process;
pub(crate) mod query;
pub mod session;
pub mod types;

pub use abort::AbortHandle;
pub use control::{
    BackgroundTaskSummary, InterruptResult, McpPermissionMode, McpPermissionModeOverrideResult,
    McpServerStatus, McpSetServersResult, PluginInfo, ReloadPluginsResult, ReloadSkillsResult,
    RewindFilesResult,
};
pub use error::{Error, ProtocolError};
pub use init::InitializationResult;
pub use mcp::{
    CreateSdkMcpServerOptions, SdkMcpServer, SdkMcpTool, SdkMcpToolCall, SdkMcpToolError,
    SdkMcpToolOptions, SdkMcpToolResult, create_sdk_mcp_server, tool,
};
pub use message::*;
pub use options::*;
pub use query::{ProcessExit, Termination, UserMessage};
pub use session::{
    Control, EventStream, PermissionSuggestion, RequestId, SdkEvent, Session, from_io,
};
pub use types::*;

/// Spawn Claude Code and return an initialized SDK session.
pub async fn spawn(options: QueryOptions) -> Result<Session, Error> {
    let (options, command) = prepare_query(options)?;
    let process = process::spawn_command(command)?;
    let warm =
        query::Query::warm_from_process(options, process, std::time::Duration::from_secs(60))
            .await?;
    Ok(session::from_query(warm.into_query()))
}

fn prepare_query(
    mut options: QueryOptions,
) -> Result<(QueryOptions, tokio::process::Command), Error> {
    options.validate()?;
    let session_id = query::query_session_id(&options);
    let command = process::query_command(&session_id, &options)?;
    // Forks without a requested target must use the same generated identity
    // in the CLI arguments and every subsequent streamed prompt.
    options.session_id = Some(session_id);
    Ok((options, command))
}
