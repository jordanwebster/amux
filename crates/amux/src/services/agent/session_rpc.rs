//! Session subscription and input RPCs for AgentService.

use serde_json::json;
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
use crate::protocol::Route;
use crate::protocol::message::ProtocolError;
use crate::protocol::wire::{
    SendInputRequest, SessionInputEvent, SessionOutputEvent, SubscribeSessionRequest,
    encode_session_output_event_payload,
};
use crate::server::{
    EndpointServerStream, RpcDispatcher, ServerStreamEncoder, ServerStreamSendError,
    SessionSubscriptionRuntime, TypedServerStreamSink,
};

impl AgentService {
    pub(crate) async fn subscribe_session(
        call: SubscribeSessionCall,
        request: SubscribeSessionRequest,
        ctx: &AgentServiceCtx,
    ) -> std::result::Result<(), crate::server::ConnectionError> {
        run_subscribe_session(call, request, ctx).await
    }

    pub(crate) async fn send_input(
        ctx: &AgentServiceCtx,
        request: SendInputRequest,
    ) -> Result<(), ProtocolError> {
        send_session_input(ctx, request).await
    }
}

fn session_send_error(error: ServerStreamSendError) -> crate::server::ConnectionError {
    crate::server::ConnectionError::Config(format!("failed to send session frame: {error}"))
}

type SessionOutputSink = TypedServerStreamSink<SessionOutputCodec>;

pub(crate) struct SubscribeSessionCall {
    runtime: SessionSubscriptionRuntime,
    output: SessionOutputSink,
}

impl SubscribeSessionCall {
    pub(crate) fn from_rpc(
        call: EndpointServerStream,
        counterparty_route: Route,
        rpc: RpcDispatcher,
    ) -> Option<Self> {
        let runtime = SessionSubscriptionRuntime::new(
            call.handle,
            counterparty_route,
            call.cancellation,
            rpc,
        )?;
        Some(Self {
            runtime,
            output: call.output.encode_with::<SessionOutputCodec>(),
        })
    }
}

pub(crate) struct SessionOutputCodec;

impl ServerStreamEncoder for SessionOutputCodec {
    type Item = SessionOutputEvent;

    fn encode_item(item: &Self::Item) -> Vec<u8> {
        encode_session_output_event_payload(item)
    }
}

async fn run_subscribe_session(
    call: SubscribeSessionCall,
    request: SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    let SubscribeSessionCall { runtime, output } = call;

    let prepared = tokio::select! {
        biased;
        () = runtime.cancelled() => {
            return Ok(());
        }
        prepared = prepare_session_subscription(&request, ctx, &runtime) => prepared,
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return terminate_session_subscription(runtime, error, ctx).await;
        }
    };

    if !runtime.activate() {
        return Ok(());
    }
    ctx.user_state().write().await.session_subscriptions.insert(
        runtime.call_id().clone(),
        crate::server::SessionSubscriptionState {
            agent_id: request.agent_id,
            counterparty: runtime.counterparty().clone(),
        },
    );
    if !runtime.is_active(ctx.user_state()).await {
        ctx.user_state()
            .write()
            .await
            .session_subscriptions
            .remove(runtime.call_id());
        return Ok(());
    }

    spawn_session_output_stream(
        prepared.output,
        SessionStreamHandle {
            runtime,
            sink: output,
            agent_id: request.agent_id,
            io_protocol: request.io_protocol,
        },
        ctx,
    );

    Ok(())
}

enum SessionOutputReader {
    Raw(crate::buffer::MultiplexByteReader),
    Structured {
        reader: crate::buffer::MultiplexStructuredReader,
        replay_cursor: Vec<u8>,
    },
}

struct PreparedSessionSubscription {
    output: SessionOutputReader,
}

async fn prepare_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
    runtime: &SessionSubscriptionRuntime,
) -> Result<PreparedSessionSubscription, ProtocolError> {
    match request.io_protocol.as_str() {
        claude_io::RAW_V1 => {
            let reader = prepare_raw_session_subscription(request, ctx, runtime).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        claude_io::PTY_TRANSCRIPT_V1 => {
            prepare_structured_session_subscription(request, ctx, runtime)
                .await
                .map(|(reader, current_seq)| PreparedSessionSubscription {
                    output: SessionOutputReader::Structured {
                        reader,
                        replay_cursor: encode_transcript_cursor(current_seq),
                    },
                })
        }
        #[cfg(test)]
        TEST_ECHO_V1 => {
            let reader = prepare_test_echo_session_subscription(request, ctx, runtime).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        other => Err(ProtocolError::InvalidArgument {
            message: format!(
                "unsupported SubscribeSession io_protocol `{other}`; expected `{}` or `{}`",
                claude_io::RAW_V1,
                claude_io::PTY_TRANSCRIPT_V1
            ),
        }),
    }
}

async fn prepare_raw_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
    runtime: &SessionSubscriptionRuntime,
) -> Result<crate::buffer::MultiplexByteReader, ProtocolError> {
    let args = claude_io::decode_raw_v1_args(request.args.as_deref())?;
    ensure_session_not_cancelled(runtime)?;
    let replay_query = args
        .replay_query
        .as_ref()
        .map(|ClaudeRawV1ReplayQuery::TailBytes { count }| ByteReplayQuery::Tail { count: *count });

    let pty = agent_pty(ctx, request.agent_id, claude_io::RAW_V1).await?;
    ensure_session_not_cancelled(runtime)?;
    if let Some(size) = args.terminal_size {
        resize_pty_unless_cancelled(&pty, size, runtime).await?;
    }

    subscribe_raw_unless_cancelled(&pty, replay_query, runtime).await
}

#[cfg(test)]
async fn prepare_test_echo_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
    runtime: &SessionSubscriptionRuntime,
) -> Result<crate::buffer::MultiplexByteReader, ProtocolError> {
    if request.args.is_some() {
        return Err(ProtocolError::InvalidArgument {
            message: format!("`{TEST_ECHO_V1}` does not accept args"),
        });
    }
    ensure_session_not_cancelled(runtime)?;
    let pty = agent_pty(ctx, request.agent_id, TEST_ECHO_V1).await?;
    subscribe_raw_unless_cancelled(&pty, None, runtime).await
}

async fn prepare_structured_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
    runtime: &SessionSubscriptionRuntime,
) -> Result<(crate::buffer::MultiplexStructuredReader, u64), ProtocolError> {
    let args = claude_io::decode_pty_transcript_v1_args(request.args.as_deref())?;
    ensure_session_not_cancelled(runtime)?;
    let replay_query = match &args.replay_query {
        None => None,
        Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count }) => {
            Some(crate::protocol::SequencedReplayQuery::Tail { count: *count })
        }
        Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id }) => {
            let seq = seq_id
                .checked_add(1)
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: "transcript SubscribeSession replay since cursor is out of range"
                        .to_string(),
                })?;
            Some(crate::protocol::SequencedReplayQuery::Since { seq })
        }
    };

    let (log_source, pty) = {
        let us = ctx.user_state().read().await;
        let session = us
            .local_agents
            .get(&request.agent_id)
            .map(|context| &context.session)
            .ok_or(ProtocolError::NoAgentFound)?;
        ensure_agent_supports_protocol(session, request.agent_id, claude_io::PTY_TRANSCRIPT_V1)?;
        (
            session.log_source().ok_or(ProtocolError::NoAgentFound)?,
            session.pty_handle().cloned(),
        )
    };

    ensure_session_not_cancelled(runtime)?;
    if let Some(size) = args.terminal_size {
        let Some(pty) = pty else {
            return Err(ProtocolError::InvalidArgument {
                message: format!(
                    "agent {} does not support terminal resize for `{}` sessions",
                    request.agent_id,
                    claude_io::PTY_TRANSCRIPT_V1
                ),
            });
        };
        resize_pty_unless_cancelled(&pty, size, runtime).await?;
    }

    subscribe_structured_unless_cancelled(&log_source, replay_query, runtime).await
}

async fn send_session_input(
    ctx: &AgentServiceCtx,
    request: SendInputRequest,
) -> Result<(), ProtocolError> {
    match request.io_protocol.as_str() {
        claude_io::RAW_V1 => {
            send_raw_session_input(ctx, request.agent_id, claude_io::RAW_V1, request.event).await
        }
        claude_io::PTY_TRANSCRIPT_V1 => {
            send_structured_session_input(ctx, request.agent_id, request.event).await
        }
        #[cfg(test)]
        TEST_ECHO_V1 => {
            send_raw_session_input(ctx, request.agent_id, TEST_ECHO_V1, request.event).await
        }
        other => Err(ProtocolError::InvalidArgument {
            message: format!(
                "unsupported SendInput io_protocol `{other}`; expected `{}` or `{}`",
                claude_io::RAW_V1,
                claude_io::PTY_TRANSCRIPT_V1
            ),
        }),
    }
}

async fn send_raw_session_input(
    ctx: &AgentServiceCtx,
    agent_id: Uuid,
    io_protocol: &str,
    event: SessionInputEvent,
) -> Result<(), ProtocolError> {
    let pty = agent_pty(ctx, agent_id, io_protocol).await?;
    match event {
        SessionInputEvent::Input { payload, .. } => {
            pty.send_input(payload)
                .await
                .map_err(|error| ProtocolError::ServerError {
                    message: error.to_string(),
                })
        }
        SessionInputEvent::Control { payload } => {
            let control = claude_io::decode_raw_v1_control(&payload)?;
            match control {
                claude_io::ClaudeRawV1Control::Resize(size) => {
                    pty.resize(size)
                        .await
                        .map_err(|error| ProtocolError::ServerError {
                            message: error.to_string(),
                        })
                }
            }
        }
    }
}

async fn send_structured_session_input(
    ctx: &AgentServiceCtx,
    agent_id: Uuid,
    event: SessionInputEvent,
) -> Result<(), ProtocolError> {
    let SessionInputEvent::Input { payload, .. } = event else {
        return Err(ProtocolError::InvalidArgument {
            message: format!(
                "`{}` does not accept SendInput control events",
                claude_io::PTY_TRANSCRIPT_V1
            ),
        });
    };
    let input = claude_io::decode_pty_transcript_v1_input(&payload)?;
    let target = structured_input_target(ctx, agent_id, claude_io::PTY_TRANSCRIPT_V1).await?;
    target
        .send_structured_input_cancellable(
            input.expected_seq,
            transcript_actions_to_pty_input_json(input.actions),
            StructuredInputCancel::new(),
        )
        .await
}

async fn agent_pty(
    ctx: &AgentServiceCtx,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<PtyHandle, ProtocolError> {
    let us = ctx.user_state().read().await;
    let session = us
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    ensure_agent_supports_protocol(session, agent_id, io_protocol)?;
    session
        .pty_handle()
        .cloned()
        .ok_or_else(|| ProtocolError::InvalidArgument {
            message: format!("agent {agent_id} does not support raw PTY sessions"),
        })
}

async fn structured_input_target(
    ctx: &AgentServiceCtx,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<StructuredInputTarget, ProtocolError> {
    let us = ctx.user_state().read().await;
    let session = us
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    ensure_agent_supports_protocol(session, agent_id, io_protocol)?;
    Ok(session.structured_input_target())
}

fn ensure_agent_supports_protocol(
    session: &crate::agent::AgentSession,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<(), ProtocolError> {
    if session
        .io_protocols()
        .iter()
        .any(|protocol| protocol == io_protocol)
    {
        Ok(())
    } else {
        Err(ProtocolError::InvalidArgument {
            message: format!("agent {agent_id} does not support `{io_protocol}` sessions"),
        })
    }
}

fn ensure_session_not_cancelled(runtime: &SessionSubscriptionRuntime) -> Result<(), ProtocolError> {
    if runtime.is_cancelled() {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

async fn resize_pty_unless_cancelled(
    pty: &PtyHandle,
    size: crate::protocol::message::TerminalSize,
    runtime: &SessionSubscriptionRuntime,
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
    runtime: &SessionSubscriptionRuntime,
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
    runtime: &SessionSubscriptionRuntime,
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

async fn terminate_session_subscription(
    runtime: SessionSubscriptionRuntime,
    error: ProtocolError,
    ctx: &AgentServiceCtx,
) -> std::result::Result<(), crate::server::ConnectionError> {
    runtime
        .terminate(ctx.user_state(), error)
        .await
        .map_err(session_send_error)
}

struct SessionStreamHandle {
    runtime: SessionSubscriptionRuntime,
    sink: SessionOutputSink,
    agent_id: Uuid,
    io_protocol: String,
}

fn spawn_session_output_stream(
    mut reader: SessionOutputReader,
    handle: SessionStreamHandle,
    ctx: &AgentServiceCtx,
) {
    let user_state = ctx.user_state().clone();
    let span = tracing::info_span!(
        "session_subscription",
        agent_id = %handle.agent_id,
        io_protocol = %handle.io_protocol
    );

    tokio::spawn(
        async move {
            tokio::select! {
                biased;
                () = handle.runtime.cancelled() => {}
                source_result = async {
                    if !send_session_output_event_if_current(
                        &handle.sink,
                        &user_state,
                        &handle.runtime,
                        SessionOutputEvent::Opened,
                    )
                    .await {
                        return Ok(false);
                    }

                    while let Some(output) = read_session_output_event(&mut reader).await {
                        let output = match output {
                            Ok(output) => output,
                            Err(error) => return Err(error),
                        };
                        if !send_session_output_event_if_current(
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

async fn read_session_output_event(
    reader: &mut SessionOutputReader,
) -> Option<Result<SessionOutputEvent, ProtocolError>> {
    match reader {
        SessionOutputReader::Raw(reader) => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(payload) | BroadcastRead::LiveItem(payload) => {
                Ok(SessionOutputEvent::Output { payload })
            }
            BroadcastRead::ReplayComplete => {
                Ok(SessionOutputEvent::ReplayComplete { cursor: None })
            }
        }),
        SessionOutputReader::Structured {
            reader,
            replay_cursor,
        } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output)
            }
            BroadcastRead::ReplayComplete => Ok(SessionOutputEvent::ReplayComplete {
                cursor: Some(replay_cursor.clone()),
            }),
        }),
    }
}

fn structured_output_event(output: StructuredOutput) -> Result<SessionOutputEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    let payload = claude_io::encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
        seq_id: output.seq,
        payload: payload_json,
    });
    Ok(SessionOutputEvent::Output { payload })
}

async fn send_session_output_event_if_current(
    sink: &SessionOutputSink,
    user_state: &std::sync::Arc<tokio::sync::RwLock<crate::server::ServerUserState>>,
    runtime: &SessionSubscriptionRuntime,
    event: SessionOutputEvent,
) -> bool {
    match sink
        .send_item_if_current(event, || async { runtime.is_active(user_state).await })
        .await
    {
        Ok(sent) => sent,
        Err(error) => {
            tracing::error!(%error, "failed to send session output event");
            false
        }
    }
}

fn cancelled_error() -> ProtocolError {
    ProtocolError::Cancelled {
        message: "SubscribeSession cancelled".to_string(),
    }
}
