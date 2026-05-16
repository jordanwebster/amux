//! Session subscription and input RPCs for AgentService.

use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::AgentServiceCtx;
#[cfg(test)]
use crate::agents::TEST_ECHO_V1;
use crate::agents::claude::io::{
    self as claude_io, ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Output,
    ClaudePtyTranscriptV1ReplayQuery, ClaudeRawV1ReplayQuery,
};
use crate::agents::{
    BroadcastRead, ByteReplayQuery, PtyHandle, SendInputRequest, SessionCloseReason,
    SessionInputEvent, StructuredInputTarget, StructuredOutput, SubscribeSessionEvent,
    SubscribeSessionRequest,
};
use crate::protocol::{ProtocolError, protocol_status};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};

impl AgentServiceCtx {
    pub(crate) async fn send_input(&self, request: SendInputRequest) -> Result<(), ProtocolError> {
        send_session_input(self, request).await
    }

    pub(crate) async fn subscribe_session_response_stream(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<super::ResponseStream<crate::protocol::wire::SubscribeSessionResponse>, ProtocolError>
    {
        let close_rx = self
            .state()
            .write()
            .await
            .local_session_close_events
            .subscribe_drop_on_overflow();
        let shutdown_rx = self
            .state()
            .write()
            .await
            .local_shutdown_events
            .subscribe_drop_on_overflow();
        let prepared = prepare_direct_session_subscription(&request, self).await?;
        Ok(direct_session_response_stream(
            request.agent_id,
            prepared.output,
            close_rx,
            shutdown_rx,
        ))
    }
}

enum SessionOutputReader {
    Raw(crate::agents::MultiplexByteReader),
    Structured {
        reader: crate::agents::MultiplexStructuredReader,
        replay_cursor: Vec<u8>,
    },
}

struct PreparedSessionSubscription {
    output: SessionOutputReader,
}

async fn prepare_direct_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
) -> Result<PreparedSessionSubscription, ProtocolError> {
    match request.io_protocol.as_str() {
        claude_io::RAW_V1 => {
            let reader = prepare_direct_raw_session_subscription(request, ctx).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        claude_io::PTY_TRANSCRIPT_V1 => {
            prepare_direct_structured_session_subscription(request, ctx)
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
            let reader = prepare_direct_test_echo_session_subscription(request, ctx).await?;
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

async fn prepare_direct_raw_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
) -> Result<crate::agents::MultiplexByteReader, ProtocolError> {
    let args = claude_io::decode_raw_v1_args(request.args.as_deref())?;
    let replay_query = args
        .replay_query
        .as_ref()
        .map(|ClaudeRawV1ReplayQuery::TailBytes { count }| ByteReplayQuery::Tail { count: *count });

    let pty = agent_pty(ctx, request.agent_id, claude_io::RAW_V1).await?;
    if let Some(size) = args.terminal_size {
        pty.resize(size)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
    }

    pty.subscribe_with_query(replay_query)
        .await
        .ok_or(ProtocolError::NoAgentFound)
}

#[cfg(test)]
async fn prepare_direct_test_echo_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
) -> Result<crate::agents::MultiplexByteReader, ProtocolError> {
    if request.args.is_some() {
        return Err(ProtocolError::InvalidArgument {
            message: format!("`{TEST_ECHO_V1}` does not accept args"),
        });
    }
    let pty = agent_pty(ctx, request.agent_id, TEST_ECHO_V1).await?;
    pty.subscribe_with_query(None)
        .await
        .ok_or(ProtocolError::NoAgentFound)
}

async fn prepare_direct_structured_session_subscription(
    request: &SubscribeSessionRequest,
    ctx: &AgentServiceCtx,
) -> Result<(crate::agents::MultiplexStructuredReader, u64), ProtocolError> {
    let args = claude_io::decode_pty_transcript_v1_args(request.args.as_deref())?;
    let replay_query = match &args.replay_query {
        None => None,
        Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count }) => {
            Some(crate::agents::SequencedReplayQuery::Tail { count: *count })
        }
        Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id }) => {
            let seq = seq_id
                .checked_add(1)
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: "transcript SubscribeSession replay since cursor is out of range"
                        .to_string(),
                })?;
            Some(crate::agents::SequencedReplayQuery::Since { seq })
        }
    };

    let (log_source, pty) = {
        let state = ctx.state().read().await;
        let session = state
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
        pty.resize(size)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
    }

    log_source
        .subscribe_with_query(replay_query)
        .await
        .ok_or(ProtocolError::NoAgentFound)
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
        .send_structured_input(
            input.expected_seq,
            transcript_actions_to_pty_input_json(input.actions),
        )
        .await
}

async fn agent_pty(
    ctx: &AgentServiceCtx,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<PtyHandle, ProtocolError> {
    let state = ctx.state().read().await;
    let session = state
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
    let state = ctx.state().read().await;
    let session = state
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    ensure_agent_supports_protocol(session, agent_id, io_protocol)?;
    Ok(session.structured_input_target())
}

fn ensure_agent_supports_protocol(
    session: &crate::agents::AgentSession,
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

enum DirectSessionStreamState {
    Opening {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
    },
    Reading {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
    },
    Done,
}

fn direct_session_response_stream(
    agent_id: Uuid,
    reader: SessionOutputReader,
    close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
    shutdown_rx: mpsc::Receiver<ShutdownReason>,
) -> super::ResponseStream<crate::protocol::wire::SubscribeSessionResponse> {
    Box::pin(futures_util::stream::unfold(
        DirectSessionStreamState::Opening {
            agent_id,
            reader,
            close_rx,
            shutdown_rx,
        },
        |state| async move {
            match state {
                DirectSessionStreamState::Opening {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                } => Some((
                    session_output_response(SubscribeSessionEvent::Opened),
                    DirectSessionStreamState::Reading {
                        agent_id,
                        reader,
                        close_rx,
                        shutdown_rx,
                    },
                )),
                DirectSessionStreamState::Reading {
                    agent_id,
                    mut reader,
                    mut close_rx,
                    mut shutdown_rx,
                } => {
                    let event = tokio::select! {
                        biased;
                        reason = shutdown_rx.recv() => {
                            let Some(reason) = reason else {
                                return Some((
                                    Err(tonic::Status::resource_exhausted(
                                        "shutdown event subscriber queue closed",
                                    )),
                                    DirectSessionStreamState::Done,
                                ));
                            };
                            return Some((
                                Err(server_shutdown_status(reason)),
                                DirectSessionStreamState::Done,
                            ));
                        }
                        reason = recv_close_reason_for_agent(&mut close_rx, agent_id) => {
                            match reason {
                                Ok(reason) => SubscribeSessionEvent::Closed { reason },
                                Err(error) => {
                                    return Some((
                                        Err(protocol_status(error)),
                                        DirectSessionStreamState::Done,
                                    ));
                                }
                            }
                        }
                        output = read_session_output_event(&mut reader) => {
                            match output {
                                Some(Ok(event)) => event,
                                Some(Err(error @ ProtocolError::ResourceExhausted { .. })) => {
                                    return Some((
                                        Err(protocol_status(error)),
                                        DirectSessionStreamState::Done,
                                    ));
                                }
                                Some(Err(error)) => SubscribeSessionEvent::Closed {
                                    reason: SessionCloseReason::InternalError {
                                        detail: error.to_string(),
                                    },
                                },
                                None => SubscribeSessionEvent::Closed {
                                    reason: SessionCloseReason::AgentExited {
                                        exit_code: None,
                                    },
                                },
                            }
                        }
                    };
                    let next_state = match &event {
                        SubscribeSessionEvent::Closed { .. } => DirectSessionStreamState::Done,
                        _ => DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                        },
                    };
                    Some((session_output_response(event), next_state))
                }
                DirectSessionStreamState::Done => None,
            }
        },
    ))
}

fn server_shutdown_status(reason: ShutdownReason) -> tonic::Status {
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert(
        SHUTDOWN_REASON_METADATA_KEY,
        tonic::metadata::MetadataValue::from_static(reason.as_wire_value()),
    );
    tonic::Status::with_metadata(tonic::Code::Unavailable, reason.to_string(), metadata)
}

async fn recv_close_reason_for_agent(
    close_rx: &mut mpsc::Receiver<(Uuid, SessionCloseReason)>,
    agent_id: Uuid,
) -> Result<SessionCloseReason, ProtocolError> {
    while let Some((closed_agent_id, reason)) = close_rx.recv().await {
        if closed_agent_id == agent_id {
            return Ok(reason);
        }
    }
    Err(ProtocolError::ResourceExhausted {
        message: "session close event subscriber queue closed".to_string(),
    })
}

fn session_output_response(
    event: SubscribeSessionEvent,
) -> Result<crate::protocol::wire::SubscribeSessionResponse, tonic::Status> {
    Ok(crate::agents::session_output_event_to_wire(&event))
}

async fn read_session_output_event(
    reader: &mut SessionOutputReader,
) -> Option<Result<SubscribeSessionEvent, ProtocolError>> {
    match reader {
        SessionOutputReader::Raw(reader) => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(payload) | BroadcastRead::LiveItem(payload) => {
                Ok(SubscribeSessionEvent::Output { payload })
            }
            BroadcastRead::ReplayComplete => {
                Ok(SubscribeSessionEvent::ReplayComplete { cursor: None })
            }
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
        }),
        SessionOutputReader::Structured {
            reader,
            replay_cursor,
        } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output)
            }
            BroadcastRead::ReplayComplete => Ok(SubscribeSessionEvent::ReplayComplete {
                cursor: Some(replay_cursor.clone()),
            }),
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
        }),
    }
}

fn structured_output_event(
    output: StructuredOutput,
) -> Result<SubscribeSessionEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    let payload = claude_io::encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
        seq_id: output.seq,
        payload: payload_json,
    });
    Ok(SubscribeSessionEvent::Output { payload })
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::agents::MultiplexByteBuffer;

    #[tokio::test]
    async fn direct_session_stream_reports_resource_exhausted_when_reader_lags() {
        let agent_id = Uuid::from_u128(1);
        let buffer = MultiplexByteBuffer::new(1024);
        let reader = buffer.subscribe().await.unwrap();
        let (_close_tx, close_rx) = mpsc::channel(1);
        let (_shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let mut stream = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Raw(reader),
            close_rx,
            shutdown_rx,
        );

        for _ in 0..300 {
            buffer.write(b"x".to_vec()).await;
        }

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            opened.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            replay_complete.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::ReplayComplete(_))
        ));

        let mut saw_resource_exhausted = false;
        for _ in 0..300 {
            match stream
                .next()
                .await
                .expect("session stream ended before lag error")
            {
                Ok(_) => {}
                Err(status) => {
                    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
                    saw_resource_exhausted = true;
                    break;
                }
            }
        }
        assert!(saw_resource_exhausted);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn direct_session_stream_reports_server_shutdown_reason() {
        let agent_id = Uuid::from_u128(1);
        let buffer = MultiplexByteBuffer::new(1024);
        let reader = buffer.subscribe().await.unwrap();
        let (_close_tx, close_rx) = mpsc::channel(1);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let mut stream = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Raw(reader),
            close_rx,
            shutdown_rx,
        );

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            opened.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::Opened(_))
        ));

        shutdown_tx
            .send(ShutdownReason::Suspending)
            .await
            .expect("shutdown receiver should be active");
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert_eq!(error.message(), "server suspending");
        assert_eq!(
            error
                .metadata()
                .get(SHUTDOWN_REASON_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("suspending")
        );
        assert!(stream.next().await.is_none());
    }
}
