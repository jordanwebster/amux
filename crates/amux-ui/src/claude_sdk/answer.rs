//! Encode a person's complete answer without terminal menu assumptions.

use serde_json::{Value, json};

use super::{
    Ask, AskKind, ClaudeSdkInput, DialogAnswer, ElicitationAnswer, ElicitationFieldKind,
    ElicitationForm, PermissionAnswer, PlanAnswer, SdkAnswer,
};

pub(super) fn encode(ask: &Ask, answer: &SdkAnswer) -> Result<ClaudeSdkInput, String> {
    let request_id = ask.request_id.clone();
    let allow = |input: Value, updates: Vec<Value>| json!({"behavior":"allow", "updatedInput":input, "updatedPermissions":updates});
    let deny = |feedback: &str| json!({"behavior":"deny", "message":feedback});
    let decision = match (&ask.kind, answer) {
        (AskKind::Permission { .. }, SdkAnswer::Permission(answer)) => match answer {
            PermissionAnswer::AllowOnce => allow(ask.input.clone(), vec![]),
            PermissionAnswer::AllowScoped { suggestion } => {
                let update = ask
                    .suggestions
                    .get(*suggestion)
                    .ok_or("permission suggestion is unavailable")?;
                allow(ask.input.clone(), vec![update.clone()])
            }
            PermissionAnswer::Deny { feedback } => {
                deny(feedback.as_deref().unwrap_or("User denied permission"))
            }
        },
        (AskKind::Plan { .. }, SdkAnswer::Plan(answer)) => match answer {
            PlanAnswer::ApproveAuto | PlanAnswer::ApproveManual => allow(
                ask.input.clone(),
                vec![
                    json!({"type":"setMode", "destination":"session", "mode":if matches!(answer, PlanAnswer::ApproveAuto) {"acceptEdits"} else {"default"}}),
                ],
            ),
            PlanAnswer::RequestChanges { feedback } => {
                if feedback.trim().is_empty() {
                    return Err("request-changes feedback must not be empty".into());
                }
                deny(feedback)
            }
        },
        (AskKind::Question { questions }, SdkAnswer::Question(answers)) => {
            if questions.is_empty() || questions.len() != answers.len() {
                return Err("answer every question".into());
            }
            let mut values = serde_json::Map::new();
            for (question, answer) in questions.iter().zip(answers) {
                let key = question
                    .question
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or("question has no text")?;
                let mut selected = std::collections::BTreeSet::new();
                let mut labels = Vec::new();
                for index in &answer.selected {
                    if !selected.insert(index) {
                        return Err("question option is selected twice".into());
                    }
                    let label = question
                        .options
                        .get(*index)
                        .map(|o| o.label.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or("question option is unavailable")?;
                    labels.push(label.to_owned());
                }
                if let Some(other) = &answer.other {
                    if other.trim().is_empty() {
                        return Err("other answer must not be empty".into());
                    }
                    labels.push(other.clone());
                }
                if labels.is_empty() || (!question.multi_select && labels.len() != 1) {
                    return Err("question selection does not fit the ask".into());
                }
                if values
                    .insert(key.to_owned(), Value::String(labels.join(", ")))
                    .is_some()
                {
                    return Err("question texts must be distinct".into());
                }
            }
            let mut input = ask.input.clone();
            input
                .as_object_mut()
                .ok_or("question input is not an object")?
                .insert("answers".into(), Value::Object(values));
            allow(input, vec![])
        }
        (AskKind::Elicitation { form, .. }, SdkAnswer::Elicitation(answer)) => {
            let result = match answer {
                ElicitationAnswer::Accept { content } => {
                    validate_form(form, content)?;
                    json!({"action":"accept", "content":content})
                }
                ElicitationAnswer::Decline => json!({"action":"decline"}),
                ElicitationAnswer::Cancel => json!({"action":"cancel"}),
            };
            return Ok(ClaudeSdkInput::ElicitationDecision { request_id, result });
        }
        (AskKind::Dialog { payload, .. }, SdkAnswer::Dialog(answer)) => {
            let result = match answer {
                DialogAnswer::Cancel => json!({"behavior":"cancelled"}),
                DialogAnswer::Choose { option } => {
                    let options = payload["options"].as_array().filter(|options| {
                        !options.is_empty()
                            && options
                                .iter()
                                .all(|o| o["label"].as_str().is_some_and(|s| !s.is_empty()))
                    });
                    if !payload["message"].is_string() {
                        return Err("dialog payload cannot be answered from chat".into());
                    }
                    let chosen = options
                        .and_then(|o| o.get(*option))
                        .ok_or("dialog option is unavailable")?;
                    json!({"behavior":"completed", "result":chosen})
                }
            };
            return Ok(ClaudeSdkInput::DialogDecision { request_id, result });
        }
        _ => return Err("answer does not fit the ask".into()),
    };
    let input = ClaudeSdkInput::PermissionDecision {
        request_id,
        decision,
    };
    input
        .clone()
        .into_native()
        .map_err(|e| format!("permission update cannot be sent: {e}"))?;
    Ok(input)
}

fn validate_form(form: &ElicitationForm, content: &Value) -> Result<(), String> {
    let fields = match form {
        ElicitationForm::Fields(fields) => fields,
        ElicitationForm::Unsupported { reason } => return Err(reason.clone()),
    };
    let object = content.as_object().ok_or("form answer must be an object")?;
    for name in object.keys() {
        if !fields.iter().any(|f| &f.name == name) {
            return Err(format!("unknown form field: {name}"));
        }
    }
    for field in fields {
        let Some(value) = object.get(&field.name) else {
            if field.required {
                return Err(format!("missing required field: {}", field.name));
            }
            continue;
        };
        let valid = match &field.kind {
            ElicitationFieldKind::String => value.is_string(),
            ElicitationFieldKind::Number => value.is_number(),
            ElicitationFieldKind::Integer => value.is_i64() || value.is_u64(),
            ElicitationFieldKind::Boolean => value.is_boolean(),
            ElicitationFieldKind::Enum(choices) => choices.contains(value),
        };
        if !valid {
            return Err(format!("invalid form field: {}", field.name));
        }
    }
    Ok(())
}
