//! Shared path-resolution helpers for config and persisted state.
//!
//! If neither the relevant XDG environment variable nor a home directory is
//! available, these helpers fall back to the relative XDG suffix (for example
//! `.local/state`) rather than a shared global temp directory.

use std::path::PathBuf;

/// Resolve an XDG base directory: `$env_var` if set, otherwise `$HOME/{default_suffix}`.
///
/// On Windows, maps XDG conventions to standard Windows directories:
/// `.config` -> `%APPDATA%`, `.local/state` -> `%LOCALAPPDATA%` (fallback `%APPDATA%`).
pub(crate) fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    xdg_dir_with_home(env_var, default_suffix, home_dir())
}

fn xdg_dir_with_home(env_var: &str, default_suffix: &str, home_dir: Option<PathBuf>) -> PathBuf {
    if let Ok(val) = std::env::var(env_var) {
        return PathBuf::from(val);
    }
    #[cfg(windows)]
    {
        let win_base = if default_suffix.starts_with(".config") {
            std::env::var("APPDATA").ok()
        } else {
            std::env::var("LOCALAPPDATA")
                .ok()
                .or_else(|| std::env::var("APPDATA").ok())
        };
        if let Some(base) = win_base {
            return PathBuf::from(base);
        }
    }
    home_dir
        .map(|home| home.join(default_suffix))
        .unwrap_or_else(|| PathBuf::from(default_suffix))
}

/// Resolve the amux application directory within an XDG base directory.
pub(crate) fn amux_xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    xdg_dir(env_var, default_suffix).join("amux")
}

/// Default state path: `$XDG_STATE_HOME/amux/state.yaml`,
/// falling back to `~/.local/state/amux/state.yaml`.
#[cfg(not(target_os = "ios"))]
pub(crate) fn default_state_path() -> PathBuf {
    amux_xdg_dir("XDG_STATE_HOME", ".local/state").join("state.yaml")
}

/// Default iOS state path under the app container.
#[cfg(target_os = "ios")]
pub(crate) fn default_state_path() -> PathBuf {
    ios_application_support_dir().join("state.yaml")
}

/// Default data directory: `$XDG_DATA_HOME/amux`,
/// falling back to `~/.local/share/amux`.
#[cfg(not(target_os = "ios"))]
pub fn default_data_dir() -> PathBuf {
    amux_xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// Default iOS data directory under the app container.
#[cfg(target_os = "ios")]
pub fn default_data_dir() -> PathBuf {
    ios_application_support_dir()
}

/// Default log path: `$XDG_STATE_HOME/amux/amux.log` (co-located with state.yaml).
#[cfg(not(target_os = "ios"))]
pub fn default_log_path() -> PathBuf {
    amux_xdg_dir("XDG_STATE_HOME", ".local/state").join("amux.log")
}

/// Default iOS log path under the app container.
#[cfg(target_os = "ios")]
pub fn default_log_path() -> PathBuf {
    ios_application_support_dir().join("amux.log")
}

#[cfg(target_os = "ios")]
fn ios_application_support_dir() -> PathBuf {
    home_dir()
        .map(|home| home.join("Library/Application Support/amux"))
        .unwrap_or_else(|| PathBuf::from("Library/Application Support/amux"))
}

/// Resolve `$HOME` when available.
#[cfg(unix)]
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Resolve the current user's home directory when available.
#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE").ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::xdg_dir_with_home;

    #[cfg(unix)]
    #[test]
    fn xdg_dir_uses_home_when_available() {
        let home = PathBuf::from("/tmp/amux-home");
        let path = xdg_dir_with_home(
            "AMUX_TEST_XDG_HOME_UNSET",
            ".local/state",
            Some(home.clone()),
        );

        assert_eq!(path, home.join(".local/state"));
    }

    #[cfg(unix)]
    #[test]
    fn xdg_dir_falls_back_to_relative_suffix_when_home_missing() {
        let path = xdg_dir_with_home("AMUX_TEST_XDG_HOME_UNSET", ".local/state", None);

        assert_eq!(path, PathBuf::from(".local/state"));
    }
}
