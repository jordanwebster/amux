//! Session subscription and input RPCs, driven by [`AgentRuntime`].

use std::future::Future;
use std::sync::Arc;

use artifacts::Owner;
use host_api::{
    HostSessionEvent, HostSessionStream, HostStreamError, SessionInputRequest, SessionRequest,
};
use model::{
    ArtifactId, ProtocolError, ReplayFacts, SessionArgs, SessionControl, SessionInput,
    SessionOutput, ShutdownReason, StructuredRow, TerminalV1ReplayQuery,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{AgentRuntime, SharedAgentServiceState};
#[cfg(unix)]
use crate::agents::CodexRawPtyLease;
use crate::agents::claude::sdk_io as claude_sdk_io;
use crate::agents::{
    ArtifactRef, BroadcastRead, ByteReplayQuery, MaterialiseBackend, Plane, Protocol, PtyHandle,
    RawPtyTarget, SessionCloseReason, StructuredInput, StructuredInputEvent, StructuredLogSource,
    StructuredOutput, attachments_row, materialise_and_log, materialise_paths,
};

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
        host.state().clone(),
    ))
}

enum SessionOutputReader {
    Raw(RawSessionOutputReader),
    Structured {
        protocol: Protocol,
        reader: crate::agents::MultiplexStructuredReader,
        replay: ReplayFacts,
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
    match &request.args {
        SessionArgs::TerminalV1(_) => {
            let reader = prepare_direct_raw_session_subscription(request, host).await?;
            Ok(PreparedSessionSubscription {
                output: SessionOutputReader::Raw(reader),
            })
        }
        SessionArgs::ClaudePtyTranscriptV1(_)
        | SessionArgs::ClaudeSdkV1(_)
        | SessionArgs::CodexSdkV1(_) => {
            prepare_direct_structured_session_subscription(request, host)
                .await
                .map(|(reader, replay)| PreparedSessionSubscription {
                    output: SessionOutputReader::Structured {
                        protocol: request.args.protocol(),
                        reader,
                        replay,
                    },
                })
        }
        SessionArgs::TestEchoV1 => {
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
    let SessionArgs::TerminalV1(args) = &request.args else {
        return Err(ProtocolError::InvalidArgument {
            message: format!("{} is not a terminal protocol", request.args.protocol()),
        });
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
) -> Result<(crate::agents::MultiplexStructuredReader, ReplayFacts), ProtocolError> {
    let (replay_query, terminal_size) = structured_replay_query(&request.args)?;
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

    log.subscribe_with_query(replay_query)
        .await
        .ok_or(ProtocolError::NoAgentFound)
}

fn structured_replay_query(
    args: &SessionArgs,
) -> Result<
    (
        Option<crate::agents::SequencedReplayQuery>,
        Option<crate::agents::TerminalSize>,
    ),
    ProtocolError,
> {
    let (query, terminal_size) = match args {
        SessionArgs::ClaudePtyTranscriptV1(args) => (args.replay_query.clone(), args.terminal_size),
        SessionArgs::ClaudeSdkV1(args) => (args.replay_query.clone(), None),
        SessionArgs::CodexSdkV1(args) => (args.replay_query.clone(), None),
        SessionArgs::TerminalV1(_) | SessionArgs::TestEchoV1 => {
            return Err(ProtocolError::InvalidArgument {
                message: format!("{} is not a structured protocol", args.protocol()),
            });
        }
    };
    let query = query.map(crate::agents::SequencedReplayQuery::from_replay);
    Ok((query, terminal_size))
}

pub(super) async fn send_session_input(
    host: &AgentRuntime,
    request: SessionInputRequest,
    attachment_owner: Option<Arc<Owner>>,
    operation: host_api::OperationLease,
) -> Result<(), ProtocolError> {
    let protocol = request.input.protocol();
    match request.input {
        input @ (SessionInput::TerminalV1 { .. } | SessionInput::Control(_)) => {
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
        SessionInput::ClaudePtyTranscriptV1(mut input) => {
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
        SessionInput::ClaudeSdkV1(input) => {
            let (log, target) = structured_plane_target(host, request.agent_id, protocol).await?;
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
                .send(StructuredInputEvent::ClaudeSdk {
                    input_id: request.input_id,
                    input,
                })
                .await
        }
        SessionInput::CodexSdkV1(mut input) => {
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
                    .send(StructuredInputEvent::Codex {
                        input_id: request.input_id,
                        input,
                    })
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
        input @ SessionInput::TestEchoV1 { .. } => {
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

pub(crate) fn claude_sdk_input(
    input: model::ClaudeSdkInput,
) -> Result<claude_sdk_io::ClaudeSdkV1Input, ProtocolError> {
    use claude_sdk_io::ClaudeSdkV1Input;
    Ok(match input {
        model::ClaudeSdkInput::Prompt { text } => ClaudeSdkV1Input::Prompt {
            text,
            image_blocks: Vec::new(),
        },
        model::ClaudeSdkInput::Interrupt => ClaudeSdkV1Input::Interrupt,
        model::ClaudeSdkInput::SetPermissionMode { mode } => ClaudeSdkV1Input::SetPermissionMode {
            mode: serde_json::from_value(serde_json::Value::String(mode)).map_err(|error| {
                ProtocolError::InvalidArgument {
                    message: format!("invalid permission mode: {error}"),
                }
            })?,
        },
        model::ClaudeSdkInput::SetModel { model } => ClaudeSdkV1Input::SetModel { model },
        model::ClaudeSdkInput::SetEffort { effort } => ClaudeSdkV1Input::SetEffort {
            effort: effort
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value)).map_err(|error| {
                        ProtocolError::InvalidArgument {
                            message: format!("invalid effort: {error}"),
                        }
                    })
                })
                .transpose()?,
        },
        model::ClaudeSdkInput::RequestContextBreakdown => ClaudeSdkV1Input::RequestContextBreakdown,
        model::ClaudeSdkInput::ElicitationDecision { request_id, result } => {
            ClaudeSdkV1Input::ElicitationDecision {
                request_id,
                result: serde_json::from_value(result).map_err(|error| {
                    ProtocolError::InvalidArgument {
                        message: format!("invalid elicitation result: {error}"),
                    }
                })?,
            }
        }
        model::ClaudeSdkInput::DialogDecision { request_id, result } => {
            ClaudeSdkV1Input::DialogDecision {
                request_id,
                result: serde_json::from_value(result).map_err(|error| {
                    ProtocolError::InvalidArgument {
                        message: format!("invalid dialog result: {error}"),
                    }
                })?,
            }
        }
        model::ClaudeSdkInput::PermissionDecision {
            request_id,
            decision,
        } => {
            let behavior = decision
                .get("behavior")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ProtocolError::InvalidArgument {
                    message: "permission decision requires behavior".into(),
                })?;
            let decision = match behavior {
                "allow" => claude::sdk::PermissionResult::Allow {
                    updated_input: decision
                        .get("updatedInput")
                        .filter(|value| !value.is_null())
                        .cloned(),
                    updated_permissions: decision
                        .get("updatedPermissions")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .cloned()
                                .map(serde_json::from_value)
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()
                        .map_err(|error| ProtocolError::InvalidArgument {
                            message: format!("invalid updated permissions: {error}"),
                        })?,
                    tool_use_id: decision
                        .get("toolUseID")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                },
                "deny" => claude::sdk::PermissionResult::Deny {
                    message: decision
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("User denied permission")
                        .to_owned(),
                    interrupt: decision
                        .get("interrupt")
                        .and_then(serde_json::Value::as_bool),
                    tool_use_id: decision
                        .get("toolUseID")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                },
                other => {
                    return Err(ProtocolError::InvalidArgument {
                        message: format!("unsupported permission behavior {other:?}"),
                    });
                }
            };
            ClaudeSdkV1Input::PermissionDecision {
                request_id,
                decision,
            }
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
    input: SessionInput,
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
        SessionInput::TerminalV1 { payload } | SessionInput::TestEchoV1 { payload } => pty
            .send_input(payload)
            .await
            .map_err(|error| ProtocolError::ServerError {
                message: error.to_string(),
            }),
        SessionInput::Control(control) => match control {
            SessionControl::Resize(size) => {
                pty.resize(size)
                    .await
                    .map_err(|error| ProtocolError::ServerError {
                        message: error.to_string(),
                    })
            }
        },
        _ => Err(ProtocolError::InvalidArgument {
            message: format!("`{protocol}` input does not belong to a raw session"),
        }),
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
    input: &mut model::ClaudePtyTranscriptV1Input,
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
        let model::ClaudePtyIntent::Prompt { text } = &mut input.intent else {
            return Err(attachments_require_prompt(Protocol::ClaudePtyTranscriptV1));
        };
        let prepared = materialise_and_log(
            owner,
            text.as_str(),
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
            pins: pins.to_vec(),
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
        agents: SharedAgentServiceState,
    },
    ReplayingAttachments {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
        refs: Vec<ArtifactRef>,
        agents: SharedAgentServiceState,
    },
    Reading {
        agent_id: Uuid,
        reader: SessionOutputReader,
        close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
        shutdown_rx: mpsc::Receiver<ShutdownReason>,
        agents: SharedAgentServiceState,
    },
    Done,
}

/// Read the backend's exit code while the agent is still registered. The
/// output stream itself only signals that it ended, so this preserves a real
/// process status instead of turning every exit into an unknown one.
async fn exit_code_for_agent(agents: &SharedAgentServiceState, agent_id: Uuid) -> Option<i32> {
    let state = agents.read().await;
    state
        .local_agents
        .get(&agent_id)
        .and_then(|context| context.session.exit_code())
}

fn direct_session_response_stream(
    agent_id: Uuid,
    reader: SessionOutputReader,
    close_rx: mpsc::Receiver<(Uuid, SessionCloseReason)>,
    shutdown_rx: mpsc::Receiver<ShutdownReason>,
    replay_attachments: Option<Vec<ArtifactRef>>,
    agents: SharedAgentServiceState,
) -> HostSessionStream {
    Box::pin(futures_util::stream::unfold(
        DirectSessionStreamState::Opening {
            agent_id,
            reader,
            close_rx,
            shutdown_rx,
            replay_attachments,
            agents,
        },
        |state| async move {
            match state {
                DirectSessionStreamState::Opening {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                    replay_attachments,
                    agents,
                } => {
                    let replay = reader.replay_facts();
                    let next = match (replay_attachments, &reader) {
                        (Some(refs), SessionOutputReader::Structured { .. }) => {
                            DirectSessionStreamState::ReplayingAttachments {
                                agent_id,
                                reader,
                                close_rx,
                                shutdown_rx,
                                refs,
                                agents,
                            }
                        }
                        _ => DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                            agents,
                        },
                    };
                    Some((Ok(HostSessionEvent::Opened { replay }), next))
                }
                DirectSessionStreamState::ReplayingAttachments {
                    agent_id,
                    reader,
                    close_rx,
                    shutdown_rx,
                    refs,
                    agents,
                } => {
                    let protocol = reader.protocol();
                    let event = structured_output_event(
                        StructuredOutput {
                            seq: 0,
                            published_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                            activity_at_unix_ms: None,
                            historical: true,
                            payload: attachments_row(None, &refs),
                            encoded_len: 0,
                        },
                        protocol,
                    );
                    Some((
                        event.map_err(HostStreamError::from),
                        DirectSessionStreamState::Reading {
                            agent_id,
                            reader,
                            close_rx,
                            shutdown_rx,
                            agents,
                        },
                    ))
                }
                DirectSessionStreamState::Reading {
                    agent_id,
                    mut reader,
                    mut close_rx,
                    mut shutdown_rx,
                    agents,
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
                                        exit_code: exit_code_for_agent(&agents, agent_id).await,
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
                            agents,
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

    fn replay_facts(&self) -> Option<ReplayFacts> {
        match self {
            Self::Raw(_) => None,
            Self::Structured { replay, .. } => Some(replay.clone()),
        }
    }
}

async fn read_session_output_event(
    reader: &mut SessionOutputReader,
) -> Option<Result<HostSessionEvent, ProtocolError>> {
    match reader {
        SessionOutputReader::Raw(raw) => raw.reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(payload) | BroadcastRead::LiveItem(payload) => {
                let output = match raw.protocol {
                    Protocol::TerminalV1 => SessionOutput::TerminalV1 { payload },
                    Protocol::TestEchoV1 => SessionOutput::TestEchoV1 { payload },
                    protocol => {
                        return Err(ProtocolError::ServerError {
                            message: format!("{protocol} cannot emit raw output"),
                        });
                    }
                };
                Ok(HostSessionEvent::Output(output))
            }
            BroadcastRead::ReplayComplete => Ok(HostSessionEvent::ReplayComplete),
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
            BroadcastRead::Reset => Ok(HostSessionEvent::Closed {
                reason: SessionCloseReason::Reset,
            }),
        }),
        SessionOutputReader::Structured {
            protocol, reader, ..
        } => reader.read_event().await.map(|event| match event {
            BroadcastRead::ReplayItem(output) | BroadcastRead::LiveItem(output) => {
                structured_output_event(output, *protocol)
            }
            BroadcastRead::ReplayComplete => Ok(HostSessionEvent::ReplayComplete),
            BroadcastRead::Lagged => Err(ProtocolError::ResourceExhausted {
                message: "session output subscriber queue closed".to_string(),
            }),
            BroadcastRead::Reset => Ok(HostSessionEvent::Closed {
                reason: SessionCloseReason::Reset,
            }),
        }),
    }
}

fn structured_output_event(
    output: StructuredOutput,
    protocol: Protocol,
) -> Result<HostSessionEvent, ProtocolError> {
    let payload_json =
        serde_json::to_vec(&output.payload).map_err(|error| ProtocolError::ServerError {
            message: format!("failed to encode transcript SubscribeSession output: {error}"),
        })?;
    let row = StructuredRow {
        seq: output.seq,
        published_at_unix_ms: output.published_at_unix_ms,
        activity_at_unix_ms: output.activity_at_unix_ms,
        historical: output.historical,
        payload: payload_json,
    };
    let output = match protocol {
        Protocol::ClaudePtyTranscriptV1 => SessionOutput::ClaudePtyTranscriptV1(row),
        Protocol::ClaudeSdkV1 => SessionOutput::ClaudeSdkV1(row),
        Protocol::CodexSdkV1 => SessionOutput::CodexSdkV1(row),
        Protocol::TerminalV1 | Protocol::TestEchoV1 => {
            return Err(ProtocolError::ServerError {
                message: format!("{protocol} cannot encode structured output"),
            });
        }
    };
    Ok(HostSessionEvent::Output(output))
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
            SessionRequest {
                agent_id,
                args: SessionArgs::CodexSdkV1(Default::default()),
            },
            None,
        )
        .await
        .unwrap();
        let opened = stream.next().await.unwrap().unwrap();
        assert_eq!(
            opened,
            HostSessionEvent::Opened {
                replay: Some(ReplayFacts {
                    retained_from: 0,
                    through: 0,
                    selected_from: 0,
                    reset_at: 0,
                    outcome: model::ReplayOutcome::Continuous,
                })
            }
        );
        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(replay_complete, HostSessionEvent::ReplayComplete));
    }

    #[tokio::test]
    async fn replay_rpc_emits_opened_facts_before_rows() {
        let cases = [
            (
                SessionArgs::ClaudePtyTranscriptV1(model::ClaudePtyTranscriptV1Args {
                    terminal_size: None,
                    replay_query: Some(model::ReplayQuery::After {
                        after: 1,
                        tail_bound: Some(2),
                    }),
                }),
                ReplayFacts {
                    retained_from: 3,
                    through: 5,
                    selected_from: 4,
                    reset_at: 0,
                    outcome: model::ReplayOutcome::Truncated { missing_after: 1 },
                },
            ),
            (
                SessionArgs::ClaudeSdkV1(model::ClaudeSdkV1Args {
                    replay_query: Some(model::ReplayQuery::After {
                        after: 3,
                        tail_bound: Some(2),
                    }),
                }),
                ReplayFacts {
                    retained_from: 3,
                    through: 5,
                    selected_from: 4,
                    reset_at: 0,
                    outcome: model::ReplayOutcome::Continuous,
                },
            ),
            (
                SessionArgs::CodexSdkV1(model::CodexSdkV1Args {
                    replay_query: Some(model::ReplayQuery::TailCount {
                        count: 2,
                        tail_bound: None,
                    }),
                }),
                ReplayFacts {
                    retained_from: 3,
                    through: 5,
                    selected_from: 4,
                    reset_at: 0,
                    outcome: model::ReplayOutcome::Truncated { missing_after: 3 },
                },
            ),
        ];

        for (args, expected) in cases {
            let protocol = args.protocol();
            let request = SessionRequest {
                agent_id: Uuid::from_u128(90),
                args,
            };
            let (query, terminal_size) = structured_replay_query(&request.args).unwrap();
            assert!(terminal_size.is_none());

            let log = StructuredLogSource::new(3);
            for seq in 1..=5 {
                log.write(serde_json::json!({"type": "row", "seq": seq}))
                    .await;
            }
            let (reader, replay) = log.subscribe_with_query(query).await.unwrap();
            assert_eq!(replay, expected);

            let (_close_tx, close_rx) = mpsc::channel(1);
            let (_shutdown_tx, shutdown_rx) = mpsc::channel(1);
            let mut stream = direct_session_response_stream(
                request.agent_id,
                SessionOutputReader::Structured {
                    protocol,
                    reader,
                    replay,
                },
                close_rx,
                shutdown_rx,
                None,
                AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
            );

            let opened = stream.next().await.unwrap().unwrap();
            assert_eq!(
                opened,
                HostSessionEvent::Opened {
                    replay: Some(expected)
                },
                "{protocol} did not open before replaying rows"
            );
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                HostSessionEvent::Output(_)
            ));
        }
    }

    #[tokio::test]
    async fn daemon_protocol_row_facts_are_identical_for_live_and_replay_observers() {
        let agent_id = Uuid::from_u128(92);
        let log = StructuredLogSource::new(8);
        let (live_reader, live_replay) = log.subscribe_with_query(None).await.unwrap();
        let (_live_close_tx, live_close_rx) = mpsc::channel(1);
        let (_live_shutdown_tx, live_shutdown_rx) = mpsc::channel(1);
        let mut live = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Structured {
                protocol: Protocol::ClaudeSdkV1,
                reader: live_reader,
                replay: live_replay,
            },
            live_close_rx,
            live_shutdown_rx,
            None,
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );
        assert!(matches!(
            live.next().await.unwrap().unwrap(),
            HostSessionEvent::Opened { .. }
        ));
        assert_eq!(
            live.next().await.unwrap().unwrap(),
            HostSessionEvent::ReplayComplete
        );

        log.write_row(
            serde_json::json!({"type": "assistant", "uuid": "same-row"}),
            Some(1_736_942_400_123),
            true,
        )
        .await;
        let HostSessionEvent::Output(SessionOutput::ClaudeSdkV1(live_row)) =
            live.next().await.unwrap().unwrap()
        else {
            panic!("live observer did not receive the structured row");
        };

        let (replay_reader, replay_facts) = log.subscribe_with_query(None).await.unwrap();
        let (_replay_close_tx, replay_close_rx) = mpsc::channel(1);
        let (_replay_shutdown_tx, replay_shutdown_rx) = mpsc::channel(1);
        let mut replay = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Structured {
                protocol: Protocol::ClaudeSdkV1,
                reader: replay_reader,
                replay: replay_facts,
            },
            replay_close_rx,
            replay_shutdown_rx,
            None,
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );
        assert!(matches!(
            replay.next().await.unwrap().unwrap(),
            HostSessionEvent::Opened { .. }
        ));
        let HostSessionEvent::Output(SessionOutput::ClaudeSdkV1(replayed_row)) =
            replay.next().await.unwrap().unwrap()
        else {
            panic!("replay observer did not receive the structured row");
        };

        assert_eq!(live_row.seq, replayed_row.seq);
        assert_eq!(
            live_row.activity_at_unix_ms,
            replayed_row.activity_at_unix_ms
        );
        assert_eq!(live_row.historical, replayed_row.historical);
        assert_eq!(replayed_row.activity_at_unix_ms, Some(1_736_942_400_123));
        assert!(replayed_row.historical);
    }

    #[tokio::test]
    async fn daemon_protocol_semantic_reset_is_a_session_closed_reset_event() {
        let agent_id = Uuid::from_u128(91);
        let log = StructuredLogSource::new(8);
        let (reader, replay) = log.subscribe_with_query(None).await.unwrap();
        let (_close_tx, close_rx) = mpsc::channel(1);
        let (_shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let mut stream = direct_session_response_stream(
            agent_id,
            SessionOutputReader::Structured {
                protocol: Protocol::ClaudeSdkV1,
                reader,
                replay,
            },
            close_rx,
            shutdown_rx,
            None,
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );

        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            HostSessionEvent::Opened { .. }
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            HostSessionEvent::ReplayComplete
        ));
        log.semantic_reset(serde_json::json!({"type": "conversation_reset"}))
            .await;
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            HostSessionEvent::Closed {
                reason: SessionCloseReason::Reset
            }
        );
        assert!(stream.next().await.is_none());
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
            SessionRequest {
                agent_id,
                args: SessionArgs::TerminalV1(Default::default()),
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
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );

        for _ in 0..300 {
            buffer.write(b"x".to_vec()).await;
        }

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(opened, HostSessionEvent::Opened { replay: None }));
        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(replay_complete, HostSessionEvent::ReplayComplete));

        let mut saw_resource_exhausted = false;
        for _ in 0..300 {
            match stream
                .next()
                .await
                .expect("session stream ended before lag error")
            {
                Ok(_) => {}
                Err(HostStreamError::Protocol(ProtocolError::ResourceExhausted { .. })) => {
                    saw_resource_exhausted = true;
                    break;
                }
                Err(error) => panic!("unexpected stream error: {error}"),
            }
        }
        assert!(saw_resource_exhausted);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn structured_session_replays_pinned_refs_immediately_after_opened() {
        let agent_id = Uuid::from_u128(9);
        let log = StructuredLogSource::new(8);
        let (reader, replay) = log.subscribe_with_query(None).await.unwrap();
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
                replay,
            },
            close_rx,
            shutdown_rx,
            Some(vec![artifact.clone()]),
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(opened, HostSessionEvent::Opened { .. }));
        let replay = stream.next().await.unwrap().unwrap();
        let HostSessionEvent::Output(SessionOutput::ClaudeSdkV1(row)) = replay else {
            panic!("expected attachment replay output");
        };
        assert_eq!(row.seq, 0);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&row.payload).unwrap(),
            attachments_row(None, &[artifact])
        );

        let replay_complete = stream.next().await.unwrap().unwrap();
        assert!(matches!(replay_complete, HostSessionEvent::ReplayComplete));
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
            AgentRuntime::new(Uuid::from_u128(1)).state().clone(),
        );

        let opened = stream.next().await.unwrap().unwrap();
        assert!(matches!(opened, HostSessionEvent::Opened { .. }));

        shutdown_tx
            .send(ShutdownReason::Suspending)
            .await
            .expect("shutdown receiver should be active");
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(matches!(
            error,
            HostStreamError::Shutdown(ShutdownReason::Suspending)
        ));
        assert!(stream.next().await.is_none());
    }
}
