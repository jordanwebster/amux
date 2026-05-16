//! Shared service context for protobuf-shaped application services.

mod agent;
mod client;
mod startup;

pub(crate) use agent::{
    AgentServiceCtx, AgentServiceState, SharedAgentServiceState, commit_server_suspend,
    prepare_server_suspend, resume_agents, shutdown_server, withdraw_agent,
};
pub(crate) use startup::{
    CloudRoutingService, StartedUserServices, establish_cloud_connection, start_user_services,
};
