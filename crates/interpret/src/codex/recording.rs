//! Recorded app-server sessions (`codex_io`: a codex-specs recording,
//! `io.jsonl`) as interpreter events.
//!
//! What the server wrote is facts. What the recording's host wrote becomes
//! the inputs that would make this interpreter write the same: a turn with
//! text is a prompt, a steer a prompt then its send-now, an injected item an agent
//! message, an interrupt an interrupt, a compaction a `/compact` prompt, and
//! a response to a server request an answer. The handshake and the host's
//! own introspection are left out; their responses stay in as facts. An
//! empty turn after an inject is the interpreter's own kick and is left out
//! too.
//!
//! The interpreter numbers its requests `amux-<n>`, so the ids of the
//! host's requests it stands in for are renumbered the same way, in order,
//! on the requests' responses. Time is microseconds since the recording
//! began, placed on the wall clock the first `emittedAtMs` gives.

use std::collections::BTreeMap;

use codex_protocol::client::{ElicitationAction, GrantScope, InjectedContent, InjectedItem};
use codex_protocol::server::{CommandDecision, Decision as Offered};
use codex_protocol::{
    ClientMessage, ClientRequest, ClientResponse, RequestId, ServerMessage, ServerNotification,
    ServerRequest,
};
use prost::Message as _;
use serde::Deserialize;
use wire::{
    AnswerInput, Approve, CodexAnswer, CodexInput, Envelope, EnvelopeKind, FormAction, FormAnswer,
    GrantAnswer, Input, Interrupt, LinkAnswer, PromptInput, QuestionAnswer, QuestionResponse,
    codex_answer, codex_input, input,
};

use crate::{Channel, Event, Fact};

/// One line of a recording.
#[derive(Deserialize)]
struct Recorded {
    /// Microseconds since the recording began.
    #[serde(default)]
    us: i64,
    /// `stdout` for what the server wrote; anything else is the host's.
    #[serde(default)]
    dir: String,
    /// The JSON-RPC line; an exit has none.
    #[serde(default)]
    line: Option<String>,
}

/// A recorded line, read in the direction it went.
enum Line {
    Server(ServerMessage),
    Host(ClientMessage),
}

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    if format != "codex_io" {
        return Err(format!("codex reads codex_io, not {format:?}"));
    }
    let lines = std::str::from_utf8(bytes)
        .map_err(|error| error.to_string())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Recorded>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parsed = |recorded: &Recorded| {
        let line = recorded.line.as_deref()?.as_bytes();
        Some(if recorded.dir == "stdout" {
            Line::Server(codex_protocol::decode(line).ok()?)
        } else {
            Line::Host(codex_protocol::decode_client(line).ok()?)
        })
    };
    let origin = lines
        .iter()
        .find_map(|recorded| match parsed(recorded)? {
            Line::Server(ServerMessage::Notification {
                emitted_at_ms: Some(at),
                ..
            }) => Some(at - recorded.us / 1000),
            _ => None,
        })
        .unwrap_or(0);

    let mut events = Vec::new();
    let mut now = None;
    // Server requests by their key, to read the host's responses as answers.
    let mut asked = BTreeMap::<String, ServerRequest>::new();
    // Host request ids the interpreter stands in for, renumbered.
    let mut renumbered = BTreeMap::<String, String>::new();
    let mut inputs = 0;
    let mut injected_while_idle = false;
    let mut busy = false;
    for recorded in &lines {
        let Some(line) = parsed(recorded) else {
            continue;
        };
        let at_ms = origin + recorded.us / 1000;
        let mut push = |event: Event| {
            if now.is_none_or(|now| at_ms > now) {
                now = Some(at_ms);
                events.push(Event::Tick { at_ms });
            }
            events.push(event);
        };
        let message = match line {
            Line::Server(mut message) => {
                match &mut message {
                    ServerMessage::Response { id, .. } => {
                        if let Some(ours) = renumbered.get(&key(id)) {
                            *id = RequestId::String(ours.clone());
                        }
                    }
                    ServerMessage::Request { id, request, .. } => {
                        asked.insert(key(id), request.clone());
                    }
                    ServerMessage::Notification {
                        notification: ServerNotification::TurnStarted(_),
                        ..
                    } => busy = true,
                    ServerMessage::Notification {
                        notification: ServerNotification::TurnCompleted(_),
                        ..
                    } => busy = false,
                    _ => {}
                }
                push(Event::Fact(Fact {
                    channel: Channel::Rpc,
                    payload: codex_protocol::encode_server(&message),
                }));
                continue;
            }
            Line::Host(message) => message,
        };

        if let ClientMessage::Request { id, request, .. } = &message
            && matches!(
                request,
                ClientRequest::TurnStart(_)
                    | ClientRequest::TurnSteer(_)
                    | ClientRequest::TurnInterrupt(_)
                    | ClientRequest::ThreadInjectItems(_)
                    | ClientRequest::ThreadCompactStart(_)
            )
        {
            // The interpreter sends these itself.
            renumbered.insert(key(id), format!("amux-{}", renumbered.len() + 1));
        }
        inputs += 1;
        let input_id = format!("stdin-{inputs}").into_bytes();
        let arm = match message {
            ClientMessage::Request {
                request: ClientRequest::TurnStart(params),
                ..
            } => {
                let text = input_text(&params.input);
                if text.is_empty() && std::mem::take(&mut injected_while_idle) {
                    continue;
                }
                // Overrides the host put on the turn are inputs before it.
                if let Some(model) = params.model {
                    push(Event::Input(Input {
                        input_id: format!("stdin-{inputs}-model").into_bytes(),
                        of: Some(input::Of::Codex(CodexInput {
                            of: Some(codex_input::Of::Model(wire::SetModel {
                                model: Some(model),
                            })),
                        })),
                    }));
                }
                if let Some(Some(effort)) = params.effort {
                    push(Event::Input(Input {
                        input_id: format!("stdin-{inputs}-effort").into_bytes(),
                        of: Some(input::Of::Codex(CodexInput {
                            of: Some(codex_input::Of::Effort(wire::SetEffort {
                                effort: Some(effort.as_str().to_owned()),
                            })),
                        })),
                    }));
                }
                codex_input::Of::Prompt(PromptInput {
                    text,
                    ..Default::default()
                })
            }
            ClientMessage::Request {
                request: ClientRequest::TurnSteer(params),
                ..
            } => {
                let queued = format!("stdin-{inputs}-queued").into_bytes();
                push(Event::Input(Input {
                    input_id: queued.clone(),
                    of: Some(input::Of::Codex(CodexInput {
                        of: Some(codex_input::Of::Prompt(PromptInput {
                            text: input_text(&params.input),
                            ..Default::default()
                        })),
                    })),
                }));
                codex_input::Of::SendNow(wire::SendQueuedNow {
                    queued_input_id: queued,
                })
            }
            ClientMessage::Request {
                request: ClientRequest::TurnInterrupt(_),
                ..
            } => codex_input::Of::Interrupt(Interrupt {}),
            ClientMessage::Request {
                request: ClientRequest::ThreadCompactStart(_),
                ..
            } => codex_input::Of::Prompt(PromptInput {
                text: "/compact".into(),
                ..Default::default()
            }),
            ClientMessage::Request {
                request: ClientRequest::ThreadInjectItems(params),
                ..
            } => {
                injected_while_idle = !busy;
                let text = params
                    .items
                    .iter()
                    .flat_map(|item| match item {
                        InjectedItem::Message(message) => message.content.as_slice(),
                        InjectedItem::Unknown(_) => &[],
                    })
                    .map(|part| match part {
                        InjectedContent::InputText(text) => text.text.as_str(),
                        InjectedContent::Unknown(_) => "",
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                push(Event::Input(Input {
                    input_id: input_id.clone(),
                    of: Some(input::Of::AgentMessage(Envelope {
                        id: input_id,
                        kind: EnvelopeKind::Message as i32,
                        text,
                        ..Default::default()
                    })),
                }));
                continue;
            }
            ClientMessage::Response {
                id,
                response: Ok(response),
                ..
            } => {
                let key = key(&id);
                match asked
                    .get(&key)
                    .and_then(|asked| answer(asked, key, &response))
                {
                    Some(arm) => arm,
                    None => continue,
                }
            }
            _ => continue,
        };
        push(Event::Input(Input {
            input_id,
            of: Some(input::Of::Codex(CodexInput { of: Some(arm) })),
        }));
    }
    Ok(events)
}

/// A request id as the interpreter keys its asks: the JSON it was sent as.
fn key(id: &RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

fn input_text(input: &[codex_protocol::items::UserInput]) -> String {
    input
        .iter()
        .filter_map(|part| match part {
            codex_protocol::items::UserInput::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host's response to a server request, as the input that sends it.
fn answer(
    request: &ServerRequest,
    key: String,
    response: &ClientResponse,
) -> Option<codex_input::Of> {
    let body = |of: codex_answer::Of| {
        Some(codex_input::Of::Answer(AnswerInput {
            ask_key: key.clone(),
            kind: "codex".into(),
            body: CodexAnswer { of: Some(of) }.encode_to_vec(),
        }))
    };
    let approve = |decision: wire::Decision| {
        Some(codex_input::Of::Approve(Approve {
            request_id: key.clone(),
            decision: decision as i32,
        }))
    };
    match (request, response) {
        (
            ServerRequest::CommandApproval(_) | ServerRequest::FileChangeApproval(_),
            ClientResponse::CommandApproval(answer),
        ) => approve(match &answer.decision {
            CommandDecision::Plain(Offered::Accept) => wire::Decision::Approve,
            CommandDecision::Plain(Offered::AcceptForSession) => wire::Decision::ApproveSession,
            CommandDecision::Plain(Offered::Decline) => wire::Decision::Deny,
            CommandDecision::Plain(Offered::Cancel) => wire::Decision::Abort,
            CommandDecision::Plain(Offered::Other(_)) => return None,
            CommandDecision::AcceptWithExecpolicyAmendment { .. } => wire::Decision::ApproveSimilar,
            CommandDecision::ApplyNetworkPolicyAmendment { .. } => wire::Decision::ApproveNetwork,
        }),
        (ServerRequest::PermissionsApproval(_), ClientResponse::PermissionsApproval(grant)) => {
            let files = grant.permissions.file_system.clone().flatten();
            let files = files.unwrap_or_default();
            body(codex_answer::Of::Grant(GrantAnswer {
                read: files.read.unwrap_or_default(),
                write: files.write.unwrap_or_default(),
                network: grant
                    .permissions
                    .network
                    .clone()
                    .flatten()
                    .and_then(|network| network.enabled)
                    .unwrap_or(false),
                for_session: grant.scope == Some(GrantScope::Session),
            }))
        }
        (ServerRequest::RequestUserInput(params), ClientResponse::UserInput(answered)) => {
            let mut note = String::new();
            let responses = params
                .questions
                .iter()
                .map(|question| {
                    let labels = question
                        .options
                        .iter()
                        .flatten()
                        .map(|option| option.label.as_str())
                        .collect::<Vec<_>>();
                    let mut response = QuestionResponse::default();
                    for picked in answered
                        .answers
                        .get(&question.id)
                        .map(|answer| answer.answers.as_slice())
                        .unwrap_or_default()
                    {
                        if let Some(noted) = picked.strip_prefix(super::USER_NOTE) {
                            note = noted.to_owned();
                            continue;
                        }
                        match labels.iter().position(|label| label == picked) {
                            Some(index) => response.selected.push(index as u32),
                            None => response.other = Some(picked.clone()),
                        }
                    }
                    response
                })
                .collect();
            body(codex_answer::Of::Question(QuestionAnswer {
                answers: responses,
                note,
            }))
        }
        (ServerRequest::Elicitation(params), ClientResponse::Elicitation(answered)) => {
            let action = match answered.action {
                ElicitationAction::Accept => FormAction::Accept,
                ElicitationAction::Decline => FormAction::Decline,
                ElicitationAction::Cancel => FormAction::Cancel,
                ElicitationAction::Other(_) => return None,
            };
            let approval_kind = params
                .meta
                .as_ref()
                .and_then(|meta| meta.codex_approval_kind.as_deref());
            if approval_kind == Some("mcp_tool_call") {
                let session = answered
                    .meta
                    .as_ref()
                    .is_some_and(|meta| meta.persist.as_deref() == Some("session"));
                approve(match action {
                    FormAction::Accept if session => wire::Decision::ApproveSession,
                    FormAction::Accept => wire::Decision::Approve,
                    FormAction::Decline => wire::Decision::Deny,
                    _ => wire::Decision::Abort,
                })
            } else if params.mode == "url" {
                body(codex_answer::Of::Link(LinkAnswer {
                    action: action as i32,
                }))
            } else {
                body(codex_answer::Of::Form(FormAnswer {
                    action: action as i32,
                    content_json: answered
                        .content
                        .as_ref()
                        .filter(|content| !content.is_null())
                        .map(|content| content.to_string().into_bytes())
                        .unwrap_or_default(),
                }))
            }
        }
        _ => None,
    }
}
