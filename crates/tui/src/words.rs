//! Words for what the agent reports, in the phone's words (its
//! `ChatWords`): the shared views carry facts, and each client words them.

use ui_state::SessionState;
use ui_view::{ModeValue, settings};

/// The current model by the name a person reads: the display name the
/// agent offers for it, else a bare alias capitalised (`opus` reads
/// "Opus"), else its id as the agent reports it. None when unknown.
pub(crate) fn model_words(state: &SessionState) -> Option<String> {
    let view = settings(state);
    let current = view.models.iter().find(|model| model.current)?;
    if !current.display_name.is_empty() {
        return Some(current.display_name.clone());
    }
    let value = current.value.as_str();
    if value.is_empty() {
        return None;
    }
    // An alias is one plain word; an id with digits or dashes is kept as is.
    if value.chars().all(|c| c.is_ascii_lowercase()) {
        let mut chars = value.chars();
        let first = chars.next()?;
        return Some(first.to_ascii_uppercase().to_string() + chars.as_str());
    }
    Some(value.to_owned())
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
            _ => format!("{approval_policy} · {sandbox}"),
        },
    }
}
