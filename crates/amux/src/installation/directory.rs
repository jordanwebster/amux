//! How a profile reads in a list, for every local surface.
//!
//! The CLI listing and the TUI switcher name the same profiles side by side.
//! A label that disambiguates in one and not the other, or a status worded
//! two ways, would read as two different installations.

use super::rpc;

/// A label a person can retype. Labels stay selectable without their
/// collision suffix, so an ambiguous selection can be diagnosed with the
/// full UUID while the displayed text remains copyable verbatim.
pub fn display_label(info: &rpc::ProfileInfo, directory: &[rpc::ProfileInfo]) -> String {
    if directory.iter().filter(|p| p.label == info.label).count() > 1 {
        // Extend the suffix if profiles happen to share their first eight digits.
        let mut length = 8.min(info.id.len());
        while length < info.id.len()
            && directory
                .iter()
                .any(|other| other.id != info.id && other.id.starts_with(&info.id[..length]))
        {
            length += 1;
        }
        format!("{} ({})", info.label, &info.id[..length])
    } else {
        info.label.clone()
    }
}

/// Persistent intent and observed state in one phrase, or the reason the
/// profile cannot be used at all.
pub fn status_label(profile: &rpc::ProfileInfo) -> String {
    if !profile.startup_error.is_empty() {
        return format!("unavailable: {}", profile.startup_error);
    }
    if !profile.available {
        return "unavailable".into();
    }
    let intent = rpc::Intent::try_from(profile.intent)
        .map(|v| v.as_str_name())
        .unwrap_or("unknown");
    let observed = rpc::Observed::try_from(profile.observed)
        .map(|v| v.as_str_name())
        .unwrap_or("unknown");
    format!(
        "{} / {}",
        intent.trim_start_matches("INTENT_").to_ascii_lowercase(),
        observed
            .trim_start_matches("OBSERVED_")
            .to_ascii_lowercase()
    )
}
