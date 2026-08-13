//! Session subscription and input RPCs, driven by [`PtyAgentHost`].

use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::PtyAgentHost;
#[cfg(unix)]
use crate::agents::CodexInput;
#[cfg(any(test, feature = "testnet"))]
use crate::agents::TEST_ECHO_V1;
use crate::agents::claude::io::{
    self as claude_io, ClaudePtyTranscriptV1Action, ClaudePtyTranscriptV1Output,
    ClaudePtyTranscriptV1ReplayQuery,
};
use crate::agents::codex::io::{self as codex_io, CodexSdkV1Output, CodexSdkV1ReplayQuery};
use crate::agents::terminal_io::{self, TerminalV1Control, TerminalV1ReplayQuery};
use crate::agents::{
    BroadcastRead, ByteReplayQuery, PtyHandle, SendInputRequest, SessionCloseReason,
    SessionInputEvent, StructuredInput, StructuredOutput, SubscribeSessionEvent,
    SubscribeSessionRequest,
};
use crate::protocol::{ProtocolError, protocol_status};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};

pub(super) async fn subscribe_session_stream(
    host: &PtyAgentHost,
    request: SubscribeSessionRequest,
) -> Result<super::ResponseStream<crate::protocol::wire::SubscribeSessionResponse>, ProtocolError> {
    let close_rx = host
        .state()
        .write()
        .await
        .local_session_close_events
        .subscribe_drop_on_overflow();
    let shutdown_rx = host
        .state()
        .write()
        .await
        .local_shutdown_events
        .subscribe_drop_on_overflow();
    let prepared = prepare_direct_session_subscription(&request, host).await?;
    Ok(direct_session_response_stream(
        request.agent_id,
        prepared.output,
        close_rx,
        shutdown_rx,
    ))
}

enum SessionOutputReader {
    Raw(crate::agents::MultiplexByteReader),
    Structured {
        reader: crate::agents::MultiplexStructuredReader,
        codec: StructuredCodec,
    },
}

/// Per-protocol structured output encoding, plus whatever the protocol reports
/// as its replay-complete cursor.
enum StructuredCodec {
    Claude { replay_cursor: Vec<u8> },
    Codex,
}

struct PreparedSessionSubscription {
    output: SessionOutputReader,
}

async fn prepare_direct_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<PreparedSessionSubscription, ProtocolError> {
    match request.io_protocol.as_str() {
        terminal_io::TERMINAL_V1 => {
            let reader = prepare_direct_raw_session_subscription(request, host).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        claude_io::PTY_TRANSCRIPT_V1 => {
            prepare_direct_structured_session_subscription(request, host)
                .await
                .map(|(reader, current_seq)| PreparedSessionSubscription {
                    output: SessionOutputReader::Structured {
                        reader,
                        codec: StructuredCodec::Claude {
                            replay_cursor: encode_transcript_cursor(current_seq),
                        },
                    },
                })
        }
        codex_io::CODEX_SDK_V1 => prepare_codex_structured_session_subscription(request, host)
            .await
            .map(|reader| PreparedSessionSubscription {
                output: SessionOutputReader::Structured {
                    reader,
                    codec: StructuredCodec::Codex,
                },
            }),
        #[cfg(any(test, feature = "testnet"))]
        TEST_ECHO_V1 => {
            let reader = prepare_direct_test_echo_session_subscription(request, host).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        other => Err(ProtocolError::InvalidArgument {
            message: format!(
                "unsupported SubscribeSession io_protocol `{other}`; expected `{}`, `{}`, or `{}`",
                terminal_io::TERMINAL_V1,
                claude_io::PTY_TRANSCRIPT_V1,
                codex_io::CODEX_SDK_V1
            ),
        }),
    }
}

async fn prepare_direct_raw_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<crate::agents::MultiplexByteReader, ProtocolError> {
    let args = terminal_io::decode_terminal_v1_args(request.args.as_deref())?;
    let replay_query = args
        .replay_query
        .as_ref()
        .map(|TerminalV1ReplayQuery::TailBytes { count }| ByteReplayQuery::Tail { count: *count });

    let pty = agent_pty(host, request.agent_id, terminal_io::TERMINAL_V1).await?;
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

#[cfg(any(test, feature = "testnet"))]
async fn prepare_direct_test_echo_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<crate::agents::MultiplexByteReader, ProtocolError> {
    if request.args.is_some() {
        return Err(ProtocolError::InvalidArgument {
            message: format!("`{TEST_ECHO_V1}` does not accept args"),
        });
    }
    let pty = agent_pty(host, request.agent_id, TEST_ECHO_V1).await?;
    pty.subscribe_with_query(None)
        .await
        .ok_or(ProtocolError::NoAgentFound)
}

async fn prepare_direct_structured_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
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
        let state = host.state().read().await;
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

async fn prepare_codex_structured_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<crate::agents::MultiplexStructuredReader, ProtocolError> {
    let args = codex_io::decode_codex_sdk_v1_args(request.args.as_deref())?;
    let replay_query = match args.replay_query {
        None => None,
        Some(CodexSdkV1ReplayQuery::Tail { count }) => {
            Some(crate::agents::SequencedReplayQuery::Tail { count })
        }
        Some(CodexSdkV1ReplayQuery::Since { seq }) => {
            let seq = seq
                .checked_add(1)
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: "Codex SubscribeSession replay since cursor is out of range"
                        .to_string(),
                })?;
            Some(crate::agents::SequencedReplayQuery::Since { seq })
        }
    };

    let log_source = {
        let state = host.state().read().await;
        let session = state
            .local_agents
            .get(&request.agent_id)
            .map(|context| &context.session)
            .ok_or(ProtocolError::NoAgentFound)?;
        ensure_agent_supports_protocol(session, request.agent_id, codex_io::CODEX_SDK_V1)?;
        session.log_source().ok_or(ProtocolError::NoAgentFound)?
    };

    log_source
        .subscribe_with_query(replay_query)
        .await
        .map(|(reader, _)| reader)
        .ok_or(ProtocolError::NoAgentFound)
}

pub(super) async fn send_session_input(
    host: &PtyAgentHost,
    request: SendInputRequest,
) -> Result<(), ProtocolError> {
    match request.io_protocol.as_str() {
        terminal_io::TERMINAL_V1 => {
            send_raw_session_input(
                host,
                request.agent_id,
                terminal_io::TERMINAL_V1,
                request.event,
            )
            .await
        }
        claude_io::PTY_TRANSCRIPT_V1 => {
            send_structured_session_input(host, request.agent_id, request.event).await
        }
        codex_io::CODEX_SDK_V1 => {
            let SessionInputEvent::Input { input_id, payload } = request.event else {
                return Err(ProtocolError::InvalidArgument {
                    message: format!(
                        "`{}` does not accept SendInput control events",
                        codex_io::CODEX_SDK_V1
                    ),
                });
            };
            let input = codex_io::decode_codex_sdk_v1_input(&payload)?;
            #[cfg(unix)]
            {
                let target = codex_input_target(host, request.agent_id).await?;
                target.send(input_id, input).await;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (host, input_id, input);
                Err(ProtocolError::ServerError {
                    message: "Codex agents are unavailable on this platform".to_string(),
                })
            }
        }
        #[cfg(any(test, feature = "testnet"))]
        TEST_ECHO_V1 => {
            send_raw_session_input(host, request.agent_id, TEST_ECHO_V1, request.event).await
        }
        other => Err(ProtocolError::InvalidArgument {
            message: format!(
                "unsupported SendInput io_protocol `{other}`; expected `{}`, `{}`, or `{}`",
                terminal_io::TERMINAL_V1,
                claude_io::PTY_TRANSCRIPT_V1,
                codex_io::CODEX_SDK_V1
            ),
        }),
    }
}

async fn send_raw_session_input(
    host: &PtyAgentHost,
    agent_id: Uuid,
    io_protocol: &str,
    event: SessionInputEvent,
) -> Result<(), ProtocolError> {
    let pty = agent_pty(host, agent_id, io_protocol).await?;
    match event {
        SessionInputEvent::Input { payload, .. } => {
            pty.send_input(payload)
                .await
                .map_err(|error| ProtocolError::ServerError {
                    message: error.to_string(),
                })
        }
        SessionInputEvent::Control { payload } => {
            let control = terminal_io::decode_terminal_v1_control(&payload)?;
            match control {
                TerminalV1Control::Resize(size) => {
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
    host: &PtyAgentHost,
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
    let target = structured_input_target(host, agent_id, claude_io::PTY_TRANSCRIPT_V1).await?;
    target
        .send(
            input.expected_seq,
            transcript_actions_to_pty_input_json(input.actions),
        )
        .await
}

async fn agent_pty(
    host: &PtyAgentHost,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<PtyHandle, ProtocolError> {
    let state = host.state().read().await;
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
    host: &PtyAgentHost,
    agent_id: Uuid,
    io_protocol: &str,
) -> Result<Box<dyn StructuredInput>, ProtocolError> {
    let state = host.state().read().await;
    let session = state
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    ensure_agent_supports_protocol(session, agent_id, io_protocol)?;
    session
        .structured_input()
        .ok_or_else(|| ProtocolError::ServerError {
            message: "structured input not supported".to_string(),
        })
}

#[cfg(unix)]
async fn codex_input_target(
    host: &PtyAgentHost,
    agent_id: Uuid,
) -> Result<Box<dyn CodexInput>, ProtocolError> {
    let state = host.state().read().await;
    let session = state
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    ensure_agent_supports_protocol(session, agent_id, codex_io::CODEX_SDK_V1)?;
    session
        .codex_input()
        .ok_or_else(|| ProtocolError::ServerError {
            message: "Codex input not supported".to_string(),
        })
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
        SessionOutputReader::Structured { reader, codec } => {
            reader.read_event().await.map(|event| match event {
                BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                    structured_output_event(output, codec)
                }
                BroadcastRead::ReplayComplete => Ok(SubscribeSessionEvent::ReplayComplete {
                    cursor: match codec {
                        StructuredCodec::Claude { replay_cursor } => Some(replay_cursor.clone()),
                        StructuredCodec::Codex => None,
                    },
                }),
                BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                    message: "session output subscriber queue closed".to_string(),
                }),
            })
        }
    }
}

fn structured_output_event(
    output: StructuredOutput,
    codec: &StructuredCodec,
) -> Result<SubscribeSessionEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    let payload = match codec {
        StructuredCodec::Claude { .. } => {
            claude_io::encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
                seq_id: output.seq,
                payload: payload_json,
            })
        }
        StructuredCodec::Codex => codex_io::encode_codex_sdk_v1_output(CodexSdkV1Output {
            seq: output.seq,
            payload: payload_json,
        }),
    };
    Ok(SubscribeSessionEvent::Output { payload })
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::agents::{AgentType, CreateAgentRequest, MultiplexByteBuffer, new_agent};

    #[tokio::test]
    async fn codex_subscription_opens_and_completes_empty_replay() {
        let host = PtyAgentHost::new(Uuid::from_u128(1));
        let agent_id = Uuid::from_u128(2);
        {
            let mut state = host.state().write().await;
            let session = new_agent(
                &CreateAgentRequest {
                    agent_id,
                    host_id: None,
                    name: Some("codex".into()),
                    agent_type: AgentType::Codex {
                        model: None,
                        approval_policy: None,
                        sandbox_policy: None,
                        resume_thread_id: None,
                    },
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                    args: Vec::new(),
                },
                #[cfg(unix)]
                state.codex_client.clone(),
            )
            .unwrap();
            state
                .insert_registered_local_agent(host.host_id(), agent_id, session)
                .unwrap();
        }

        let mut stream = subscribe_session_stream(
            &host,
            SubscribeSessionRequest {
                agent_id,
                io_protocol: codex_io::CODEX_SDK_V1.to_string(),
                args: None,
            },
        )
        .await
        .unwrap();
        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            opened.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay_complete = stream.next().await.unwrap().unwrap();
        let Some(crate::protocol::wire::subscribe_session_response::Event::ReplayComplete(
            replay_complete,
        )) = replay_complete.event
        else {
            panic!("expected replay-complete marker");
        };
        assert!(replay_complete.cursor.is_none());
    }

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
