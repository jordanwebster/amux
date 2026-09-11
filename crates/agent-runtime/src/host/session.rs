//! Session subscription and input RPCs, driven by [`AgentRuntime`].

use std::future::Future;
use std::sync::Arc;

use artifacts::Owner;
use host_api::{
    HostSessionArgs, HostSessionEvent, HostSessionInput, HostSessionStream, HostStreamError,
    SessionInputRequest, SessionRequest,
};
use model::{ArtifactId, ProtocolError};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::AgentRuntime;
#[cfg(unix)]
use crate::agents::CodexRawPtyLease;
use crate::agents::claude::io::{self as claude_io, ClaudePtyTranscriptV1ReplayQuery};
use crate::agents::claude::sdk_io as claude_sdk_io;
use crate::agents::terminal_io::{TerminalV1Control, TerminalV1ReplayQuery};
use crate::agents::{
    ArtifactRef, BroadcastRead, ByteReplayQuery, MaterialiseBackend, Plane, Protocol, PtyHandle,
    RawPtyTarget, SessionCloseReason, StructuredInput, StructuredInputEvent, StructuredLogSource,
    StructuredOutput, attachments_row, materialise_and_log, materialise_paths,
};
use model::ShutdownReason;

pub(super) async fn subscribe_session_stream(
    host: &AgentRuntime,
    request: SessionRequest,
    replay_attachments: Option<Vec<ArtifactRef>>,
) -> Result<HostSessionStream, ProtocolError> {
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
        replay_cursor: Option<u64>,
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
    request: &SessionRequest,
    host: &AgentRuntime,
) -> Result<PreparedSessionSubscription, ProtocolError> {
    let protocol = request.args.protocol();
    match protocol {
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
                        protocol,
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
    request: &SessionRequest,
    host: &AgentRuntime,
) -> Result<RawSessionOutputReader, ProtocolError> {
    let HostSessionArgs::Terminal(args) = &request.args else {
        return Err(ProtocolError::InvalidArgument { message: "terminal subscription requires terminal arguments".into() });
    };
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
    request: &SessionRequest,
    host: &AgentRuntime,
) -> Result<RawSessionOutputReader, ProtocolError> {
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
    host: &AgentRuntime,
    agent_id: Uuid,
    protocol: Protocol,
) -> Result<RawPtySubscription, ProtocolError> {
    raw_pty_subscription_with(host, agent_id, protocol, prepare_raw_pty_target).await
}

async fn raw_pty_subscription_with<Prepare, Prepared>(
    host: &AgentRuntime,
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
    host: &AgentRuntime,
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
    request: &SessionRequest,
    host: &AgentRuntime,
) -> Result<(crate::agents::MultiplexStructuredReader, Option<u64>), ProtocolError> {
    let (replay_query, terminal_size) = structured_replay_query(request)?;
    let protocol = request.args.protocol();
    let log = {
        let state = host.state().read().await;
        let session = state
            .local_agents
            .get(&request.agent_id)
            .map(|context| &context.session)
            .ok_or(ProtocolError::NoAgentFound)?;
        match session.plane(protocol)? {
            Plane::Structured { log, .. } => log,
            Plane::Terminal(_) => {
                return Err(ProtocolError::ServerError {
                    message: format!("{protocol} resolved to a terminal plane"),
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
    let replay_cursor = (protocol == Protocol::ClaudePtyTranscriptV1).then_some(current_seq);
    Ok((reader, replay_cursor))
}

fn structured_replay_query(
    request: &SessionRequest,
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
    let protocol = request.args.protocol();
    match &request.args {
        HostSessionArgs::ClaudePty(args) => {
            let query = match args.replay_query {
                None => None,
                Some(ClaudePtyTranscriptV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(ClaudePtyTranscriptV1ReplayQuery::Since { seq_id }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq_id
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(protocol))?,
                    })
                }
            };
            Ok((query, args.terminal_size))
        }
        HostSessionArgs::ClaudeSdk(args) => {
            let query = match args.replay_query {
                None => None,
                Some(model::ClaudeSdkV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(model::ClaudeSdkV1ReplayQuery::Since { seq_id }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq_id
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(protocol))?,
                    })
                }
            };
            Ok((query, None))
        }
        HostSessionArgs::Codex(args) => {
            let query = match args.replay_query {
                None => None,
                Some(model::CodexSdkV1ReplayQuery::Tail { count }) => {
                    Some(crate::agents::SequencedReplayQuery::Tail { count })
                }
                Some(model::CodexSdkV1ReplayQuery::Since { seq }) => {
                    Some(crate::agents::SequencedReplayQuery::Since {
                        seq: seq
                            .checked_add(1)
                            .ok_or_else(|| out_of_range(protocol))?,
                    })
                }
            };
            Ok((query, None))
        }
        HostSessionArgs::Terminal(_) | HostSessionArgs::TestEcho => Err(ProtocolError::InvalidArgument {
            message: format!("{protocol} is not a structured protocol"),
        }),
    }
}

pub(super) async fn send_session_input(
    host: &AgentRuntime,
    request: SessionInputRequest,
    attachment_owner: Option<Arc<Owner>>,
    operation: host_api::OperationLease,
) -> Result<(), ProtocolError> {
    let protocol = request.input.protocol();
    match request.input {
        input @ (HostSessionInput::TerminalBytes(_) | HostSessionInput::TerminalControl(_)) => {
            reject_raw_attachments(attachment_owner.as_deref(), &request.pin, protocol)?;
            send_raw_session_input(
                host,
                request.agent_id,
                Protocol::TerminalV1,
                input,
                operation,
            )
            .await
        }
        HostSessionInput::ClaudePty(mut input) => {
            send_structured_session_input(
                host,
                request.agent_id,
                request.input_id,
                &mut input,
                attachment_owner.as_deref(),
                &request.pin,
                operation,
            )
            .await
        }
        HostSessionInput::ClaudeSdk(input) => {
            let (log, target) =
                structured_plane_target(host, request.agent_id, protocol).await?;
            let mut input = claude_sdk_input(input)?;
            if let Some(owner) = attachment_owner.as_deref() {
                let crate::agents::claude::sdk_io::ClaudeSdkV1Input::Prompt { text, image_blocks } =
                    &mut input
                else {
                    return Err(attachments_require_prompt(protocol));
                };
                let prepared = materialise_and_log(
                    owner,
                    text,
                    &request.pin,
                    MaterialiseBackend::ClaudeSdk,
                    &request.input_id,
                    &log,
                )
                .await?;
                *text = prepared.text;
                *image_blocks = prepared.image_blocks;
            }
            drop(operation);
            target
                .send(StructuredInputEvent::ClaudeSdk { input_id: request.input_id, input })
                .await
        }
        HostSessionInput::Codex(mut input) => {
            #[cfg(unix)]
            {
                let (log, target) =
                    structured_plane_target(host, request.agent_id, protocol).await?;
                if let Some(owner) = attachment_owner.as_deref() {
                    let model::CodexSdkInput::UserTurn {
                        input: encoded_items,
                    } = &mut input
                    else {
                        return Err(attachments_require_prompt(protocol));
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
                        &request.pin,
                        MaterialiseBackend::Codex,
                        &request.input_id,
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
                drop(operation);
                target
                    .send(StructuredInputEvent::Codex { input_id: request.input_id, input })
                    .await
            }
            #[cfg(not(unix))]
            {
                let _ = (host, input);
                Err(ProtocolError::ServerError {
                    message: "Codex agents are unavailable on this platform".to_string(),
                })
            }
        }
        input @ HostSessionInput::TestEcho(_) => {
            reject_raw_attachments(attachment_owner.as_deref(), &request.pin, protocol)?;
            send_raw_session_input(
                host,
                request.agent_id,
                Protocol::TestEchoV1,
                input,
                operation,
            )
            .await
        }
    }
}

fn claude_sdk_input(input: model::ClaudeSdkInput) -> Result<claude_sdk_io::ClaudeSdkV1Input, ProtocolError> {
    use claude_sdk_io::ClaudeSdkV1Input;
    Ok(match input {
        model::ClaudeSdkInput::Prompt { text } => ClaudeSdkV1Input::Prompt { text, image_blocks: Vec::new() },
        model::ClaudeSdkInput::Interrupt => ClaudeSdkV1Input::Interrupt,
        model::ClaudeSdkInput::SetPermissionMode { mode } => ClaudeSdkV1Input::SetPermissionMode {
            mode: serde_json::from_value(serde_json::Value::String(mode)).map_err(|error| ProtocolError::InvalidArgument { message: format!("invalid permission mode: {error}") })?,
        },
        model::ClaudeSdkInput::SetModel { model } => ClaudeSdkV1Input::SetModel { model },
        model::ClaudeSdkInput::RequestContextBreakdown => ClaudeSdkV1Input::RequestContextBreakdown,
        model::ClaudeSdkInput::ElicitationDecision { request_id, result } => ClaudeSdkV1Input::ElicitationDecision {
            request_id,
            result: serde_json::from_value(result).map_err(|error| ProtocolError::InvalidArgument { message: format!("invalid elicitation result: {error}") })?,
        },
        model::ClaudeSdkInput::DialogDecision { request_id, result } => ClaudeSdkV1Input::DialogDecision {
            request_id,
            result: serde_json::from_value(result).map_err(|error| ProtocolError::InvalidArgument { message: format!("invalid dialog result: {error}") })?,
        },
        model::ClaudeSdkInput::PermissionDecision { request_id, decision } => {
            let behavior = decision.get("behavior").and_then(serde_json::Value::as_str).ok_or_else(|| ProtocolError::InvalidArgument { message: "permission decision requires behavior".into() })?;
            let decision = match behavior {
                "allow" => claude::sdk::PermissionResult::Allow {
                    updated_input: decision.get("updatedInput").filter(|value| !value.is_null()).cloned(),
                    updated_permissions: decision.get("updatedPermissions").and_then(serde_json::Value::as_array).map(|values| values.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<_>, _>>()).transpose().map_err(|error| ProtocolError::InvalidArgument { message: format!("invalid updated permissions: {error}") })?,
                    tool_use_id: decision.get("toolUseID").and_then(serde_json::Value::as_str).map(str::to_owned),
                },
                "deny" => claude::sdk::PermissionResult::Deny {
                    message: decision.get("message").and_then(serde_json::Value::as_str).unwrap_or("User denied permission").to_owned(),
                    interrupt: decision.get("interrupt").and_then(serde_json::Value::as_bool),
                    tool_use_id: decision.get("toolUseID").and_then(serde_json::Value::as_str).map(str::to_owned),
                },
                other => return Err(ProtocolError::InvalidArgument { message: format!("unsupported permission behavior {other:?}") }),
            };
            ClaudeSdkV1Input::PermissionDecision { request_id, decision }
        }
    })
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
    host: &AgentRuntime,
    agent_id: Uuid,
    protocol: Protocol,
    input: HostSessionInput,
    operation: host_api::OperationLease,
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
    drop(operation);
    match input {
        HostSessionInput::TerminalBytes(payload) | HostSessionInput::TestEcho(payload) => {
            pty.send_input(payload)
                .await
                .map_err(|error| ProtocolError::ServerError {
                    message: error.to_string(),
                })
        }
        HostSessionInput::TerminalControl(control) => {
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
        _ => Err(ProtocolError::InvalidArgument { message: format!("{protocol} received incompatible input") }),
    }
}

async fn send_structured_session_input(
    host: &AgentRuntime,
    agent_id: Uuid,
    input_id: Vec<u8>,
    input: &mut model::ClaudePtyTranscriptV1Input,
    attachment_owner: Option<&Owner>,
    pins: &[ArtifactId],
    operation: host_api::OperationLease,
) -> Result<(), ProtocolError> {
    let (log, target) =
        structured_plane_target(host, agent_id, Protocol::ClaudePtyTranscriptV1).await?;
    send_claude_pty_to_target(
        log,
        target,
        input_id,
        input,
        attachment_owner,
        pins,
        operation,
    )
    .await
}

async fn send_claude_pty_to_target(
    log: StructuredLogSource,
    target: Box<dyn StructuredInput>,
    input_id: Vec<u8>,
    input: &mut claude_io::ClaudePtyTranscriptV1Input,
    attachment_owner: Option<&Owner>,
    pins: &[ArtifactId],
    operation: host_api::OperationLease,
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
    drop(operation);
    target
        .send(StructuredInputEvent::ClaudePty {
            client_seq: input.expected_seq,
            intent: input.intent.clone(),
        })
        .await
}

async fn structured_plane_target(
    host: &AgentRuntime,
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
) -> HostSessionStream {
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
                    Some((Ok(HostSessionEvent::Opened), next))
                }
                DirectSessionStreamState::ReplayingAttachments {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                    refs,
                } => {
                    let event = structured_output_event(
                        StructuredOutput {
                            seq: 0,
                            payload: attachments_row(None, &refs),
                        },
                    );
                    Some((
                        event.map_err(HostStreamError::from),
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
                    let event = tokio::select! {
                        biased;
                        reason = shutdown_rx.recv() => {
                            let Some(reason) = reason else {
                                return Some((
                                    Err(HostStreamError::Protocol(ProtocolError::ResourceExhausted { message: "shutdown event subscriber queue closed".into() })),
                                    DirectSessionStreamState::Done,
                                ));
                            };
                            return Some((
                                Err(HostStreamError::Shutdown(reason)),
                                DirectSessionStreamState::Done,
                            ));
                        }
                        reason = recv_close_reason_for_agent(&mut close_rx, agent_id) => {
                            match reason {
                                Ok(reason) => HostSessionEvent::Closed { reason },
                                Err(error) => {
                                    return Some((
                                        Err(HostStreamError::Protocol(error)),
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
                                        Err(HostStreamError::Protocol(error)),
                                        DirectSessionStreamState::Done,
                                    ));
                                }
                                Some(Err(error)) => HostSessionEvent::Closed {
                                    reason: SessionCloseReason::InternalError {
                                        detail: error.to_string(),
                                    },
                                },
                                None => HostSessionEvent::Closed {
                                    reason: SessionCloseReason::AgentExited {
                                        exit_code: None,
                                    },
                                },
                            }
                        }
                    };
                    let next_state = match &event {
                        HostSessionEvent::Closed { .. } => DirectSessionStreamState::Done,
                        _ => DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                        },
                    };
                    Some((Ok(event), next_state))
                }
                DirectSessionStreamState::Done => None,
            }
        },
    ))
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
) -> Option<Result<HostSessionEvent, ProtocolError>> {
    match reader {
        SessionOutputReader::Raw(raw) => raw.reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(payload) | BroadcastRead::LiveItem(payload) => {
                Ok(HostSessionEvent::Output { sequence: None, payload })
            }
            BroadcastRead::ReplayComplete => {
                Ok(HostSessionEvent::ReplayComplete { sequence: None })
            }
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
        }),
        SessionOutputReader::Structured { reader, replay_cursor, .. } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output)
            }
            BroadcastRead::ReplayComplete => Ok(HostSessionEvent::ReplayComplete {
                sequence: *replay_cursor,
            }),
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
        }),
    }
}

fn structured_output_event(
    output: StructuredOutput,
) -> Result<HostSessionEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    Ok(HostSessionEvent::Output { sequence: Some(output.seq), payload: payload_json })
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
    let host = AgentRuntime::new_with_mcp_launch_route(
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
        &SessionRequest {
            agent_id,
            args: match protocol {
                Protocol::TerminalV1 => HostSessionArgs::Terminal(model::TerminalV1Args::default()),
                Protocol::ClaudePtyTranscriptV1 => HostSessionArgs::ClaudePty(model::ClaudePtyTranscriptV1Args { terminal_size: None, replay_query: None }),
                Protocol::ClaudeSdkV1 => HostSessionArgs::ClaudeSdk(model::ClaudeSdkV1Args::default()),
                Protocol::CodexSdkV1 => HostSessionArgs::Codex(model::CodexSdkV1Args::default()),
                Protocol::TestEchoV1 => HostSessionArgs::TestEcho,
            },
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
    let host = AgentRuntime::new_with_mcp_launch_route(
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
        let host = AgentRuntime::new(Uuid::from_u128(1));
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
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay_complete = stream.next().await.unwrap().unwrap();
        let Some(wire::subscribe_session_response::Event::ReplayComplete(replay_complete)) =
            replay_complete.event
        else {
            panic!("expected replay-complete marker");
        };
        assert!(replay_complete.cursor.is_none());
    }

    #[tokio::test]
    async fn dropping_non_codex_raw_subscription_keeps_pty_alive() {
        let host = AgentRuntime::new(Uuid::from_u128(1));
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
        let host = AgentRuntime::new(Uuid::from_u128(1));
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
        let host = AgentRuntime::new(Uuid::from_u128(1));
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
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            replay_complete.event,
            Some(wire::subscribe_session_response::Event::ReplayComplete(_))
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
            id: model::id_of(b"image"),
            kind: model::ArtifactKind::Image,
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
            Some(wire::subscribe_session_response::Event::Opened(_))
        ));
        let replay = stream.next().await.unwrap().unwrap();
        let Some(wire::subscribe_session_response::Event::Output(output)) = replay.event else {
            panic!("expected attachment replay output");
        };
        let Some(wire::session_output::Output::ClaudeSdkV1(output)) = output.output else {
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
            Some(wire::subscribe_session_response::Event::ReplayComplete(_))
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
            Some(wire::subscribe_session_response::Event::Opened(_))
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
