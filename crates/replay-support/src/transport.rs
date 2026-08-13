use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream};

use crate::{IoDirection, IoEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplayTiming {
    #[default]
    Immediate,
    Recorded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplayOptions {
    pub timing: ReplayTiming,
}

#[derive(Debug, Clone)]
struct ReadEntry {
    us: u64,
    line: String,
}

#[derive(Debug)]
struct ReplayState {
    expected_writes: Vec<String>,
    read_groups: Vec<Vec<ReadEntry>>,
    validated_writes: usize,
    next_group_idx: usize,
    next_read_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayAdvance {
    Advanced { event_us: u64 },
    BlockedOnWrite,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayPeek {
    Ready { event_us: u64 },
    BlockedOnWrite,
    Exhausted,
}

#[derive(Debug, Clone)]
pub struct ReplayController {
    state: Arc<Mutex<ReplayState>>,
    writer: Arc<tokio::sync::Mutex<DuplexStream>>,
}

#[derive(Debug, Default, Clone)]
pub struct ReplayClock {
    inner: Arc<Mutex<ReplayClockState>>,
}

#[derive(Debug, Default)]
struct ReplayClockState {
    controllers: Vec<ReplayController>,
    current_us: Option<u64>,
}

/// Create a replay reader/writer pair from a script of IO events.
pub fn replay_transport(
    script: Vec<IoEvent>,
) -> (
    impl AsyncBufRead + Unpin + Send + 'static,
    impl AsyncWrite + Unpin + Send + 'static,
) {
    replay_transport_with_options(script, ReplayOptions::default())
}

/// Create a replay reader/writer pair with an explicit controller.
pub fn replay_transport_with_controller(
    script: Vec<IoEvent>,
    _options: ReplayOptions,
) -> (BufReader<DuplexStream>, ReplayWriter, ReplayController) {
    let (expected_writes, read_groups) = split_script(script);
    let state = Arc::new(Mutex::new(ReplayState {
        expected_writes,
        read_groups,
        validated_writes: 0,
        next_group_idx: 0,
        next_read_idx: 0,
    }));
    let (read_half, write_half) = tokio::io::duplex(1024 * 1024);
    let writer = Arc::new(tokio::sync::Mutex::new(write_half));
    let controller = ReplayController {
        state: Arc::clone(&state),
        writer,
    };

    (
        BufReader::new(read_half),
        ReplayWriter {
            state,
            line_buf: Vec::new(),
        },
        controller,
    )
}

/// Create a replay reader/writer pair with explicit replay options.
pub fn replay_transport_with_options(
    script: Vec<IoEvent>,
    options: ReplayOptions,
) -> (
    impl AsyncBufRead + Unpin + Send + 'static,
    impl AsyncWrite + Unpin + Send + 'static,
) {
    let (reader, writer, _controller) = replay_transport_with_controller(script, options);
    (reader, writer)
}

fn split_script(script: Vec<IoEvent>) -> (Vec<String>, Vec<Vec<ReadEntry>>) {
    let mut expected_writes = Vec::new();
    let mut read_groups: Vec<Vec<ReadEntry>> = Vec::new();
    let mut current_reads = Vec::new();

    for event in script {
        match event.direction {
            IoDirection::Write => {
                read_groups.push(std::mem::take(&mut current_reads));
                expected_writes.push(event.line);
            }
            IoDirection::Read => {
                current_reads.push(ReadEntry {
                    us: event.us,
                    line: event.line,
                });
            }
        }
    }

    read_groups.push(current_reads);
    (expected_writes, read_groups)
}

impl ReplayController {
    pub fn peek_next(&self) -> ReplayPeek {
        let state = self.state.lock().expect("replay state lock");
        peek_next_locked(&state)
    }

    pub async fn advance_one(&self) -> ReplayAdvance {
        let entry = {
            let mut state = self.state.lock().expect("replay state lock");
            match peek_next_locked(&state) {
                ReplayPeek::Ready { .. } => take_next_locked(&mut state),
                ReplayPeek::BlockedOnWrite => return ReplayAdvance::BlockedOnWrite,
                ReplayPeek::Exhausted => return ReplayAdvance::Exhausted,
            }
        };

        let Some(entry) = entry else {
            return ReplayAdvance::Exhausted;
        };

        let mut writer = self.writer.lock().await;
        if writer.write_all(entry.line.as_bytes()).await.is_err() {
            return ReplayAdvance::Exhausted;
        }
        if writer.write_all(b"\n").await.is_err() {
            return ReplayAdvance::Exhausted;
        }
        if writer.flush().await.is_err() {
            return ReplayAdvance::Exhausted;
        }

        ReplayAdvance::Advanced { event_us: entry.us }
    }
}

impl ReplayClock {
    pub fn new(start_us: Option<u64>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ReplayClockState {
                controllers: Vec::new(),
                current_us: start_us,
            })),
        }
    }

    pub fn register(&self, controller: ReplayController) {
        self.inner
            .lock()
            .expect("replay clock lock")
            .controllers
            .push(controller);
    }

    pub fn current_us(&self) -> Option<u64> {
        self.inner.lock().expect("replay clock lock").current_us
    }

    pub async fn advance_one(&self) -> ReplayAdvance {
        let (current_us, controllers) = {
            let inner = self.inner.lock().expect("replay clock lock");
            (inner.current_us, inner.controllers.clone())
        };

        if let Some(controller) = choose_due_controller(&controllers, current_us) {
            return controller.advance_one().await;
        }

        let controller = choose_next_controller(&controllers);

        let Some(controller) = controller else {
            return aggregate_status(&controllers);
        };

        let result = controller.advance_one().await;
        if let ReplayAdvance::Advanced { event_us } = result {
            let mut inner = self.inner.lock().expect("replay clock lock");
            inner.current_us = Some(
                inner
                    .current_us
                    .map_or(event_us, |current| current.max(event_us)),
            );
        }
        result
    }

    pub async fn advance_for(&self, duration: Duration) -> ReplayAdvance {
        let target_us = {
            let mut inner = self.inner.lock().expect("replay clock lock");
            let start_us = inner.current_us.unwrap_or(0);
            let delta_us = duration.as_micros().try_into().unwrap_or(u64::MAX);
            let target_us = start_us.saturating_add(delta_us);
            inner.current_us = Some(target_us);
            target_us
        };

        let mut last_result =
            aggregate_status(&self.inner.lock().expect("replay clock lock").controllers);
        loop {
            let controllers = self
                .inner
                .lock()
                .expect("replay clock lock")
                .controllers
                .clone();

            let Some(controller) = choose_due_controller(&controllers, Some(target_us)) else {
                return last_result;
            };

            let result = controller.advance_one().await;
            match result {
                ReplayAdvance::Advanced { .. } => {
                    last_result = result;
                }
                ReplayAdvance::BlockedOnWrite | ReplayAdvance::Exhausted => {
                    return result;
                }
            }
        }
    }
}

pub struct ReplayWriter {
    state: Arc<Mutex<ReplayState>>,
    line_buf: Vec<u8>,
}

impl AsyncWrite for ReplayWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        self.get_mut().line_buf.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();

        while let Some(pos) = this.line_buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = this.line_buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).to_string();
            let mut state = this.state.lock().expect("replay state lock");

            if state.validated_writes >= state.expected_writes.len() {
                panic!(
                    "unexpected replay write at index {}: {}",
                    state.validated_writes, line
                );
            }

            let write_idx = state.validated_writes;
            let expected = &state.expected_writes[write_idx];
            let actual_val = serde_json::from_str::<serde_json::Value>(&line);
            let expected_val = serde_json::from_str::<serde_json::Value>(expected);

            match (actual_val, expected_val) {
                (Ok(actual), Ok(expected)) => {
                    assert_eq!(
                        actual, expected,
                        "replay write mismatch at index {}",
                        write_idx
                    );
                }
                _ => {
                    assert_eq!(
                        line.trim(),
                        expected.trim(),
                        "replay write mismatch at index {}",
                        write_idx
                    );
                }
            }

            state.validated_writes += 1;
        }

        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

fn peek_next_locked(state: &ReplayState) -> ReplayPeek {
    let mut group_idx = state.next_group_idx;
    let mut read_idx = state.next_read_idx;

    loop {
        if group_idx >= state.read_groups.len() {
            return ReplayPeek::Exhausted;
        }
        if group_idx > state.validated_writes {
            return ReplayPeek::BlockedOnWrite;
        }

        let group = &state.read_groups[group_idx];
        if read_idx >= group.len() {
            group_idx += 1;
            read_idx = 0;
            continue;
        }

        return ReplayPeek::Ready {
            event_us: group[read_idx].us,
        };
    }
}

fn take_next_locked(state: &mut ReplayState) -> Option<ReadEntry> {
    loop {
        if state.next_group_idx >= state.read_groups.len() {
            return None;
        }
        if state.next_group_idx > state.validated_writes {
            return None;
        }

        let group = &state.read_groups[state.next_group_idx];
        if state.next_read_idx >= group.len() {
            state.next_group_idx += 1;
            state.next_read_idx = 0;
            continue;
        }

        let entry = group[state.next_read_idx].clone();
        state.next_read_idx += 1;
        return Some(entry);
    }
}

fn choose_next_controller(controllers: &[ReplayController]) -> Option<ReplayController> {
    controllers
        .iter()
        .filter_map(|controller| match controller.peek_next() {
            ReplayPeek::Ready { event_us } => Some((event_us, controller.clone())),
            ReplayPeek::BlockedOnWrite | ReplayPeek::Exhausted => None,
        })
        .min_by_key(|(event_us, _)| *event_us)
        .map(|(_, controller)| controller)
}

fn choose_due_controller(
    controllers: &[ReplayController],
    current_us: Option<u64>,
) -> Option<ReplayController> {
    let current_us = current_us?;

    controllers
        .iter()
        .filter_map(|controller| match controller.peek_next() {
            ReplayPeek::Ready { event_us } if event_us <= current_us => {
                Some((event_us, controller.clone()))
            }
            ReplayPeek::Ready { .. } | ReplayPeek::BlockedOnWrite | ReplayPeek::Exhausted => None,
        })
        .min_by_key(|(event_us, _)| *event_us)
        .map(|(_, controller)| controller)
}

fn aggregate_status(controllers: &[ReplayController]) -> ReplayAdvance {
    if controllers
        .iter()
        .any(|controller| controller.peek_next() == ReplayPeek::BlockedOnWrite)
    {
        ReplayAdvance::BlockedOnWrite
    } else {
        ReplayAdvance::Exhausted
    }
}
