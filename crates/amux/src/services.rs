//! Shared service context for protobuf-shaped application services.

mod admin;
mod agent;
mod hook;
mod routing;

pub(crate) use admin::{AdminService, AdminServiceCtx};
pub(crate) use agent::{AgentService, AgentServiceCtx, SubscribeSessionCall};
pub(crate) use hook::{HookService, HookServiceCtx};
pub(crate) use routing::{RoutingService, RoutingServiceCtx, SubscribeRoutingEventsStartError};
