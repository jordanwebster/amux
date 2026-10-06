//! Words for what the agent reports, in the phone's words (its
//! `ChatWords`): the shared views carry facts, and each client words them.

use ui_state::SessionState;
use ui_view::settings;

/// The model running, by the name a person reads: the name the agent's
/// snapshot gives it, else the catalogue's name for the entry it matches,
/// else its id. None when unknown.
pub(crate) fn model_words(state: &SessionState) -> Option<String> {
    if let Some(name) = &state.agent_state().model_name {
        return Some(name.clone());
    }
    let view = settings(state);
    let current = view.models.iter().find(|model| model.current)?;
    if !current.display_name.is_empty() {
        return Some(current.display_name.clone());
    }
    Some(current.value.clone()).filter(|value| !value.is_empty())
}

/// A permission or mode by the name a person reads, lowercase as the
/// terminal's own words are: the catalogue's name, else the provider's
/// value as it is.
pub(crate) fn named(display_name: &str, value: &str) -> String {
    if display_name.is_empty() {
        value.to_owned()
    } else {
        display_name.to_lowercase()
    }
}

/// The permission and mode as the composer's edge names them, "full access
/// · plan": each left unsaid while it is the agent's normal one, so the
/// edge speaks only when the agent asks less or works otherwise. Codex
/// settings that match no named permission read "custom". Nothing is said
/// until the agent's catalogue is held, which says which one is normal.
pub(crate) fn control_words(state: &SessionState) -> Option<String> {
    let agent = state.agent_state();
    let view = settings(state);
    let mut words: Vec<String> = Vec::new();
    if !agent.permissions.is_empty()
        && let Some(permission) = view
            .permissions
            .iter()
            .find(|permission| permission.current && !permission.normal)
    {
        words.push(if permission.value.is_empty() {
            "custom".to_owned()
        } else {
            named(&permission.display_name, &permission.value)
        });
    }
    if let Some(mode) = view.modes.iter().find(|mode| mode.current && !mode.normal) {
        words.push(named(&mode.display_name, &mode.value));
    }
    (!words.is_empty()).then(|| words.join(" · "))
}
