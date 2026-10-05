//! Finding the transcript file Claude Code keeps for a session.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a session transcript under the current project, a sibling Git
/// worktree, or an unambiguous project-directory fallback.
pub fn find_session_file(
    config_root: &Path,
    working_dir: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    if !valid_session_id(session_id) {
        return None;
    }
    let mut worktrees = vec![absolute_path(working_dir).ok()?];
    if let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(working_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        && output.status.success()
    {
        worktrees.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(PathBuf::from),
        );
    }
    worktrees.sort();
    worktrees.dedup();

    let projects = config_root.join("projects");
    if let Some(path) = worktrees.into_iter().find_map(|worktree| {
        let path = projects
            .join(project_slug(&worktree))
            .join(format!("{session_id}.jsonl"));
        path.is_file().then_some(path)
    }) {
        return Some(path);
    }

    let mut found = None;
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let path = entry.path().join(format!("{session_id}.jsonl"));
        if path.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(path);
        }
    }
    found
}

fn valid_session_id(session_id: &str) -> bool {
    uuid::Uuid::parse_str(session_id).is_ok()
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn project_slug(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn session_file_uses_the_configured_project_slug() {
        let root = TempDir::new().unwrap();
        let project = Path::new("/Users/example/project.with_under score");
        let config = root.path().join("claude");
        let session_id = "11111111-1111-1111-1111-111111111111";
        let transcript = config
            .join("projects")
            .join("-Users-example-project-with-under-score")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();

        assert_eq!(
            find_session_file(&config, project, session_id),
            Some(transcript)
        );
    }

    #[test]
    fn session_file_scans_for_one_unambiguous_fallback() {
        let root = TempDir::new().unwrap();
        let config = root.path().join("claude");
        let session_id = "22222222-2222-2222-2222-222222222222";
        let transcript = config
            .join("projects")
            .join("recorded-elsewhere")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();

        assert_eq!(
            find_session_file(
                &config,
                Path::new("/working/directory/whose-slug-is-absent"),
                session_id,
            ),
            Some(transcript.clone())
        );

        let duplicate = config
            .join("projects")
            .join("duplicate")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
        std::fs::write(duplicate, b"{}\n").unwrap();
        assert_eq!(
            find_session_file(
                &config,
                Path::new("/working/directory/whose-slug-is-absent"),
                session_id,
            ),
            None
        );
    }
}
