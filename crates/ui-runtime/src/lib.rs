//! Resource-owning execution shell for [`ui_state`].

mod recorder;
pub mod report;
mod runtime;

pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, MSGS_SCHEMA_VERSION, Recorder, RecorderSnapshot, ReplayError,
    replay_msgs,
};
pub use runtime::{
    AttachmentClient, AttachmentClientFuture, AttachmentOpener, BUILD, ConnectFailure,
    ConnectFuture, Connector, Generation, LateResult, MsgTap, ProfileDirectory, ProfileEntry,
    ReportExtras, ReportExtrasProvider, Runtime, RuntimeGone, RuntimeOptions, ShellEdge,
    execute_put_then_send, write_panic_report,
};
