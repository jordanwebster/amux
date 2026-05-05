//! OpenSession implementation for AgentService.

use std::collections::VecDeque;

use serde_json::json;
use tokio::sync::{mpsc, watch};
use tracing::Instrument;
use uuid::Uuid;

use super::{AgentService, AgentServiceCtx};
#[cfg(test)]
use crate::agent::TEST_ECHO_V1;
use crate::agent::claude::io::{
    self as claude_io, ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Output,
    ClaudePtyTranscriptV1ReplayQuery, ClaudeRawV1ReplayQuery,
};
use crate::agent::{PtyHandle, StructuredInputCancel, StructuredInputTarget};
use crate::buffer::{BroadcastRead, ByteReplayQuery, StructuredOutput};
use crate::protocol::message::{FrameBody, ProtocolError};
#[cfg(test)]
use crate::protocol::method;
use crate::protocol::wire::{
    OpenSessionClientFrame, OpenSessionInputEvent, OpenSessionOutputEvent, SessionOpenRequest,
    decode_open_session_input_payload, encode_open_session_output_event_payload,
};
use crate::rpc::{
    RpcInboundBidi, RpcRoutedSendError, RpcStreamCodec, RpcStreamEncoder, RpcTypedRoutedSink,
    RpcTypedStreamReader,
};
use crate::server::{
    OpenSessionRuntime, OpenSessionStructuredInput, OpenSessionStructuredInputJob,
    OpenSessionStructuredInputPayload,
};

impl AgentService {
    pub(crate) async fn open_session(
        open: SessionOpenRequest,
        call: OpenSessionCall,
        ctx: &AgentServiceCtx,
    ) -> std::result::Result<(), crate::server::ConnectionError> {
        run_open_session(open, call, ctx).await
    }
}

fn open_session_send_error(error: RpcRoutedSendError) -> crate::server::ConnectionError {
    crate::server::ConnectionError::Config(format!("failed to send OpenSession frame: {error}"))
}

type OpenSessionOutputSink = RpcTypedRoutedSink<OpenSessionOutputCodec>;
type OpenSessionInputStream = RpcTypedStreamReader<OpenSessionInputCodec>;
const PRE_ACTIVATION_INPUT_CAPACITY: usize = 256;

pub(crate) struct OpenSessionCall {
    runtime: OpenSessionRuntime,
    input: OpenSessionInputStream,
    output: OpenSessionOutputSink,
}

impl OpenSessionCall {
    pub(crate) fn from_rpc(call: RpcInboundBidi) -> Option<Self> {
        let call = call.into_typed::<OpenSessionInputCodec, OpenSessionOutputCodec>();
        Some(Self {
            runtime: OpenSessionRuntime::new(call.handle, call.cancellation)?,
            input: call.input,
            output: call.output,
        })
    }
}

async fn run_open_session(
    open: SessionOpenRequest,
    call: OpenSessionCall,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    let OpenSessionCall {
        runtime,
        input: stream_reader,
        output,
    } = call;
    if !runtime.is_active(ctx.user_state()).await {
        return Ok(());
    }

    let (input_gate_tx, input_gate_rx) = watch::channel(OpenSessionInputGate::Preparing);
    spawn_open_session_input_dispatcher(stream_reader, runtime.clone(), input_gate_rx, ctx.clone());

    let prepared = tokio::select! {
        biased;
        () = runtime.cancelled() => {
            return Ok(());
        }
        prepared = prepare_open_session(&open, ctx, &runtime) => prepared,
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return terminate_open_session_call(runtime, error, ctx).await;
        }
    };
    if !runtime.is_active(ctx.user_state()).await {
        return Ok(());
    }

    let input_cancel = StructuredInputCancel::new();
    let (input_handler, structured_input_worker) = match prepared.input {
        PreparedOpenSessionInput::Raw { pty } => (
            OpenSessionInputHandler::Raw {
                pty,
                runtime: runtime.clone(),
            },
            None,
        ),
        PreparedOpenSessionInput::Structured { target } => {
            let (input, rx) = OpenSessionStructuredInput::channel();
            (
                OpenSessionInputHandler::Structured { input },
                Some((rx, target, input_cancel.clone())),
            )
        }
    };
    let sink = output.clone();

    spawn_open_session_output_stream(
        prepared.output,
        OpenSessionStreamHandle {
            runtime: runtime.clone(),
            sink: sink.clone(),
            agent_id: open.agent_id,
            io_protocol: open.io_protocol,
        },
        input_cancel.clone(),
        ctx,
    );
    if let Some((rx, target, cancel)) = structured_input_worker {
        spawn_structured_open_session_input_worker(
            rx,
            OpenSessionStructuredInputWorker {
                sink,
                runtime,
                target,
                cancel,
            },
            ctx,
        );
    }
    let _ = input_gate_tx.send(OpenSessionInputGate::Active(input_handler));

    Ok(())
}

#[derive(Clone)]
enum OpenSessionInputHandler {
    Raw {
        pty: PtyHandle,
        runtime: OpenSessionRuntime,
    },
    Structured {
        input: OpenSessionStructuredInput,
    },
}

#[derive(Clone)]
enum OpenSessionInputGate {
    Preparing,
    Active(OpenSessionInputHandler),
}

fn spawn_open_session_input_dispatcher(
    stream_reader: OpenSessionInputStream,
    runtime: OpenSessionRuntime,
    input_gate: watch::Receiver<OpenSessionInputGate>,
    ctx: AgentServiceCtx,
) {
    tokio::spawn(async move {
        let mut stream_reader = stream_reader;
        let mut input_gate = input_gate;
        let mut pending_inputs = VecDeque::new();
        let input_handler = loop {
            let gate_state = {
                let borrowed = input_gate.borrow();
                borrowed.clone()
            };
            match gate_state {
                OpenSessionInputGate::Preparing => {
                    tokio::select! {
                        biased;
                        () = runtime.cancelled() => {
                            return;
                        }
                        frame = stream_reader.recv() => {
                            let Some(frame) = frame else {
                                return;
                            };
                            let Some(frame) =
                                open_session_client_frame_or_terminate(frame, runtime.clone(), &ctx)
                                    .await
                            else {
                                return;
                            };
                            match frame {
                                OpenSessionClientFrame::Input(event) => {
                                    if pending_inputs.len() >= PRE_ACTIVATION_INPUT_CAPACITY {
                                        if let Err(error) = terminate_open_session_call(
                                            runtime.clone(),
                                            ProtocolError::ResourceExhausted {
                                                message: "OpenSession input queue is full before activation".to_string(),
                                            },
                                            &ctx,
                                        ).await {
                                            tracing::warn!(error = %error, "OpenSession input dispatcher failed");
                                        }
                                        return;
                                    }
                                    pending_inputs.push_back(event);
                                }
                                OpenSessionClientFrame::Cancel => {
                                    if let Err(error) = handle_open_session_cancel(runtime.clone(), &ctx).await {
                                        tracing::warn!(error = %error, "OpenSession input dispatcher failed");
                                    }
                                    return;
                                }
                            }
                        }
                        changed = input_gate.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                    }
                }
                OpenSessionInputGate::Active(input_handler) => break input_handler,
            }
        };

        while let Some(event) = pending_inputs.pop_front() {
            if let Err(error) =
                handle_open_session_input_event(runtime.clone(), &input_handler, event, &ctx).await
            {
                tracing::warn!(error = %error, "OpenSession input dispatcher failed");
                return;
            }
        }

        loop {
            let frame = tokio::select! {
                biased;
                () = runtime.cancelled() => {
                    return;
                }
                frame = stream_reader.recv() => frame,
            };
            let Some(frame) = frame else {
                return;
            };
            let Some(frame) =
                open_session_client_frame_or_terminate(frame, runtime.clone(), &ctx).await
            else {
                return;
            };

            if let Err(error) =
                handle_open_session_client_frame(runtime.clone(), &input_handler, frame, &ctx).await
            {
                tracing::warn!(error = %error, "OpenSession input dispatcher failed");
                return;
            }
        }
    });
}

async fn open_session_client_frame_or_terminate(
    frame: Result<OpenSessionClientFrame, ProtocolError>,
    runtime: OpenSessionRuntime,
    ctx: &AgentServiceCtx,
) -> Option<OpenSessionClientFrame> {
    match frame {
        Ok(frame) => Some(frame),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to decode protobuf OpenSession client frame"
            );
            if let Err(error) = terminate_open_session_call(runtime, error, ctx).await {
                tracing::warn!(error = %error, "OpenSession input dispatcher failed");
            }
            None
        }
    }
}

pub(crate) struct OpenSessionInputCodec;

impl RpcStreamCodec for OpenSessionInputCodec {
    type Item = OpenSessionClientFrame;

    fn decode_frame(frame: FrameBody) -> Result<Self::Item, ProtocolError> {
        match frame {
            FrameBody::StreamItem(payload) => {
                let event = decode_open_session_input_payload(&payload).map_err(|error| {
                    ProtocolError::InvalidArgument {
                        message: error.to_string(),
                    }
                })?;
                Ok(OpenSessionClientFrame::Input(event))
            }
            FrameBody::Cancel => Ok(OpenSessionClientFrame::Cancel),
            FrameBody::Request(_) | FrameBody::Response(_) => Err(ProtocolError::InvalidArgument {
                message: "OpenSession stream accepts only stream items or cancel frames"
                    .to_string(),
            }),
        }
    }
}

pub(crate) struct OpenSessionOutputCodec;

impl RpcStreamEncoder for OpenSessionOutputCodec {
    type Item = OpenSessionOutputEvent;

    fn encode_item(item: &Self::Item) -> Vec<u8> {
        encode_open_session_output_event_payload(item)
    }
}

async fn handle_open_session_client_frame(
    runtime: OpenSessionRuntime,
    input_handler: &OpenSessionInputHandler,
    frame: OpenSessionClientFrame,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    match frame {
        OpenSessionClientFrame::Input(event) => {
            handle_open_session_input_event(runtime, input_handler, event, ctx).await
        }
        OpenSessionClientFrame::Cancel => handle_open_session_cancel(runtime, ctx).await,
    }
}

enum OpenSessionOutputReader {
    Raw(crate::buffer::MultiplexByteReader),
    Structured {
        reader: crate::buffer::MultiplexStructuredReader,
        replay_cursor: Vec<u8>,
    },
}

struct OpenSessionPrepared {
    output: OpenSessionOutputReader,
    input: PreparedOpenSessionInput,
}

enum PreparedOpenSessionInput {
    Raw { pty: PtyHandle },
    Structured { target: StructuredInputTarget },
}

async fn prepare_open_session(
    open: &SessionOpenRequest,
    ctx: &AgentServiceCtx,
    runtime: &OpenSessionRuntime,
) -> Result<OpenSessionPrepared, ProtocolError> {
    match open.io_protocol.as_str() {
        claude_io::RAW_V1 => {
            let (reader, pty) = prepare_raw_open_session(open, ctx, runtime).await?;
            Ok(OpenSessionPrepared {
                output: OpenSessionOutputReader::Raw(reader),
                input: PreparedOpenSessionInput::Raw { pty },
            })
        }
        claude_io::PTY_TRANSCRIPT_V1 => prepare_structured_open_session(open, ctx, runtime)
            .await
            .map(|(reader, current_seq, target)| OpenSessionPrepared {
                output: OpenSessionOutputReader::Structured {
                    reader,
                    replay_cursor: encode_transcript_cursor(current_seq),
                },
                input: PreparedOpenSessionInput::Structured { target },
            }),
        #[cfg(test)]
        TEST_ECHO_V1 => {
            let (reader, pty) = prepare_test_echo_open_session(open, ctx, runtime).await?;
            Ok(OpenSessionPrepared {
                output: OpenSessionOutputReader::Raw(reader),
                input: PreparedOpenSessionInput::Raw { pty },
            })
        }
        other => Err(ProtocolError::InvalidArgument {
            message: format!(
                "unsupported OpenSession io_protocol `{other}`; expected `{}` or `{}`",
                claude_io::RAW_V1,
                claude_io::PTY_TRANSCRIPT_V1
            ),
        }),
    }
}

async fn prepare_raw_open_session(
    open: &SessionOpenRequest,
    ctx: &AgentServiceCtx,
    runtime: &OpenSessionRuntime,
) -> Result<(crate::buffer::MultiplexByteReader, PtyHandle), ProtocolError> {
    let args = claude_io::decode_raw_v1_args(open.args.as_deref())?;
    ensure_open_session_not_cancelled(runtime)?;
    let replay_query = args
        .replay_query
        .as_ref()
        .map(|ClaudeRawV1ReplayQuery::TailBytes { count }| ByteReplayQuery::Tail { count: *count });

    let pty = {
        let us = ctx.user_state().read().await;
        let session = us
            .agents
            .get(&open.agent_id)
            .ok_or(ProtocolError::NoAgentFound)?;
        session
            .pty_handle()
            .cloned()
            .ok_or_else(|| ProtocolError::InvalidArgument {
                message: format!("agent {} does not support raw PTY sessions", open.agent_id),
            })?
    };

    ensure_open_session_not_cancelled(runtime)?;
    if let Some(size) = args.terminal_size {
        resize_pty_unless_cancelled(&pty, size, runtime).await?;
    }

    let reader = subscribe_raw_unless_cancelled(&pty, replay_query, runtime).await?;
    Ok((reader, pty))
}

#[cfg(test)]
async fn prepare_test_echo_open_session(
    open: &SessionOpenRequest,
    ctx: &AgentServiceCtx,
    runtime: &OpenSessionRuntime,
) -> Result<(crate::buffer::MultiplexByteReader, PtyHandle), ProtocolError> {
    if open.args.is_some() {
        return Err(ProtocolError::InvalidArgument {
            message: format!("`{TEST_ECHO_V1}` does not accept args"),
        });
    }
    ensure_open_session_not_cancelled(runtime)?;

    let pty = {
        let us = ctx.user_state().read().await;
        let session = us
            .agents
            .get(&open.agent_id)
            .ok_or(ProtocolError::NoAgentFound)?;
        if !session
            .io_protocols()
            .iter()
            .any(|protocol| protocol == TEST_ECHO_V1)
        {
            return Err(ProtocolError::InvalidArgument {
                message: format!(
                    "agent {} does not support `{TEST_ECHO_V1}` sessions",
                    open.agent_id
                ),
            });
        }
        session
            .pty_handle()
            .cloned()
            .ok_or(ProtocolError::NoAgentFound)?
    };

    let reader = subscribe_raw_unless_cancelled(&pty, None, runtime).await?;
    Ok((reader, pty))
}

async fn prepare_structured_open_session(
    open: &SessionOpenRequest,
    ctx: &AgentServiceCtx,
    runtime: &OpenSessionRuntime,
) -> Result<
    (
        crate::buffer::MultiplexStructuredReader,
        u64,
        StructuredInputTarget,
    ),
    ProtocolError,
> {
    let args = claude_io::decode_pty_transcript_v1_args(open.args.as_deref())?;
    ensure_open_session_not_cancelled(runtime)?;
    let replay_query = match &args.replay_query {
        None => None,
        Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count }) => {
            Some(crate::protocol::SequencedReplayQuery::Tail { count: *count })
        }
        Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id }) => {
            let seq = seq_id
                .checked_add(1)
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: "transcript OpenSession replay since cursor is out of range"
                        .to_string(),
                })?;
            Some(crate::protocol::SequencedReplayQuery::Since { seq })
        }
    };

    let (log_source, pty, input_target) = {
        let us = ctx.user_state().read().await;
        let session = us
            .agents
            .get(&open.agent_id)
            .ok_or(ProtocolError::NoAgentFound)?;
        if !session
            .io_protocols()
            .iter()
            .any(|protocol| protocol == claude_io::PTY_TRANSCRIPT_V1)
        {
            return Err(ProtocolError::InvalidArgument {
                message: format!(
                    "agent {} does not support `{}` sessions",
                    open.agent_id,
                    claude_io::PTY_TRANSCRIPT_V1
                ),
            });
        }
        (
            session.log_source().ok_or(ProtocolError::NoAgentFound)?,
            session.pty_handle().cloned(),
            session.structured_input_target(),
        )
    };

    ensure_open_session_not_cancelled(runtime)?;
    if let Some(size) = args.terminal_size {
        let Some(pty) = pty else {
            return Err(ProtocolError::InvalidArgument {
                message: format!(
                    "agent {} does not support terminal resize for `{}` sessions",
                    open.agent_id,
                    claude_io::PTY_TRANSCRIPT_V1
                ),
            });
        };
        resize_pty_unless_cancelled(&pty, size, runtime).await?;
    };

    let (reader, current_seq) =
        subscribe_structured_unless_cancelled(&log_source, replay_query, runtime).await?;
    Ok((reader, current_seq, input_target))
}

fn ensure_open_session_not_cancelled(runtime: &OpenSessionRuntime) -> Result<(), ProtocolError> {
    if runtime.is_cancelled() {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

async fn resize_pty_unless_cancelled(
    pty: &PtyHandle,
    size: crate::protocol::message::TerminalSize,
    runtime: &OpenSessionRuntime,
) -> Result<(), ProtocolError> {
    tokio::select! {
        biased;
        () = runtime.cancelled() => Err(cancelled_error()),
        result = pty.resize(size) => result.map_err(|error| ProtocolError::ServerError {
            message: error.to_string(),
        }),
    }
}

async fn subscribe_raw_unless_cancelled(
    pty: &PtyHandle,
    replay_query: Option<ByteReplayQuery>,
    runtime: &OpenSessionRuntime,
) -> Result<crate::buffer::MultiplexByteReader, ProtocolError> {
    tokio::select! {
        biased;
        () = runtime.cancelled() => Err(cancelled_error()),
        reader = pty.subscribe_with_query(replay_query) => {
            reader.ok_or(ProtocolError::NoAgentFound)
        }
    }
}

async fn subscribe_structured_unless_cancelled(
    log_source: &crate::agent::StructuredLogSource,
    replay_query: Option<crate::protocol::SequencedReplayQuery>,
    runtime: &OpenSessionRuntime,
) -> Result<(crate::buffer::MultiplexStructuredReader, u64), ProtocolError> {
    tokio::select! {
        biased;
        () = runtime.cancelled() => Err(cancelled_error()),
        reader = log_source.subscribe_with_query(replay_query) => {
            reader.ok_or(ProtocolError::NoAgentFound)
        }
    }
}

fn encode_transcript_cursor(seq: u64) -> Vec<u8> {
    claude_io::encode_pty_transcript_v1_cursor(seq)
}

fn transcript_actions_to_pty_input_json(
    actions: Vec<ClaudePtyTranscriptV1Action>,
) -> serde_json::Value {
    serde_json::Value::Array(
        actions
            .into_iter()
            .map(|action| match action {
                ClaudePtyTranscriptV1Action::Write(bytes) => json!({ "Bytes": bytes }),
                ClaudePtyTranscriptV1Action::DelayMs(delay_ms) => json!({ "Delay": delay_ms }),
            })
            .collect(),
    )
}

#[derive(Clone)]
struct OpenSessionInputTarget {
    runtime: OpenSessionRuntime,
}

impl OpenSessionInputTarget {
    async fn terminate_current(
        self,
        error: ProtocolError,
        ctx: &AgentServiceCtx,
    ) -> std::result::Result<(), crate::server::ConnectionError> {
        terminate_open_session_call(self.runtime, error, ctx).await
    }
}

async fn handle_open_session_input_event(
    runtime: OpenSessionRuntime,
    input_handler: &OpenSessionInputHandler,
    event: OpenSessionInputEvent,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    if !runtime.is_active(ctx.user_state()).await {
        return Ok(());
    }

    let target = OpenSessionInputTarget {
        runtime: runtime.clone(),
    };

    match input_handler {
        OpenSessionInputHandler::Raw { pty, runtime } => {
            handle_raw_open_session_input_event(target, pty, runtime, event, ctx).await
        }
        OpenSessionInputHandler::Structured { input } => {
            handle_structured_open_session_input_event(target, input, event, ctx).await
        }
    }
}

async fn handle_raw_open_session_input_event(
    target: OpenSessionInputTarget,
    pty: &PtyHandle,
    runtime: &OpenSessionRuntime,
    event: OpenSessionInputEvent,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    match event {
        OpenSessionInputEvent::Input { payload, .. } => {
            tokio::select! {
                biased;
                () = runtime.cancelled() => Ok(()),
                result = pty.send_input(payload) => {
                    if let Err(error) = result {
                        return target
                            .terminate_current(
                                ProtocolError::ServerError {
                                    message: error.to_string(),
                                },
                                ctx,
                            )
                            .await;
                    }
                    Ok(())
                }
            }
        }
    }
}

async fn handle_structured_open_session_input_event(
    target: OpenSessionInputTarget,
    structured_input: &OpenSessionStructuredInput,
    event: OpenSessionInputEvent,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    match event {
        OpenSessionInputEvent::Input { input_id, payload } => {
            let input = claude_io::decode_pty_transcript_v1_input(&payload);
            let job = OpenSessionStructuredInputJob {
                input_id,
                input: input.map(|input| OpenSessionStructuredInputPayload {
                    client_seq: input.expected_seq,
                    payload: transcript_actions_to_pty_input_json(input.actions),
                }),
            };
            match structured_input.tx.try_send(job) {
                Ok(()) => Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    target
                        .terminate_current(
                            ProtocolError::ResourceExhausted {
                                message: "transcript OpenSession input queue is full".to_string(),
                            },
                            ctx,
                        )
                        .await
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    target
                        .terminate_current(
                            ProtocolError::Cancelled {
                                message: "transcript OpenSession input worker is closed"
                                    .to_string(),
                            },
                            ctx,
                        )
                        .await
                }
            }
        }
    }
}

struct OpenSessionStructuredInputWorker {
    sink: OpenSessionOutputSink,
    runtime: OpenSessionRuntime,
    target: StructuredInputTarget,
    cancel: crate::agent::StructuredInputCancel,
}

fn spawn_structured_open_session_input_worker(
    mut rx: mpsc::Receiver<OpenSessionStructuredInputJob>,
    worker: OpenSessionStructuredInputWorker,
    ctx: &AgentServiceCtx,
) {
    let user_state = ctx.user_state().clone();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            if worker.cancel.is_cancelled() || worker.runtime.is_cancelled() {
                return;
            }

            let result = match job.input {
                Ok(input) => tokio::select! {
                    biased;
                    () = worker.runtime.cancelled() => {
                        worker.cancel.cancel();
                        return;
                    }
                    result = worker.target.send_structured_input_cancellable(
                        input.client_seq,
                        input.payload,
                        worker.cancel.clone(),
                    ) => result,
                },
                Err(error) => Err(error),
            };

            if worker.cancel.is_cancelled()
                || worker.runtime.is_cancelled()
                || !worker.runtime.is_active(&user_state).await
            {
                return;
            }

            tokio::select! {
                biased;
                () = worker.runtime.cancelled() => {
                    worker.cancel.cancel();
                }
                () = worker.cancel.cancelled() => {}
                sent = send_open_session_output_event_if_current(
                    &worker.sink,
                    &user_state,
                    &worker.runtime,
                    OpenSessionOutputEvent::InputResult {
                        input_id: job.input_id,
                        result,
                    },
                ) => {
                    if !sent {
                        return;
                    }
                }
            }
        }
    });
}

async fn handle_open_session_cancel(
    runtime: OpenSessionRuntime,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    runtime
        .terminate(ctx.user_state(), cancelled_error())
        .await
        .map_err(open_session_send_error)
}

async fn terminate_open_session_call(
    runtime: OpenSessionRuntime,
    error: ProtocolError,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    runtime
        .terminate(ctx.user_state(), error)
        .await
        .map_err(open_session_send_error)
}

struct OpenSessionStreamHandle {
    runtime: OpenSessionRuntime,
    sink: OpenSessionOutputSink,
    agent_id: Uuid,
    io_protocol: String,
}

fn spawn_open_session_output_stream(
    mut reader: OpenSessionOutputReader,
    handle: OpenSessionStreamHandle,
    input_cancel: StructuredInputCancel,
    ctx: &AgentServiceCtx,
) {
    let user_state = ctx.user_state().clone();
    let span = tracing::info_span!(
        "open_session_stream",
        agent_id = %handle.agent_id,
        io_protocol = %handle.io_protocol
    );

    tokio::spawn(
        async move {
            tokio::select! {
                biased;
                () = handle.runtime.cancelled() => {
                    input_cancel.cancel();
                }
                source_result = async {
                    if !send_open_session_output_event_if_current(
                        &handle.sink,
                        &user_state,
                        &handle.runtime,
                        OpenSessionOutputEvent::Opened,
                    )
                    .await {
                        return Ok(false);
                    }

                    while let Some(output) = read_open_session_output_event(&mut reader).await {
                        let output = match output {
                            Ok(output) => output,
                            Err(error) => return Err(error),
                        };
                        if !send_open_session_output_event_if_current(
                            &handle.sink,
                            &user_state,
                            &handle.runtime,
                            output,
                        )
                        .await {
                            return Ok(false);
                        }
                    }

                    Ok(true)
                } => {
                    input_cancel.cancel();
                    let _ = handle
                        .runtime
                        .finish_output_source(&user_state, source_result)
                        .await;
                }
            }
        }
        .instrument(span),
    );
}

async fn read_open_session_output_event(
    reader: &mut OpenSessionOutputReader,
) -> Option<Result<OpenSessionOutputEvent, ProtocolError>> {
    match reader {
        OpenSessionOutputReader::Raw(reader) => {
            reader.read_event().await.map(|event| match event {
                BroadcastRead::ReplayItem(payload) | BroadcastRead::LiveItem(payload) => {
                    Ok(OpenSessionOutputEvent::Output {
                        payload,
                        cursor: None,
                    })
                }
                BroadcastRead::ReplayComplete => {
                    Ok(OpenSessionOutputEvent::ReplayComplete { cursor: None })
                }
            })
        }
        OpenSessionOutputReader::Structured {
            reader,
            replay_cursor,
        } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output)
            }
            BroadcastRead::ReplayComplete => Ok(OpenSessionOutputEvent::ReplayComplete {
                cursor: Some(replay_cursor.clone()),
            }),
        }),
    }
}

fn structured_output_event(
    output: StructuredOutput,
) -> Result<OpenSessionOutputEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript OpenSession output: {error}"),
        })?;
    let payload = claude_io::encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
        seq_id: output.seq,
        payload: payload_json,
    });
    Ok(OpenSessionOutputEvent::Output {
        payload,
        cursor: Some(encode_transcript_cursor(output.seq)),
    })
}

async fn send_open_session_output_event_if_current(
    sink: &OpenSessionOutputSink,
    user_state: &std::sync::Arc<tokio::sync::RwLock<crate::server::ServerUserState>>,
    runtime: &OpenSessionRuntime,
    event: OpenSessionOutputEvent,
) -> bool {
    match sink
        .send_item_if_current(event, || async { runtime.is_active(user_state).await })
        .await
    {
        Ok(sent) => sent,
        Err(error) => {
            tracing::error!(%error, "failed to send OpenSession output event");
            false
        }
    }
}

fn cancelled_error() -> ProtocolError {
    ProtocolError::Cancelled {
        message: "OpenSession cancelled".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use super::*;
    use crate::protocol::Link;
    use crate::protocol::message::{Message, ResponseFrame, RoutedCallId, RoutedFrameMessage};
    use crate::protocol::route::Route;
    use crate::rpc::{
        InboundCallState, RpcCallCancellation, RpcInboundCallHandle, RpcInboundFrameTarget,
        RpcRoutedBidiStart,
    };
    use crate::server::test_helpers;

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn call_id(n: u128) -> RoutedCallId {
        RoutedCallId::from(Uuid::from_u128(n))
    }

    fn runtime_for(
        counterparty_route: Route,
        call_id: RoutedCallId,
        generation: Uuid,
    ) -> OpenSessionRuntime {
        OpenSessionRuntime::new(
            RpcInboundCallHandle {
                counterparty_route,
                call_id,
                method: method::AGENT_OPEN_SESSION,
                generation,
            },
            RpcCallCancellation::for_tests(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn stale_open_session_cancel_does_not_close_reused_call() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (host_id, is_cloud_server) = {
            let state = state.read().await;
            (state.host_id(), state.is_cloud_server())
        };
        let ctx = AgentServiceCtx::new(
            user_state.clone(),
            event_tx,
            Uuid::nil(),
            host_id,
            is_cloud_server,
        );
        let (output_tx, mut output_rx) = mpsc::channel(4);
        let counterparty_route = route("client");
        let routed_call_id = call_id(42);
        let stale_generation = Uuid::new_v4();

        {
            let mut us = user_state.write().await;
            us.rpc
                .register_routed_bidi(RpcRoutedBidiStart {
                    tx: output_tx,
                    owner_link: Link::new("owner").unwrap(),
                    reply_src: route("server"),
                    reply_dst: route("client"),
                    counterparty_route: counterparty_route.clone(),
                    call_id: routed_call_id.clone(),
                    method: method::AGENT_OPEN_SESSION,
                    dedup_key: None,
                    stream_capacity: 1,
                })
                .unwrap();
        }

        handle_open_session_cancel(
            runtime_for(
                counterparty_route.clone(),
                routed_call_id.clone(),
                stale_generation,
            ),
            &ctx,
        )
        .await
        .unwrap();

        let us = user_state.read().await;
        assert!(matches!(
            us.rpc.inbound_for_route(&counterparty_route, &routed_call_id),
            Some(call)
                if call.method == method::AGENT_OPEN_SESSION
                    && matches!(call.state, InboundCallState::Active)
        ));
        assert!(matches!(
            output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn pre_activation_input_buffer_overflow_closes_call() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (host_id, is_cloud_server) = {
            let state = state.read().await;
            (state.host_id(), state.is_cloud_server())
        };
        let ctx = AgentServiceCtx::new(
            user_state.clone(),
            event_tx,
            Uuid::nil(),
            host_id,
            is_cloud_server,
        );
        let (output_tx, mut output_rx) = mpsc::channel(4);
        let counterparty_route = route("client");
        let routed_call_id = call_id(42);
        let call = {
            let mut us = user_state.write().await;
            us.rpc
                .register_routed_bidi(RpcRoutedBidiStart {
                    tx: output_tx,
                    owner_link: Link::new("owner").unwrap(),
                    reply_src: route("server"),
                    reply_dst: route("client"),
                    counterparty_route: counterparty_route.clone(),
                    call_id: routed_call_id.clone(),
                    method: method::AGENT_OPEN_SESSION,
                    dedup_key: None,
                    stream_capacity: 256,
                })
                .unwrap()
        };
        let open_call = OpenSessionCall::from_rpc(call).unwrap();
        let (_input_gate_tx, input_gate_rx) = watch::channel(OpenSessionInputGate::Preparing);
        spawn_open_session_input_dispatcher(
            open_call.input,
            open_call.runtime.clone(),
            input_gate_rx,
            ctx,
        );
        let stream_writer = {
            let us = user_state.read().await;
            let Some(RpcInboundFrameTarget::ActiveStream { stream_writer, .. }) = us
                .rpc
                .inbound_frame_target_for_route(&counterparty_route, &routed_call_id)
            else {
                panic!("expected active OpenSession stream writer");
            };
            stream_writer
        };
        let frame = crate::protocol::wire::decode_frame_body(
            &crate::protocol::open_session::encode_open_session_input(Vec::new(), b"x".to_vec())
                .unwrap(),
        )
        .unwrap();

        for _ in 0..=PRE_ACTIVATION_INPUT_CAPACITY {
            stream_writer.send_frame_body(frame.clone()).await.unwrap();
        }

        let msg = timeout(Duration::from_secs(1), output_rx.recv())
            .await
            .expect("timed out waiting for pre-activation overflow response")
            .expect("expected pre-activation overflow response");
        let Message::Routed(crate::protocol::message::RoutedFrame {
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed response");
        };
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::ResourceExhausted { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected resource-exhausted response");
        };
        assert!(message.contains("before activation"));
        assert!(
            user_state
                .read()
                .await
                .rpc
                .inbound_for_route(&counterparty_route, &routed_call_id)
                .is_none()
        );
    }
}
