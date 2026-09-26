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

use prost::Message as _;
use serde_json::{Value, json};
use wire::{
    AnswerInput, Approve, CodexAnswer, CodexInput, Envelope, EnvelopeKind, FormAction, FormAnswer,
    GrantAnswer, Input, Interrupt, LinkAnswer, PromptInput, QuestionAnswer, QuestionResponse,
    codex_answer, codex_input, input,
};

use crate::claude_common::text;
use crate::{Channel, Event, Fact};

/// Requests the interpreter itself sends.
const INTERPRETER_METHODS: &[&str] = &[
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "thread/inject_items",
    "thread/compact/start",
];

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
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let us = |line: &Value| line.get("us").and_then(Value::as_i64).unwrap_or(0);
    let parsed = |line: &Value| {
        line.get("line")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
    };
    let origin = lines
        .iter()
        .find_map(|line| Some(parsed(line)?.get("emittedAtMs")?.as_i64()? - us(line) / 1000))
        .unwrap_or(0);

    let mut events = Vec::new();
    let mut now = None;
    // Server requests by id, to read the host's responses as answers.
    let mut asked = BTreeMap::<String, Value>::new();
    // Host request ids the interpreter stands in for, renumbered.
    let mut renumbered = BTreeMap::<String, String>::new();
    let mut inputs = 0;
    let mut injected_while_idle = false;
    let mut busy = false;
    for line in &lines {
        let Some(mut message) = parsed(line) else {
            continue;
        };
        let at_ms = origin + us(line) / 1000;
        let mut push = |event: Event| {
            if now.is_none_or(|now| at_ms > now) {
                now = Some(at_ms);
                events.push(Event::Tick { at_ms });
            }
            events.push(event);
        };
        let method = text(&message, "method").to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if line.get("dir").and_then(Value::as_str) == Some("stdout") {
            match (method.as_str(), message.get("id")) {
                ("", Some(id)) => {
                    if let Some(ours) = renumbered.get(&id.to_string()) {
                        message["id"] = json!(ours);
                    }
                }
                (_, Some(id)) => {
                    asked.insert(id.to_string(), message.clone());
                }
                ("turn/started", None) => busy = true,
                ("turn/completed", None) => busy = false,
                _ => {}
            }
            push(Event::Fact(Fact {
                channel: Channel::Rpc,
                payload: message.to_string().into_bytes(),
            }));
            continue;
        }

        let id = message.get("id").map(Value::to_string).unwrap_or_default();
        if INTERPRETER_METHODS.contains(&method.as_str()) {
            renumbered.insert(id.clone(), format!("amux-{}", renumbered.len() + 1));
        }
        inputs += 1;
        let input_id = format!("stdin-{inputs}").into_bytes();
        let arm = match method.as_str() {
            "turn/start" => {
                let text = input_text(&params);
                if text.is_empty() && std::mem::take(&mut injected_while_idle) {
                    continue;
                }
                // Overrides the host put on the turn are inputs before it.
                if let Some(model) = params.get("model").and_then(Value::as_str) {
                    push(Event::Input(Input {
                        input_id: format!("stdin-{inputs}-model").into_bytes(),
                        of: Some(input::Of::Codex(CodexInput {
                            of: Some(codex_input::Of::Model(wire::SetModel {
                                model: Some(model.to_owned()),
                            })),
                        })),
                    }));
                }
                if let Some(effort) = params.get("effort").and_then(Value::as_str) {
                    push(Event::Input(Input {
                        input_id: format!("stdin-{inputs}-effort").into_bytes(),
                        of: Some(input::Of::Codex(CodexInput {
                            of: Some(codex_input::Of::Effort(wire::SetEffort {
                                effort: Some(effort.to_owned()),
                            })),
                        })),
                    }));
                }
                codex_input::Of::Prompt(PromptInput {
                    text,
                    ..Default::default()
                })
            }
            "turn/steer" => {
                let queued = format!("stdin-{inputs}-queued").into_bytes();
                push(Event::Input(Input {
                    input_id: queued.clone(),
                    of: Some(input::Of::Codex(CodexInput {
                        of: Some(codex_input::Of::Prompt(PromptInput {
                            text: input_text(&params),
                            ..Default::default()
                        })),
                    })),
                }));
                codex_input::Of::SendNow(wire::SendQueuedNow {
                    queued_input_id: queued,
                })
            }
            "turn/interrupt" => codex_input::Of::Interrupt(Interrupt {}),
            "thread/compact/start" => codex_input::Of::Prompt(PromptInput {
                text: "/compact".into(),
                ..Default::default()
            }),
            "thread/inject_items" => {
                injected_while_idle = !busy;
                let text = params
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .flat_map(|item| {
                        item.get("content")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .map(|part| text(&part, "text").to_owned())
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
            "" => match answer(asked.get(&id), &message) {
                Some(arm) => arm,
                None => continue,
            },
            _ => continue,
        };
        push(Event::Input(Input {
            input_id,
            of: Some(input::Of::Codex(CodexInput { of: Some(arm) })),
        }));
    }
    Ok(events)
}

fn input_text(params: &Value) -> String {
    params
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| text(part, "type") == "text")
        .map(|part| text(part, "text").to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host's response to a server request, as the input that sends it.
fn answer(request: Option<&Value>, response: &Value) -> Option<codex_input::Of> {
    let request = request?;
    let key = request.get("id")?.to_string();
    let params = request.get("params").unwrap_or(&Value::Null);
    let result = response.get("result")?;
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
    match text(request, "method") {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            approve(match result.get("decision")? {
                Value::String(name) => match name.as_str() {
                    "accept" => wire::Decision::Approve,
                    "acceptForSession" => wire::Decision::ApproveSession,
                    "decline" => wire::Decision::Deny,
                    "cancel" => wire::Decision::Abort,
                    _ => return None,
                },
                Value::Object(object) if object.contains_key("acceptWithExecpolicyAmendment") => {
                    wire::Decision::ApproveSimilar
                }
                Value::Object(object) if object.contains_key("applyNetworkPolicyAmendment") => {
                    wire::Decision::ApproveNetwork
                }
                _ => return None,
            })
        }
        "item/permissions/requestApproval" => {
            let permissions = result.get("permissions").unwrap_or(&Value::Null);
            let files = permissions.get("fileSystem").unwrap_or(&Value::Null);
            let paths = |key: &str| {
                files
                    .get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect()
            };
            body(codex_answer::Of::Grant(GrantAnswer {
                read: paths("read"),
                write: paths("write"),
                network: permissions
                    .get("network")
                    .and_then(|network| network.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                for_session: text(result, "scope") == "session",
            }))
        }
        "item/tool/requestUserInput" => {
            let answers = result.get("answers").unwrap_or(&Value::Null);
            let responses = params
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|question| {
                    let labels = question
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|option| text(option, "label"))
                        .collect::<Vec<_>>();
                    let mut response = QuestionResponse::default();
                    for picked in answers
                        .get(text(question, "id"))
                        .and_then(|answer| answer.get("answers"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        match labels.iter().position(|label| *label == picked) {
                            Some(index) => response.selected.push(index as u32),
                            None => response.other = Some(picked.to_owned()),
                        }
                    }
                    response
                })
                .collect();
            body(codex_answer::Of::Question(QuestionAnswer {
                answers: responses,
                note: String::new(),
            }))
        }
        "mcpServer/elicitation/request" => {
            let action = match text(result, "action") {
                "accept" => FormAction::Accept,
                "decline" => FormAction::Decline,
                "cancel" => FormAction::Cancel,
                _ => return None,
            };
            let meta = params.get("_meta").unwrap_or(&Value::Null);
            if text(meta, "codex_approval_kind") == "mcp_tool_call" {
                let session = result
                    .get("_meta")
                    .is_some_and(|meta| text(meta, "persist") == "session");
                approve(match action {
                    FormAction::Accept if session => wire::Decision::ApproveSession,
                    FormAction::Accept => wire::Decision::Approve,
                    FormAction::Decline => wire::Decision::Deny,
                    _ => wire::Decision::Abort,
                })
            } else if text(params, "mode") == "url" {
                body(codex_answer::Of::Link(LinkAnswer {
                    action: action as i32,
                }))
            } else {
                body(codex_answer::Of::Form(FormAnswer {
                    action: action as i32,
                    content_json: result
                        .get("content")
                        .filter(|content| !content.is_null())
                        .map(|content| content.to_string().into_bytes())
                        .unwrap_or_default(),
                }))
            }
        }
        _ => None,
    }
}
