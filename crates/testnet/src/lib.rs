//! The many-daemons harness.
//!
//! A [`Topology`] declares hosts, the links between them and the agents
//! each runs; [`Net`] starts it as production daemons inside this process,
//! each on its own temporary installation with its own identity, trust
//! store, profile store and front door, running real `amux agent`
//! processes on the scripted fake providers. Policy timers on every daemon
//! run on one [`DrivenClock`] the specification advances; transports stay
//! on real time.
//!
//! The harness owns resources, verbs and observations, never scenarios:
//!
//! - **Verbs** change the world and return an [`Ack`] once the change is
//!   installed: sever and restore a link, kill, stop and restart a daemon,
//!   checkpoint a host and rewind it to that checkpoint under a new boot
//!   id, spawn and resume agents, advance policy time.
//! - **Observations** wait for consequences and return what they saw:
//!   [`Observer`] and [`InventoryObserver`] record Subscribe and
//!   SubscribeInventory streams exactly as a client receives them, and
//!   [`ObserverOf::observe_until`] fails on a stuck predicate, at its
//!   deadline or as soon as the stream ends, never passing by timing out.
//!   [`Net::assert_block_invariant`] checks a replica's rows against the
//!   origin's.
//! - **The door** ([`door::serve`]) serves a topology to a real client in
//!   another process with the same verbs, and publishes readiness only once
//!   the net is ready.
//!
//! Scenarios live in the tests that use these pieces.
//!
//! <!-- door-capabilities:start -->
//! | Door verb (`door::Control`) | Harness capability |
//! | --- | --- |
//! | `Sever` | `Net::sever_link` |
//! | `Restore` | `Net::restore_link` |
//! | `Link` | `Net::link_up` |
//! | `KillDaemon` | `Net::kill_daemon` |
//! | `StopDaemon` | `Net::stop_daemon` |
//! | `RestartDaemon` | `Net::kill_daemon when running + Net::restart_daemon` |
//! | `Checkpoint` | `Net::checkpoint_host` |
//! | `Rewind` | `Net::rewind_host` |
//! | `Advance` | `Net::advance` |
//! | `Spawn` | `Net::spawn` |
//! | `Resume` | `Net::resume` |
//! | `Send` | `Net::send` |
//! | `OpenGate` | `Net::open_gate` |
//! | `Inventory` | `Net::observe_inventory + observe_until(CaughtUp)` |
//! | `Block` | `Net::assert_block_invariant` |
//! | `Shutdown` | `Net::shutdown + closes the control socket` |
//! <!-- door-capabilities:end -->

mod binaries;
mod clock;
pub mod door;
mod invariant;
mod net;
pub mod observe;
mod topology;

pub use binaries::Binaries;
pub use clock::DrivenClock;
pub use invariant::BlockViolation;
pub use net::{
    Ack, AgentRef, ClockMode, EdgeHook, HostInfo, JournalCut, LaunchHook, Net, NetError,
    NetOptions, prompt, withdraw,
};
pub use observe::{InventoryObserver, Observer, ObserverOf, PATIENCE, Stuck};
pub use topology::{AgentDecl, FakeKind, HostDecl, LinkDecl, Topology, TopologyError};
