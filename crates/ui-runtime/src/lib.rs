//! Resource-owning execution shell for [`ui_state`].

#[cfg(test)]
mod test_allocator {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct CountingAllocator;

    static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: this allocator delegates every operation to the system
            // allocator with the caller's unchanged layout.
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
            // SAFETY: `pointer` was returned by the system allocator for this
            // layout and has not been deallocated yet.
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            // SAFETY: this forwards the allocation and layouts unchanged.
            let replacement = unsafe { System.realloc(pointer, layout, size) };
            if !replacement.is_null() {
                if size >= layout.size() {
                    LIVE_BYTES.fetch_add(size - layout.size(), Ordering::Relaxed);
                } else {
                    LIVE_BYTES.fetch_sub(layout.size() - size, Ordering::Relaxed);
                }
            }
            replacement
        }
    }

    pub(crate) fn live_bytes() -> usize {
        LIVE_BYTES.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: test_allocator::CountingAllocator = test_allocator::CountingAllocator;

mod recorder;
pub mod report;
mod runtime;
mod store_worker;

pub use recorder::{
    DEFAULT_RECORDER_CAPACITY, DEFAULT_RECORDER_MAX_BYTES, MSGS_SCHEMA_VERSION, Recorder,
    RecorderSnapshot, ReplayError, replay_msgs,
};
pub use runtime::{
    AttachmentClient, AttachmentClientFuture, AttachmentOpener, BUILD, ChatRetentionReport,
    ConnectFailure, ConnectFuture, Connector, Generation, HostEventStream, HostEventStreamFuture,
    HostInventory, LateResult, MAX_STREAM_BATCH, MsgTap, ProfileDirectory, ProfileEntry,
    ReportExtras, ReportExtrasProvider, Runtime, RuntimeGone, RuntimeOptions,
    RuntimeRetentionReport, ShellEdge, execute_put, execute_put_then_send, write_panic_report,
};
pub use store_worker::LOCAL_HOST_VIEW;
