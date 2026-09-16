//! Resource-owning execution shell for [`ui_state`].

mod recorder;
pub mod report;
mod runtime;
mod store_worker;

pub use store_worker::LOCAL_HOST_VIEW;

pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, MSGS_SCHEMA_VERSION, Recorder, RecorderSnapshot, ReplayError,
    replay_msgs,
};
pub use runtime::{
    AttachmentClient, AttachmentClientFuture, AttachmentOpener, BUILD, ConnectFailure,
    ConnectFuture, Connector, Generation, HostEventStream, HostEventStreamFuture, HostInventory,
    LateResult, MsgTap, ProfileDirectory, ProfileEntry, ReportExtras, ReportExtrasProvider,
    Runtime, RuntimeGone, RuntimeOptions, ShellEdge, execute_put, execute_put_then_send,
    write_panic_report,
};
