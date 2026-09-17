//! Resource-owning execution shell for [`ui_state`].

mod recorder;
pub mod report;
mod runtime;
mod store_worker;

pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, DEFAULT_RECORDER_MAX_BYTES, MSGS_SCHEMA_VERSION, Recorder,
    RecorderSnapshot, RecorderSnapshotMode, ReplayError, replay_msgs,
};
pub use runtime::{
    AttachmentClient, AttachmentClientFuture, AttachmentOpener, BUILD, ChatRetentionReport,
    ConnectFailure, ConnectFuture, Connector, Generation, HostEventStream, HostEventStreamFuture,
    HostInventory, LateResult, MAX_STREAM_BATCH, MsgTap, ProfileDirectory, ProfileEntry,
    ReportExtras, ReportExtrasProvider, Runtime, RuntimeGone, RuntimeOptions,
    RuntimeRetentionReport, ShellEdge, execute_put, execute_put_then_send, write_panic_report,
};
pub use store_worker::LOCAL_HOST_VIEW;
