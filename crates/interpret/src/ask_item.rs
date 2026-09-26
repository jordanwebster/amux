//! Asks that are the work: a question, a tool server's form or link, or an
//! access grant. Each is its own item, written when the ask opens and
//! revised once with how it closed, so the decision row outlives the ask.
//! The kinds call these helpers so every carrier records a close the same
//! way.

use serde_json::Value;
use wire::{
    AnsweredQuestion, AskClosed, AskItem, AskOutcome, FormAction, FormAnswer, GrantAnswer,
    QuestionAnswer, QuestionAsk, ask_item,
};

/// The key of the item an ask that is the work is drawn on.
pub(crate) fn key(ask_key: &str) -> String {
    format!("ask:{ask_key}")
}

pub(crate) fn opened(ask: ask_item::Ask) -> AskItem {
    AskItem {
        ask: Some(ask),
        closed: None,
    }
}

/// Closed by a fact that did not say how: withdrawn, answered elsewhere, or
/// the provider gone.
pub(crate) fn dismissed() -> AskClosed {
    outcome(AskOutcome::Dismissed)
}

pub(crate) fn close(item: AskItem, closed: AskClosed) -> AskItem {
    AskItem {
        closed: Some(closed),
        ..item
    }
}

/// Closed with the outcome the provider reported and nothing more.
pub(crate) fn outcome(outcome: AskOutcome) -> AskClosed {
    AskClosed {
        outcome: outcome as i32,
        ..Default::default()
    }
}

/// A question answered: the picked labels and typed answers per question,
/// with a secret question's typed answer left out. None when an index names
/// no option.
pub(crate) fn answered(asked: &QuestionAsk, answer: &QuestionAnswer) -> Option<AskClosed> {
    let answers = asked
        .questions
        .iter()
        .zip(&answer.answers)
        .map(|(question, response)| {
            let picked = response
                .selected
                .iter()
                .map(|index| {
                    question
                        .options
                        .get(*index as usize)
                        .map(|option| option.label.clone())
                })
                .collect::<Option<Vec<_>>>()?;
            let hidden = question.secret && response.other.is_some();
            Some(AnsweredQuestion {
                picked,
                other: if hidden { None } else { response.other.clone() },
                hidden,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(AskClosed {
        outcome: AskOutcome::Answered as i32,
        answers,
        note: answer.note.clone(),
        ..Default::default()
    })
}

/// A form answered: sent with its field names, declined or cancelled.
pub(crate) fn form_sent(form: &FormAnswer) -> AskClosed {
    let mut closed = link_answered(form.action);
    if closed.outcome == AskOutcome::Answered as i32 {
        closed.fields = serde_json::from_slice::<Value>(&form.content_json)
            .ok()
            .and_then(|content| {
                content
                    .as_object()
                    .map(|fields| fields.keys().cloned().collect())
            })
            .unwrap_or_default();
    }
    closed
}

/// A link opened, declined or cancelled.
pub(crate) fn link_answered(action: i32) -> AskClosed {
    outcome(
        match FormAction::try_from(action).unwrap_or(FormAction::Unspecified) {
            FormAction::Accept => AskOutcome::Answered,
            FormAction::Decline => AskOutcome::Declined,
            FormAction::Cancel => AskOutcome::Cancelled,
            FormAction::Unspecified => AskOutcome::Dismissed,
        },
    )
}

/// An access grant answered: what it granted, or declined when nothing.
pub(crate) fn granted(grant: &GrantAnswer) -> AskClosed {
    if grant.read.is_empty() && grant.write.is_empty() && !grant.network {
        return outcome(AskOutcome::Declined);
    }
    AskClosed {
        outcome: AskOutcome::Answered as i32,
        grant: Some(grant.clone()),
        ..Default::default()
    }
}

/// An ask item as goldens show it: the ask was described when it opened in
/// the snapshot, so only its shape and how it closed.
pub(crate) fn describe(item: &AskItem) -> String {
    let asked = match &item.ask {
        Some(ask_item::Ask::Question(question)) => {
            format!("question×{}", question.questions.len())
        }
        Some(ask_item::Ask::Form(form)) => format!("form {}", form.server),
        Some(ask_item::Ask::Link(link)) => format!("link {}", link.server),
        Some(ask_item::Ask::Access(access)) => format!(
            "access read={:?} write={:?} network={}",
            access.read, access.write, access.network
        ),
        None => "none".into(),
    };
    let Some(closed) = &item.closed else {
        return format!("{asked} open");
    };
    let mut text = format!(
        "{asked} {}",
        AskOutcome::try_from(closed.outcome).map_or("?", |outcome| outcome.as_str_name())
    );
    if !closed.answers.is_empty() {
        let answers = closed
            .answers
            .iter()
            .map(|answer| {
                let mut parts = answer.picked.clone();
                if let Some(other) = &answer.other {
                    parts.push(Value::String(other.clone()).to_string());
                }
                if answer.hidden {
                    parts.push("(hidden)".into());
                }
                parts.join("+")
            })
            .collect::<Vec<_>>();
        text.push_str(&format!(" answers=[{}]", answers.join("; ")));
    }
    if !closed.note.is_empty() {
        text.push_str(&format!(" note={}", Value::String(closed.note.clone())));
    }
    if !closed.fields.is_empty() {
        text.push_str(&format!(" fields={:?}", closed.fields));
    }
    if let Some(grant) = &closed.grant {
        text.push_str(&format!(
            " granted read={:?} write={:?} network={}{}",
            grant.read,
            grant.write,
            grant.network,
            if grant.for_session {
                " session"
            } else {
                " turn"
            }
        ));
    }
    text
}
