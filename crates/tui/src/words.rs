//! Words for what the agent reports, in the phone's words (its
//! `ChatWords`): the shared views carry facts, and each client words them.

use ui_state::SessionState;
use ui_view::{ModeValue, settings};

/// The model running, by the name a person reads: the display name the
/// agent offers for exactly that model, else its id tidied into a name.
/// An alias that stands for it ("Default (recommended)", "opus") names a
/// choice, not the model, so the id wins over it. Before the agent reports
/// a model, the offer's current choice by its display name or alias.
/// None when unknown.
pub(crate) fn model_words(state: &SessionState) -> Option<String> {
    let view = settings(state);
    let running = state.agent_state().model.as_deref().unwrap_or_default();
    if !running.is_empty() {
        let named = view
            .models
            .iter()
            .find(|model| model.value == running && !model.display_name.is_empty());
        return Some(match named {
            Some(model) => model.display_name.clone(),
            None => model_name(running),
        });
    }
    let current = view.models.iter().find(|model| model.current)?;
    if !current.display_name.is_empty() {
        return Some(current.display_name.clone());
    }
    let value = current.value.as_str();
    if value.is_empty() {
        return None;
    }
    // An alias is one plain word, capitalised; an id is tidied into a name.
    if value.chars().all(|c| c.is_ascii_lowercase()) {
        return Some(capitalised(value));
    }
    Some(model_name(value))
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// A model id read as a name when the agent offers none: "gpt-5-codex"
/// reads "GPT-5 Codex", "claude-opus-4-1-20250805" reads "Opus 4.1". The
/// provider's family prefix goes, a trailing date goes, a bracketed
/// variant goes ("claude-opus-5[1m]" reads "Opus 5"), version numbers
/// join with dots and words are capitalised. Anything else keeps its id.
pub(crate) fn model_name(id: &str) -> String {
    let base = match id.split_once('[') {
        Some((base, _)) if id.ends_with(']') && !base.is_empty() => base,
        _ => id,
    };
    let mut parts: Vec<&str> = base.split('-').filter(|part| !part.is_empty()).collect();
    if parts
        .last()
        .is_some_and(|last| last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    let numeric = |part: &str| part.chars().all(|c| c.is_ascii_digit() || c == '.');
    let mut words: Vec<String> = Vec::new();
    let mut rest = parts.as_slice();
    match rest.first() {
        // "gpt-5" is one word, "GPT-5".
        Some(&"gpt") if rest.get(1).is_some_and(|v| numeric(v)) => {
            words.push(format!("GPT-{}", rest[1]));
            rest = &rest[2..];
        }
        Some(&"claude") => rest = &rest[1..],
        _ => {}
    }
    for part in rest {
        match words.last_mut() {
            // "4-1" is version 4.1.
            Some(last)
                if numeric(part) && last.chars().last().is_some_and(|c| c.is_ascii_digit()) =>
            {
                last.push('.');
                last.push_str(part);
            }
            _ if part.starts_with('o') && part[1..].chars().all(|c| c.is_ascii_digit()) => {
                words.push((*part).to_owned())
            }
            _ => words.push(capitalised(part)),
        }
    }
    if words.is_empty() {
        id.to_owned()
    } else {
        words.join(" ")
    }
}

/// The current mode by the name a person reads ("Accept edits"). None when
/// the agent has not said.
pub(crate) fn mode_words(state: &SessionState) -> Option<String> {
    let view = settings(state);
    view.modes
        .iter()
        .find(|mode| mode.current)
        .map(|mode| mode_name(&mode.value))
}

/// A mode by the name a person reads, never the provider's identifier:
/// Claude's `acceptEdits` reads "Accept edits", Codex's `full-access`
/// preset "Full access". A mode outside the known set keeps the provider's
/// words.
pub(crate) fn mode_name(value: &ModeValue) -> String {
    match value {
        ModeValue::Claude(mode) => match mode.as_str() {
            "default" => "Default",
            "acceptEdits" => "Accept edits",
            "plan" => "Plan",
            "auto" => "Auto",
            "bypassPermissions" => "Bypass permissions",
            other => other,
        }
        .to_owned(),
        ModeValue::Codex {
            preset,
            approval_policy,
            sandbox,
        } => match preset.as_deref() {
            Some("read-only") => "Read only".to_owned(),
            Some("auto") => "Auto".to_owned(),
            Some("full-access") => "Full access".to_owned(),
            Some("plan") => "Plan".to_owned(),
            _ => format!("{approval_policy} · {sandbox}"),
        },
    }
}
