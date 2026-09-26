//! Recorded headless Claude sessions (`claude_sdk_io`: a claude-specs
//! provider recording, `io.jsonl`) as interpreter events.
//!
//! Stdout lines are facts. What the recording's host wrote to stdin becomes
//! the inputs that would make this interpreter write the same thing: user
//! messages are prompts, answers to permission and elicitation requests are
//! answers, and model, mode and interrupt requests are those inputs. The
//! host's own handshake and introspection requests are left out; their
//! responses stay in as facts. Time is microseconds since the recording
//! began, placed on the wall clock the first timestamped line uses.

use prost::Message as _;
use serde_json::Value;
use wire::{
    AnswerInput, ClaudeAnswer, ClaudeSdkInput, FormAction, FormAnswer, Input, Interrupt,
    PermissionAllow, PermissionAnswer, PermissionDeny, PlanAnswer, PlanApprove, PlanSendBack,
    PromptInput, QuestionAnswer, QuestionResponse, SetModel, SetPermissionMode, claude_answer,
    claude_sdk_input, input, permission_answer, plan_answer,
};

use crate::claude_common::{PLAN_TOOL, QUESTION_TOOL, text, timestamp_ms};
use crate::{Channel, Event, Fact};

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    if format != "claude_sdk_io" {
        return Err(format!("claude_sdk reads claude_sdk_io, not {format:?}"));
    }
    let text_bytes = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let lines = text_bytes
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
        .find_map(|line| Some(timestamp_ms(&parsed(line)?)? - us(line) / 1000))
        .unwrap_or(0);
    let mut events = Vec::new();
    let mut now = None;
    let mut requests = std::collections::BTreeMap::<String, Value>::new();
    let mut inputs = 0;
    for line in &lines {
        let Some(message) = parsed(line) else {
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
        let dir = line.get("dir").and_then(Value::as_str).unwrap_or_default();
        if dir == "stdout" {
            if text(&message, "type") == "control_request" {
                requests.insert(
                    text(&message, "request_id").to_owned(),
                    message.get("request").cloned().unwrap_or(Value::Null),
                );
            }
            push(Event::Fact(Fact {
                channel: Channel::Stream,
                payload: message.to_string().into_bytes(),
            }));
            continue;
        }
        inputs += 1;
        let id = format!("stdin-{inputs}").into_bytes();
        let arm = match text(&message, "type") {
            "user" => Some(claude_sdk_input::Of::Prompt(PromptInput {
                text: crate::claude_common::content_text(
                    message.pointer("/message/content").unwrap_or(&Value::Null),
                ),
                ..Default::default()
            })),
            "control_request" => {
                let request = message.get("request").unwrap_or(&Value::Null);
                match text(request, "subtype") {
                    "interrupt" => Some(claude_sdk_input::Of::Interrupt(Interrupt {})),
                    "set_model" => Some(claude_sdk_input::Of::Model(SetModel {
                        model: request
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })),
                    "set_permission_mode" => Some(claude_sdk_input::Of::Mode(SetPermissionMode {
                        mode: text(request, "mode").to_owned(),
                    })),
                    _ => None,
                }
            }
            "control_response" => {
                let response = message.get("response").unwrap_or(&Value::Null);
                let request_id = text(response, "request_id");
                requests
                    .get(request_id)
                    .and_then(|request| {
                        answer(request, response.get("response").unwrap_or(&Value::Null))
                    })
                    .map(|body| {
                        claude_sdk_input::Of::Answer(AnswerInput {
                            ask_key: request_id.to_owned(),
                            kind: "claude_sdk".into(),
                            body: ClaudeAnswer { of: Some(body) }.encode_to_vec(),
                        })
                    })
            }
            _ => None,
        };
        if let Some(arm) = arm {
            push(Event::Input(Input {
                input_id: id,
                of: Some(input::Of::ClaudeSdk(ClaudeSdkInput { of: Some(arm) })),
            }));
        }
    }
    Ok(events)
}

/// The answer a recorded control response gave to the request it answers.
fn answer(request: &Value, response: &Value) -> Option<claude_answer::Of> {
    match text(request, "subtype") {
        "elicitation" => {
            let action = match text(response, "action") {
                "accept" => FormAction::Accept,
                "decline" => FormAction::Decline,
                _ => FormAction::Cancel,
            };
            Some(claude_answer::Of::Form(FormAnswer {
                action: action as i32,
                content_json: response
                    .get("content")
                    .map(|content| content.to_string().into_bytes())
                    .unwrap_or_default(),
            }))
        }
        "can_use_tool" => {
            let allow = text(response, "behavior") == "allow";
            let message = text(response, "message").to_owned();
            let stop = response
                .get("interrupt")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match text(request, "tool_name") {
                PLAN_TOOL => Some(claude_answer::Of::Plan(PlanAnswer {
                    of: Some(if allow {
                        plan_answer::Of::Approve(PlanApprove {
                            auto_accept_edits: response
                                .pointer("/updatedPermissions/0/mode")
                                .and_then(Value::as_str)
                                == Some("acceptEdits"),
                        })
                    } else {
                        plan_answer::Of::SendBack(PlanSendBack { note: message })
                    }),
                })),
                QUESTION_TOOL if allow => {
                    let answers = response.pointer("/updatedInput/answers")?;
                    let questions = request.pointer("/input/questions")?.as_array()?;
                    Some(claude_answer::Of::Question(QuestionAnswer {
                        answers: questions
                            .iter()
                            .map(|question| {
                                let picked = text(answers, text(question, "question"));
                                let labels = question
                                    .get("options")
                                    .and_then(Value::as_array)
                                    .cloned()
                                    .unwrap_or_default();
                                let mut selected = Vec::new();
                                let mut other = Vec::new();
                                for pick in picked.split(", ") {
                                    match labels
                                        .iter()
                                        .position(|label| text(label, "label") == pick)
                                    {
                                        Some(index) => selected.push(index as u32),
                                        None => other.push(pick),
                                    }
                                }
                                QuestionResponse {
                                    selected,
                                    other: (!other.is_empty()).then(|| other.join(", ")),
                                }
                            })
                            .collect(),
                        note: String::new(),
                    }))
                }
                _ => Some(claude_answer::Of::Permission(PermissionAnswer {
                    of: Some(if allow {
                        let offered = request
                            .get("permission_suggestions")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let scope = response
                            .pointer("/updatedPermissions/0")
                            .and_then(|chosen| offered.iter().position(|offer| offer == chosen))
                            .map(|index| index as u32);
                        permission_answer::Of::Allow(PermissionAllow { scope })
                    } else {
                        permission_answer::Of::Deny(PermissionDeny {
                            note: message,
                            stop,
                        })
                    }),
                })),
            }
        }
        _ => None,
    }
}
