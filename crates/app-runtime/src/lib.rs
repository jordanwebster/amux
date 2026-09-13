//! The app layer any rich amux client reuses: account-scoped sessions, the
//! projection from reducer state to typed presentation values, the fleet
//! cache, and the frame-coalesced event queue.
//!
//! Nothing here knows how a session came to exist. An embedded node hands
//! sessions in through the traits in [`session`]; a desktop app attached to a
//! running daemon does the same over its admin client. Presentation values
//! are plain Rust and JSON; no platform type appears here.

pub mod cache;
pub mod command;
pub mod compose;
pub mod projection;
pub mod queue;
pub mod session;

// The few values the C boundary spells without a runtime of its own.
pub use model::{DisconnectReason, RelayConnection, Tier};
pub use queue::{Control, Reply, Sink, Token, TokenError, TokenRequest, run};
pub use session::{
    AccountAdmin, FoundHost, Link, Places, Refusal, Session, Sessions, Waiting, Watchers,
};
pub use ui_runtime::{HostEventStream, HostEventStreamFuture, HostInventory};
pub use ui_state::{CloudState, Command, OpId};
