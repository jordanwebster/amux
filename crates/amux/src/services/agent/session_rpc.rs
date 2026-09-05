//! Session subscription and input RPCs, driven by [`PtyAgentHost`].

use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;

use amux_artifacts::{ArtifactId, Owner};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::PtyAgentHost;
#[cfg(unix)]
use crate::agents::CodexRawPtyLease;
use crate::agents::claude::io::{
    self as claude_io, ClaudePtyTranscriptV1Output, ClaudePtyTranscriptV1ReplayQuery,
};
use crate::agents::claude::sdk_io::{
    self as claude_sdk_io, ClaudeSdkV1Output, ClaudeSdkV1ReplayQuery,
};
use crate::agents::codex::io::{self as codex_io, CodexSdkV1Output, CodexSdkV1ReplayQuery};
use crate::agents::terminal_io::{self, TerminalV1Control, TerminalV1ReplayQuery};
use crate::agents::{
    ArtifactRef, BroadcastRead, ByteReplayQuery, MaterialiseBackend, Plane, Protocol, PtyHandle,
    RawPtyTarget, SendInputRequest, SessionCloseReason, SessionInputEvent, StructuredInput,
    StructuredInputEvent, StructuredLogSource, StructuredOutput, SubscribeSessionEvent,
    SubscribeSessionRequest, attachments_row, materialise_and_log, materialise_paths,
};
use crate::protocol::{ProtocolError, protocol_status};
use crate::server::{SHUTDOWN_REASON_METADATA_KEY, ShutdownReason};

pub(super) async fn subscribe_session_stream(
    host: &PtyAgentHost,
    request: SubscribeSessionRequest,
    replay_attachments: Option<Vec<ArtifactRef>>,
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
        replay_attachments,
    ))
}

enum SessionOutputReader {
    Raw(RawSessionOutputReader),
    Structured {
        protocol: Protocol,
        reader: crate::agents::MultiplexStructuredReader,
        replay_cursor: Option<Vec<u8>>,
    },
}

struct RawSessionOutputReader {
    protocol: Protocol,
    reader: crate::agents::MultiplexByteReader,
    #[cfg(unix)]
    _codex_lease: Option<CodexRawPtyLease>,
}

struct RawPtySubscription {
    pty: PtyHandle,
    #[cfg(unix)]
    codex_lease: Option<CodexRawPtyLease>,
}

struct PreparedSessionSubscription {
    output: SessionOutputReader,
}

async fn prepare_direct_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<PreparedSessionSubscription, ProtocolError> {
    match request.protocol {
        Protocol::TerminalV1 => {
            let reader = prepare_direct_raw_session_subscription(request, host).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        Protocol::ClaudePtyTranscriptV1 | Protocol::ClaudeSdkV1 | Protocol::CodexSdkV1 => {
            prepare_direct_structured_session_subscription(request, host)
                .await
                .map(|(reader, replay_cursor)| PreparedSessionSubscription {
                    output: SessionOutputReader::Structured {
                        protocol: request.protocol,
                        reader,
                        replay_cursor,
                    },
                })
        }
        Protocol::TestEchoV1 => {
            let reader = prepare_direct_test_echo_session_subscription(request, host).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
    }
}

async fn prepare_direct_raw_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<RawSessionOutputReader, ProtocolError> {
    let args = terminal_io::decode_terminal_v1_args(request.args.as_deref())?;
    let replay_query = args
        .replay_query
        .as_ref()
        .map(|TerminalV1ReplayQuery::TailBytes { count }| ByteReplayQuery::Tail { count: *count });

    let subscription = raw_pty_subscription(host, request.agent_id, Protocol::TerminalV1).await?;
    if let Some(size) = args.terminal_size {
        subscription
            .pty
            .resize(size)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
    }

    let reader = subscription
        .pty
        .subscribe_with_query(replay_query)
        .await
        .ok_or(ProtocolError::NoAgentFound)?;
    Ok(RawSessionOutputReader {
        protocol: Protocol::TerminalV1,
        reader,
        #[cfg(unix)]
        _codex_lease: subscription.codex_lease,
    })
}

async fn prepare_direct_test_echo_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<RawSessionOutputReader, ProtocolError> {
    if request.args.is_some() {
        return Err(ProtocolError::InvalidArgument {
            message: format!("`{}` does not accept args", Protocol::TestEchoV1),
        });
    }
    let subscription = raw_pty_subscription(host, request.agent_id, Protocol::TestEchoV1).await?;
    let pty = subscription.pty;
    let reader = pty
        .subscribe_with_query(None)
        .await
        .ok_or(ProtocolError::NoAgentFound)?;
    Ok(RawSessionOutputReader {
        protocol: Protocol::TestEchoV1,
        reader,
        #[cfg(unix)]
        _codex_lease: subscription.codex_lease,
    })
}

async fn raw_pty_subscription(
    host: &PtyAgentHost,
    agent_id: Uuid,
    protocol: Protocol,
) -> Result<RawPtySubscription, ProtocolError> {
    raw_pty_subscription_with(host, agent_id, protocol, prepare_raw_pty_target).await
}

async fn raw_pty_subscription_with<Prepare, Prepared>(
    host: &PtyAgentHost,
    agent_id: Uuid,
    protocol: Protocol,
    prepare: Prepare,
) -> Result<RawPtySubscription, ProtocolError>
where
    Prepare: FnOnce(RawPtyTarget) -> Prepared,
    Prepared: Future<Output = Result<RawPtySubscription, ProtocolError>>,
{
    let target = raw_plane_target(host, agent_id, protocol).await?;
    prepare(target).await
}

async fn raw_plane_target(
    host: &PtyAgentHost,
    agent_id: Uuid,
    protocol: Protocol,
) -> Result<RawPtyTarget, ProtocolError> {
    let state = host.state().read().await;
    let session = state
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    match session.plane(protocol)? {
        Plane::Terminal(target) => Ok(target),
        Plane::Structured { .. } => Err(ProtocolError::ServerError {
            message: format!("{protocol} resolved to a structured plane"),
        }),
    }
}

async fn prepare_raw_pty_target(target: RawPtyTarget) -> Result<RawPtySubscription, ProtocolError> {
    match target {
        RawPtyTarget::Existing(pty) => Ok(RawPtySubscription {
            pty,
            #[cfg(unix)]
            codex_lease: None,
        }),
        #[cfg(unix)]
        RawPtyTarget::Codex(target) => {
            let lease =
                target
                    .acquire_lease()
                    .await
                    .map_err(|error| ProtocolError::ServerError {
                        message: error.to_string(),
                    })?;
            Ok(RawPtySubscription {
                pty: lease.handle().clone(),
                codex_lease: Some(lease),
            })
        }
    }
}

async fn prepare_direct_structured_session_subscription(
    request: &SubscribeSessionRequest,
    host: &PtyAgentHost,
) -> Result<(crate::agents::MultiplexStructuredReader, Option<Vec<u8>>), ProtocolError> {
    let (replay_query, terminal_size) = structured_replay_query(request)?;
    let log = {
        let state = host.state().read().await;
        let session = state
            .local_agents
            .get(&request.agent_id)
            .map(|context| &context.session)
            .ok_or(ProtocolError::NoAgentFound)?;
        match session.plane(request.protocol)? {
            Plane::Structured { log, .. } => log,
            Plane::Terminal(_) => {
                return Err(ProtocolError::ServerError {
                    message: format!("{} resolved to a terminal plane", request.protocol),
                });
            }
        }
    };

    if let Some(size) = terminal_size {
        let subscription =
            raw_pty_subscription(host, request.agent_id, Protocol::TerminalV1).await?;
        subscription
            .pty
            .resize(size)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?;
    }

    let (reader, current_seq) = log
        .subscribe_with_query(replay_query)
        .await
        .ok_or(ProtocolError::NoAgentFound)?;
    let replay_cursor = (request.protocol == Protocol::ClaudePtyTranscriptV1)
        .then(|| encode_transcript_cursor(current_seq));
    Ok((reader, replay_cursor))
}

fn structured_replay_query(
    request: &SubscribeSessionRequest,
) -> Result<
    (
        Option<crate::agents::SequencedReplayQuery>,
        Option<crate::agents::TerminalSize>,
    ),
    ProtocolError,
> {
    let out_of_range = |protocol: Protocol| ProtocolError::InvalidArgument {
        message: format!("{protocol} replay since cursor is out of range"),
    };
    match request.protocol {
        Protocol::ClaudePtyTranscriptV1 => {
            let args = claude_io::decode_pty_transcript_v1_args(request.args.as_deref())?;
            let query = match args.replay_query {
                None => None,
                Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq_id
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(request.protocol))?,
                    })
                }
            };
            Ok((query, args.terminal_size))
        }
        Protocol::ClaudeSdkV1 => {
            let args = claude_sdk_io::decode_claude_sdk_v1_args(request.args.as_deref())?;
            let query = match args.replay_query {
                None => None,
                Some(ClaudeSdkV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(ClaudeSdkV1ReplayQuery::Since { seq_id }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq_id
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(request.protocol))?,
                    })
                }
            };
            Ok((query, None))
        }
        Protocol::CodexSdkV1 => {
            let args = codex_io::decode_codex_sdk_v1_args(request.args.as_deref())?;
            let query = match args.replay_query {
                None => None,
                Some(CodexSdkV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(CodexSdkV1ReplayQuery::Since { seq }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(request.protocol))?,
                    })
                }
            };
            Ok((query, None))
        }
        Protocol::TerminalV1 | Protocol::TestEchoV1 => Err(ProtocolError::InvalidArgument {
            message: format!("{} is not a structured protocol", request.protocol),
        }),
    }
}

pub(super) async fn send_session_input(
    host: &PtyAgentHost,
    request: SendInputRequest,
    attachment_owner: Option<Arc<Owner>>,
) -> Result<(), ProtocolError> {
    let pins = request
        .pin
        .iter()
        .map(|id| {
            ArtifactId::from_str(id).map_err(|error| ProtocolError::InvalidArgument {
                message: format!("invalid attachment id `{id}`: {error}"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    match request.protocol {
        Protocol::TerminalV1 => {
            reject_raw_attachments(attachment_owner.as_deref(), &pins, request.protocol)?;
            send_raw_session_input(host, request.agent_id, Protocol::TerminalV1, request.event)
                .await
        }
        Protocol::ClaudePtyTranscriptV1 => {
            send_structured_session_input(
                host,
                request.agent_id,
                request.event,
                attachment_owner.as_deref(),
                &pins,
            )
            .await
        }
        Protocol::ClaudeSdkV1 => {
            let (log, target) =
                structured_plane_target(host, request.agent_id, request.protocol).await?;
            let SessionInputEvent::Input { input_id, payload } = request.event else {
                return Err(ProtocolError::InvalidArgument {
                    message: format!(
                        "`{}` does not accept SendInput control events",
                        request.protocol
                    ),
                });
            };
            let mut input = claude_sdk_io::decode_claude_sdk_v1_input(&payload)?;
            if let Some(owner) = attachment_owner.as_deref() {
                let crate::agents::claude::sdk_io::ClaudeSdkV1Input::Prompt { text, image_blocks } =
                    &mut input
                else {
                    return Err(attachments_require_prompt(request.protocol));
                };
                let prepared = materialise_and_log(
                    owner,
                    text,
                    &pins,
                    MaterialiseBackend::ClaudeSdk,
                    &input_id,
                    &log,
                )
                .await?;
                *text = prepared.text;
                *image_blocks = prepared.image_blocks;
            }
            target
                .send(StructuredInputEvent::ClaudeSdk { input_id, input })
                .await
        }
        Protocol::CodexSdkV1 => {
            let SessionInputEvent::Input { input_id, payload } = request.event else {
                return Err(ProtocolError::InvalidArgument {
                    message: format!(
                        "`{}` does not accept SendInput control events",
                        request.protocol
                    ),
                });
            };
            let mut input = codex_io::decode_codex_sdk_v1_input(&payload)?;
            #[cfg(unix)]
            {
                let (log, target) =
                    structured_plane_target(host, request.agent_id, request.protocol).await?;
                if let Some(owner) = attachment_owner.as_deref() {
                    let codex_io::CodexSdkV1Input::UserTurn {
                        input: encoded_items,
                    } = &mut input
                    else {
                        return Err(attachments_require_prompt(request.protocol));
                    };
                    let mut items: Vec<codex::InputItem> = serde_json::from_slice(encoded_items)
                        .map_err(|error| ProtocolError::InvalidArgument {
                            message: format!(
                                "Codex user_turn input must be JSON input items: {error}"
                            ),
                        })?;
                    let mut prepared = materialise_and_log(
                        owner,
                        "",
                        &pins,
                        MaterialiseBackend::Codex,
                        &input_id,
                        &log,
                    )
                    .await?;
                    for item in &mut items {
                        if let codex::InputItem::Text { text } = item {
                            *text = materialise_paths(
                                owner,
                                text,
                                &prepared.refs,
                                MaterialiseBackend::Codex,
                            );
                        }
                    }
                    items.append(&mut prepared.codex_items);
                    *encoded_items =
                        serde_json::to_vec(&items).map_err(|error| ProtocolError::ServerError {
                            message: format!("failed to encode materialised Codex input: {error}"),
                        })?;
                }
                target
                    .send(StructuredInputEvent::Codex { input_id, input })
                    .await
            }
            #[cfg(not(unix))]
            {
                let _ = (host, input_id, input);
                Err(ProtocolError::ServerError {
                    message: "Codex agents are unavailable on this platform".to_string(),
                })
            }
        }
        Protocol::TestEchoV1 => {
            reject_raw_attachments(attachment_owner.as_deref(), &pins, request.protocol)?;
            send_raw_session_input(host, request.agent_id, Protocol::TestEchoV1, request.event)
                .await
        }
    }
}

fn reject_raw_attachments(
    owner: Option<&Owner>,
    pins: &[ArtifactId],
    protocol: Protocol,
) -> Result<(), ProtocolError> {
    let Some(owner) = owner else {
        return Ok(());
    };
    for id in pins {
        owner.meta(id).map_err(crate::agents::store_error)?;
    }
    Err(ProtocolError::InvalidArgument {
        message: format!("`{protocol}` does not accept attachment-bearing inputs"),
    })
}

fn attachments_require_prompt(protocol: Protocol) -> ProtocolError {
    ProtocolError::InvalidArgument {
        message: format!("`{protocol}` attachments require a prompt input"),
    }
}

async fn send_raw_session_input(
    host: &PtyAgentHost,
    agent_id: Uuid,
    protocol: Protocol,
    event: SessionInputEvent,
) -> Result<(), ProtocolError> {
    let pty = match raw_plane_target(host, agent_id, protocol).await? {
        RawPtyTarget::Existing(pty) => pty,
        #[cfg(unix)]
        RawPtyTarget::Codex(target) => {
            target
                .active_handle()
                .ok_or_else(|| ProtocolError::FailedPrecondition {
                    message: "Codex raw PTY is not active; open terminal_v1 first".to_string(),
                })?
        }
    };
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
    attachment_owner: Option<&Owner>,
    pins: &[ArtifactId],
) -> Result<(), ProtocolError> {
    let SessionInputEvent::Input { input_id, payload } = event else {
        return Err(ProtocolError::InvalidArgument {
            message: format!(
                "`{}` does not accept SendInput control events",
                claude_io::PTY_TRANSCRIPT_V1
            ),
        });
    };
    let mut input = claude_io::decode_pty_transcript_v1_input(&payload)?;
    let (log, target) =
        structured_plane_target(host, agent_id, Protocol::ClaudePtyTranscriptV1).await?;
    send_claude_pty_to_target(log, target, input_id, &mut input, attachment_owner, pins).await
}

async fn send_claude_pty_to_target(
    log: StructuredLogSource,
    target: Box<dyn StructuredInput>,
    input_id: Vec<u8>,
    input: &mut claude_io::ClaudePtyTranscriptV1Input,
    attachment_owner: Option<&Owner>,
    pins: &[ArtifactId],
) -> Result<(), ProtocolError> {
    if let Some(owner) = attachment_owner {
        let current_seq = log.current_seq().await;
        if input.expected_seq != current_seq {
            return Err(ProtocolError::SequenceNumberMismatch {
                client_seq: input.expected_seq,
                current_seq,
            });
        }
        let claude_io::Intent::Prompt { text } = &mut input.intent else {
            return Err(attachments_require_prompt(Protocol::ClaudePtyTranscriptV1));
        };
        let prepared = materialise_and_log(
            owner,
            text,
            pins,
            MaterialiseBackend::ClaudePty,
            &input_id,
            &log,
        )
        .await?;
        *text = prepared.text;
        // The metadata row is part of this accepted input and must precede
        // provider delivery. Advance the provider-facing sequence across it.
        input.expected_seq = log.current_seq().await;
    }
    target
        .send(StructuredInputEvent::ClaudePty {
            client_seq: input.expected_seq,
            intent: input.intent.clone(),
            pins: pins.to_vec(),
        })
        .await
}

async fn structured_plane_target(
    host: &PtyAgentHost,
    agent_id: Uuid,
    protocol: Protocol,
) -> Result<(StructuredLogSource, Box<dyn StructuredInput>), ProtocolError> {
    let state = host.state().read().await;
    let session = state
        .local_agents
        .get(&agent_id)
        .map(|context| &context.session)
        .ok_or(ProtocolError::NoAgentFound)?;
    match session.plane(protocol)? {
        Plane::Structured { log, input } => Ok((log, input)),
        Plane::Terminal(_) => Err(ProtocolError::ServerError {
            message: format!("{protocol} resolved to a terminal plane"),
        }),
    }
}

fn encode_transcript_cursor(seq: u64) -> Vec<u8> {
    claude_io::encode_pty_transcript_v1_cursor(seq)
}

enum DirectSessionStreamState {
    Opening {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
        replay_attachments: Option<Vec<ArtifactRef>>,
    },
    ReplayingAttachments {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
        refs: Vec<ArtifactRef>,
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
    replay_attachments: Option<Vec<ArtifactRef>>,
) -> super::ResponseStream<crate::protocol::wire::SubscribeSessionResponse> {
    Box::pin(futures_util::stream::unfold(
        DirectSessionStreamState::Opening {
            agent_id,
            reader,
            close_rx,
            shutdown_rx,
            replay_attachments,
        },
        |state| async move {
            match state {
                DirectSessionStreamState::Opening {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                    replay_attachments,
                } => {
                    let protocol = reader.protocol();
                    let next = match (replay_attachments, &reader) {
                        (Some(refs), SessionOutputReader::Structured { .. }) => {
                            DirectSessionStreamState::ReplayingAttachments {
                                agent_id,
                                reader,
                                close_rx,
                                shutdown_rx,
                                refs,
                            }
                        }
                        _ => DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                        },
                    };
                    Some((
                        session_output_response(SubscribeSessionEvent::Opened, protocol),
                        next,
                    ))
                }
                DirectSessionStreamState::ReplayingAttachments {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                    refs,
                } => {
                    let protocol = reader.protocol();
                    let event = structured_output_event(
                        StructuredOutput {
                            seq: 0,
                            payload: attachments_row(None, &refs),
                        },
                        protocol,
                    );
                    Some((
                        event
                            .map(|event| session_output_response(event, protocol))
                            .unwrap_or_else(|error| Err(protocol_status(error))),
                        DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                        },
                    ))
                }
                DirectSessionStreamState::Reading {
                    agent_id,
                    mut reader,
                    mut close_rx,
                    mut shutdown_rx,
                } => {
                    let protocol = reader.protocol();
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
                    Some((session_output_response(event, protocol), next_state))
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
    protocol: Protocol,
) -> Result<crate::protocol::wire::SubscribeSessionResponse, tonic::Status> {
    crate::agents::session_output_event_to_wire(&event, protocol)
        .map_err(|error| tonic::Status::internal(error.to_string()))
}

impl SessionOutputReader {
    fn protocol(&self) -> Protocol {
        match self {
            Self::Raw(raw) => raw.protocol,
            Self::Structured { protocol, .. } => *protocol,
        }
    }
}

async fn read_session_output_event(
    reader: &mut SessionOutputReader,
) -> Option<Result<SubscribeSessionEvent, ProtocolError>> {
    match reader {
        SessionOutputReader::Raw(raw) => raw.reader.read_event().await.map(|event| match event {
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
            protocol,
            reader,
            replay_cursor,
        } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output, *protocol)
            }
            BroadcastRead::ReplayComplete => Ok(SubscribeSessionEvent::ReplayComplete {
                cursor: replay_cursor.clone(),
            }),
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
        }),
    }
}

fn structured_output_event(
    output: StructuredOutput,
    protocol: Protocol,
) -> Result<SubscribeSessionEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    let payload = match protocol {
        Protocol::ClaudePtyTranscriptV1 => {
            claude_io::encode_pty_transcript_v1_output(ClaudePtyTranscriptV1Output {
                seq_id: output.seq,
                payload: payload_json,
            })
        }
        Protocol::ClaudeSdkV1 => claude_sdk_io::encode_claude_sdk_v1_output(ClaudeSdkV1Output {
            seq_id: output.seq,
            payload: payload_json,
        }),
        Protocol::CodexSdkV1 => codex_io::encode_codex_sdk_v1_output(CodexSdkV1Output {
            seq: output.seq,
            payload: payload_json,
        }),
        Protocol::TerminalV1 | Protocol::TestEchoV1 => {
            return Err(ProtocolError::ServerError {
                message: format!("{protocol} cannot encode structured output"),
            });
        }
    };
    Ok(SubscribeSessionEvent::Output { payload })
}

#[cfg(debug_assertions)]
pub(super) async fn open_in_process_protocol_plane(
    kind: crate::agents::AgentKind,
    protocol: Protocol,
) -> Result<(), ProtocolError> {
    use crate::agents::{AgentSession, AgentType, ClaudeDriver, CreateAgentRequest, new_agent};

    let host_id = Uuid::new_v4();
    let config = crate::config::Config::default();
    let route =
        crate::agents::McpLaunchRoute::for_current_process(&config, host_id).map_err(|error| {
            ProtocolError::ServerError {
                message: error.to_string(),
            }
        })?;
    let host = PtyAgentHost::new_with_mcp_launch_route(
        route,
        crate::keymap_dir(&config.data_dir),
        config.data_dir.clone(),
    )
    .map_err(|error| ProtocolError::ServerError {
        message: error.to_string(),
    })?;
    let agent_id = Uuid::new_v4();
    let agent_type = match kind {
        crate::agents::AgentKind::Claude { driver } => AgentType::Claude { driver },
        crate::agents::AgentKind::Codex => AgentType::Codex {
            model: None,
            approval_policy: None,
            sandbox_policy: None,
            resume_thread_id: None,
        },
        crate::agents::AgentKind::TestAgent => AgentType::TestAgent {
            command: "in-process-test-agent".to_string(),
        },
    };
    let request = CreateAgentRequest {
        agent_id,
        host_id: None,
        name: Some("typed-protocol-test".to_string()),
        agent_type,
        working_dir: std::env::temp_dir(),
        terminal_size: None,
        args: Vec::new(),
        parent: None,
        initial_prompt: None,
    };
    let deps = host.state().read().await.deps.clone();
    let session: AgentSession = match kind {
        crate::agents::AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        } => {
            let session = crate::agents::claude::ClaudeSession::for_protocol_tests(
                &request,
                deps.runtime_dir.clone(),
                deps.claude_version_cache.clone(),
                deps.mcp_launch_route.clone(),
                deps.claude_user_keymap_dir.clone(),
            );
            Box::new(session)
        }
        crate::agents::AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        }
        | crate::agents::AgentKind::Codex
        | crate::agents::AgentKind::TestAgent => {
            new_agent(&request, &deps).map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            })?
        }
    };
    host.state()
        .write()
        .await
        .insert_registered_local_agent(host_id, agent_id, session)
        .map_err(|message| ProtocolError::ServerError { message })?;

    let prepared = prepare_direct_session_subscription(
        &SubscribeSessionRequest {
            agent_id,
            protocol,
            args: None,
        },
        &host,
    )
    .await?;
    drop(prepared);
    if matches!(
        kind,
        crate::agents::AgentKind::Claude {
            driver: ClaudeDriver::Sdk
        }
    ) {
        debug_assert_eq!(protocol, Protocol::ClaudeSdkV1);
    }
    Ok(())
}

#[cfg(debug_assertions)]
pub(super) async fn create_sdk_in_process() -> Result<(), ProtocolError> {
    use crate::agents::{AgentType, ClaudeDriver, CreateAgentRequest, McpLaunchRoute, new_agent};

    let host_id = Uuid::new_v4();
    let config = crate::config::Config::default();
    let route = McpLaunchRoute::for_current_process(&config, host_id).map_err(|error| {
        ProtocolError::ServerError {
            message: error.to_string(),
        }
    })?;
    let host = PtyAgentHost::new_with_mcp_launch_route(
        route,
        crate::keymap_dir(&config.data_dir),
        config.data_dir.clone(),
    )
    .map_err(|error| ProtocolError::ServerError {
        message: error.to_string(),
    })?;
    let request = CreateAgentRequest {
        agent_id: Uuid::new_v4(),
        host_id: None,
        name: Some("sdk-placeholder".to_string()),
        parent: None,
        initial_prompt: None,
        agent_type: AgentType::Claude {
            driver: ClaudeDriver::Sdk,
        },
        working_dir: std::env::temp_dir(),
        args: Vec::new(),
        terminal_size: None,
    };
    let state = host.state().read().await;
    let session = new_agent(&request, &state.deps).map_err(|error| ProtocolError::ServerError {
        message: error.to_string(),
    })?;
    debug_assert_eq!(
        session.kind(),
        crate::agents::AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use tokio::time::{Duration, timeout};

    use super::*;
    #[cfg(unix)]
    use crate::agents::{AgentType, CreateAgentRequest, new_agent};
    use crate::agents::{MultiplexByteBuffer, TestAgentSession};

    #[cfg(unix)]
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
                    parent: None,
                    initial_prompt: None,
                },
                &state.deps,
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
                protocol: Protocol::CodexSdkV1,
                args: None,
            },
            None,
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
    async fn dropping_non_codex_raw_subscription_keeps_pty_alive() {
        let host = PtyAgentHost::new(Uuid::from_u128(1));
        let agent_id = Uuid::from_u128(3);
        {
            let mut state = host.state().write().await;
            state
                .insert_registered_local_agent(
                    host.host_id(),
                    agent_id,
                    Box::new(TestAgentSession::echo_for_tests(agent_id, None)),
                )
                .unwrap();
        }

        let stream = subscribe_session_stream(
            &host,
            SubscribeSessionRequest {
                agent_id,
                protocol: Protocol::TerminalV1,
                args: None,
            },
            None,
        )
        .await
        .unwrap();
        drop(stream);

        let pty = {
            let state = host.state().read().await;
            let Plane::Terminal(RawPtyTarget::Existing(pty)) = state.local_agents[&agent_id]
                .session
                .plane(Protocol::TerminalV1)
                .unwrap()
            else {
                panic!("test-agent terminal plane should hold an existing PTY");
            };
            pty
        };
        let mut reader = pty.subscribe_with_query(None).await.unwrap();
        pty.send_input(b"still-live".to_vec()).await.unwrap();
        assert_eq!(reader.read().await.unwrap(), b"still-live");
    }

    #[tokio::test]
    async fn raw_preparation_does_not_hold_the_host_state_lock() {
        let host = PtyAgentHost::new(Uuid::from_u128(1));
        let agent_id = Uuid::from_u128(4);
        {
            let mut state = timeout(Duration::from_secs(1), host.state().write())
                .await
                .expect("initial host-state write timed out");
            state
                .insert_registered_local_agent(
                    host.host_id(),
                    agent_id,
                    Box::new(TestAgentSession::echo_for_tests(agent_id, None)),
                )
                .unwrap();
        }

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let subscription_host = host.clone();
        let subscription = tokio::spawn(async move {
            timeout(
                Duration::from_secs(2),
                raw_pty_subscription_with(
                    &subscription_host,
                    agent_id,
                    Protocol::TerminalV1,
                    move |target| async move {
                        entered_tx
                            .send(())
                            .expect("preparation observer dropped unexpectedly");
                        timeout(Duration::from_secs(1), release_rx)
                            .await
                            .expect("raw preparation release timed out")
                            .expect("raw preparation release sender dropped");
                        timeout(Duration::from_secs(1), prepare_raw_pty_target(target))
                            .await
                            .expect("raw target preparation timed out")
                    },
                ),
            )
            .await
            .expect("raw subscription preparation timed out")
        });

        timeout(Duration::from_secs(1), entered_rx)
            .await
            .expect("raw preparation did not reach the controlled seam")
            .expect("raw preparation ended before reaching the controlled seam");

        let writer_host = host.clone();
        let (writer_acquired_tx, writer_acquired_rx) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn(async move {
            let _state = timeout(Duration::from_secs(1), writer_host.state().write())
                .await
                .expect("host-state writer timed out");
            writer_acquired_tx
                .send(())
                .expect("writer observer dropped unexpectedly");
        });
        timeout(Duration::from_secs(1), writer_acquired_rx)
            .await
            .expect("host-state writer was blocked by raw preparation")
            .expect("host-state writer ended without acquiring the lock");

        release_tx
            .send(())
            .expect("raw preparation ended before release");
        timeout(Duration::from_secs(1), writer)
            .await
            .expect("host-state writer task timed out")
            .expect("host-state writer task panicked");
        let prepared = timeout(Duration::from_secs(1), subscription)
            .await
            .expect("raw subscription task timed out")
            .expect("raw subscription task panicked")
            .expect("raw subscription failed after release");
        drop(prepared);
    }

    #[tokio::test]
    async fn raw_target_snapshot_preserves_missing_agent_and_protocol_errors() {
        let host = PtyAgentHost::new(Uuid::from_u128(1));
        let agent_id = Uuid::from_u128(5);

        let missing = timeout(
            Duration::from_secs(1),
            raw_pty_subscription(&host, agent_id, Protocol::TerminalV1),
        )
        .await
        .expect("missing-agent lookup timed out");
        let Err(missing) = missing else {
            panic!("missing agent unexpectedly produced a raw subscription");
        };
        assert!(matches!(missing, ProtocolError::NoAgentFound));

        {
            let mut state = timeout(Duration::from_secs(1), host.state().write())
                .await
                .expect("host-state write timed out");
            state
                .insert_registered_local_agent(
                    host.host_id(),
                    agent_id,
                    Box::new(TestAgentSession::echo_for_tests(agent_id, None)),
                )
                .unwrap();
        }
        let unsupported = timeout(
            Duration::from_secs(1),
            raw_pty_subscription(&host, agent_id, Protocol::ClaudeSdkV1),
        )
        .await
        .expect("protocol validation timed out");
        let Err(unsupported) = unsupported else {
            panic!("unsupported protocol unexpectedly produced a raw subscription");
        };
        assert!(matches!(
            unsupported,
            ProtocolError::NotExposed {
                kind: crate::agents::AgentKind::TestAgent,
                protocol: Protocol::ClaudeSdkV1,
            }
        ));
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
            SessionOutputReader::Raw(RawSessionOutputReader {
                protocol: Protocol::TerminalV1,
                reader,
                #[cfg(unix)]
                _codex_lease: None,
            }),
            close_rx,
            shutdown_rx,
            None,
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
    async fn structured_session_replays_pinned_refs_immediately_after_opened() {
        let agent_id = Uuid::from_u128(9);
        let log = StructuredLogSource::new(8);
        let (reader, _) = log.subscribe_with_query(None).await.unwrap();
        let (_close_tx, close_rx) = mpsc::channel(1);
        let (_shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let artifact = ArtifactRef {
            id: amux_artifacts::id_of(b"image"),
            kind: amux_artifacts::ArtifactKind::Image,
            name: "screen.png".to_string(),
            mime: "image/png".to_string(),
            size: 5,
        };
        let mut stream = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Structured {
                protocol: Protocol::ClaudeSdkV1,
                reader,
                replay_cursor: None,
            },
            close_rx,
            shutdown_rx,
            Some(vec![artifact.clone()]),
        );

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            opened.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay = stream.next().await.unwrap().unwrap();
        let Some(crate::protocol::wire::subscribe_session_response::Event::Output(output)) =
            replay.event
        else {
            panic!("expected attachment replay output");
        };
        let Some(crate::protocol::wire::session_output::Output::ClaudeSdkV1(output)) =
            output.output
        else {
            panic!("expected Claude SDK attachment replay output");
        };
        assert_eq!(output.seq_id, 0);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.payload).unwrap(),
            attachments_row(None, &[artifact])
        );

        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            replay_complete.event,
            Some(crate::protocol::wire::subscribe_session_response::Event::ReplayComplete(_))
        ));
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
            SessionOutputReader::Raw(RawSessionOutputReader {
                protocol: Protocol::TerminalV1,
                reader,
                #[cfg(unix)]
                _codex_lease: None,
            }),
            close_rx,
            shutdown_rx,
            None,
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
