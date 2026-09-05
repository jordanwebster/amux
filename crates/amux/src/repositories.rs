//! Host-owned project discovery and the client-facing repository list.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::wire;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRepositoriesRequest {
    pub host: crate::HostId,
    pub query: Option<String>,
    /// Maximum total entries, with recent projects first. Zero returns no entries.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRepositoriesResponse {
    pub recent: Vec<ProjectEntry>,
    pub repositories: Vec<ProjectEntry>,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub path: PathBuf,
    pub name: String,
    pub last_used: Option<DateTime<Utc>>,
}

impl From<ListRepositoriesResponse> for wire::ListRepositoriesResponse {
    fn from(value: ListRepositoriesResponse) -> Self {
        fn entry(value: ProjectEntry) -> wire::ProjectEntry {
            wire::ProjectEntry {
                path: value.path.to_string_lossy().into_owned(),
                name: value.name,
                last_used_unix_ms: value.last_used.map(|time| time.timestamp_millis()),
            }
        }
        Self {
            recent: value.recent.into_iter().map(entry).collect(),
            repositories: value.repositories.into_iter().map(entry).collect(),
            roots: value
                .roots
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        }
    }
}

impl TryFrom<wire::ListRepositoriesResponse> for ListRepositoriesResponse {
    type Error = String;

    fn try_from(value: wire::ListRepositoriesResponse) -> Result<Self, Self::Error> {
        fn entry(value: wire::ProjectEntry) -> Result<ProjectEntry, String> {
            Ok(ProjectEntry {
                path: value.path.into(),
                name: value.name,
                last_used: value
                    .last_used_unix_ms
                    .map(|time| {
                        DateTime::from_timestamp_millis(time)
                            .ok_or_else(|| "invalid ProjectEntry.last_used_unix_ms".to_owned())
                    })
                    .transpose()?,
            })
        }
        Ok(Self {
            recent: value
                .recent
                .into_iter()
                .map(entry)
                .collect::<Result<_, _>>()?,
            repositories: value
                .repositories
                .into_iter()
                .map(entry)
                .collect::<Result<_, _>>()?,
            roots: value.roots.into_iter().map(PathBuf::from).collect(),
        })
    }
}

#[cfg(feature = "local-agents")]
pub(crate) mod host {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    const MAX_PROJECTS: usize = 200;

    pub(crate) struct RecentProjects {
        path: PathBuf,
        entries: Vec<ProjectEntry>,
    }

    impl RecentProjects {
        pub(crate) fn load(data_dir: &Path) -> Self {
            let path = data_dir.join("recent-projects.json");
            let entries = match std::fs::read(&path) {
                Ok(bytes) => match serde_json::from_slice(&bytes) {
                    Ok(entries) => entries,
                    Err(error) => {
                        tracing::warn!(%error, "cannot read recent projects; starting empty");
                        Vec::new()
                    }
                },
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(%error, "cannot read recent projects; starting empty");
                    }
                    Vec::new()
                }
            };
            Self { path, entries }
        }

        pub(crate) fn record(&mut self, path: &Path, created_at: DateTime<Utc>) {
            let Ok(path) = path.canonicalize() else {
                return;
            };
            if !path.is_dir() || path.to_str().is_none() {
                return;
            }
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
                entry.last_used = Some(entry.last_used.unwrap_or(created_at).max(created_at));
            } else {
                self.entries.push(project(path, Some(created_at)));
            }
            self.entries
                .sort_by(|a, b| b.last_used.cmp(&a.last_used).then(a.path.cmp(&b.path)));
            self.entries.truncate(MAX_PROJECTS);
            let bytes = serde_json::to_vec(&self.entries).expect("project entries serialize");
            // Discovery history must not turn a successfully started agent into a failed creation.
            if let Err(error) = crate::identity::atomic_replace_private(&self.path, &bytes) {
                tracing::warn!(%error, "cannot save recent projects");
            }
        }

        pub(crate) fn snapshot(&self) -> Vec<ProjectEntry> {
            self.entries.clone()
        }
    }

    fn project(path: PathBuf, last_used: Option<DateTime<Utc>>) -> ProjectEntry {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        ProjectEntry {
            path,
            name,
            last_used,
        }
    }

    pub(crate) fn list(
        roots: Vec<PathBuf>,
        recent: Vec<ProjectEntry>,
        query: Option<String>,
        limit: u32,
    ) -> ListRepositoriesResponse {
        let limit = (limit as usize).min(MAX_PROJECTS);
        let query = query.unwrap_or_default().to_lowercase();
        let matches = |entry: &ProjectEntry| {
            entry.name.to_lowercase().contains(&query)
                || entry.path.to_string_lossy().to_lowercase().contains(&query)
        };
        let roots: Vec<_> = roots
            .into_iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| root.is_dir() && root.to_str().is_some())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut seen = BTreeSet::new();
        let recent: Vec<_> = recent
            .into_iter()
            .filter_map(|entry| {
                let path = entry.path.canonicalize().ok()?;
                // A former project replaced with a symlink does not authorize a new directory.
                if path != entry.path || !path.is_dir() {
                    return None;
                }
                Some(project(path, entry.last_used))
            })
            .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
            .take(limit)
            .collect();
        let mut repositories = Vec::new();
        let mut visited = BTreeSet::new();
        let mut pending: BTreeSet<_> = roots.iter().cloned().collect();
        while recent.len() + repositories.len() < limit {
            let Some(path) = pending.pop_first() else {
                break;
            };
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if path.to_str().is_none()
                || !roots.iter().any(|root| path.starts_with(root))
                || !visited.insert(path.clone())
            {
                continue;
            }
            // A worktree has a .git file; an ordinary checkout has a .git directory.
            // Stop at repository boundaries so dependencies and Git internals are not searched.
            if path.join(".git").is_file() || path.join(".git").is_dir() {
                let entry = project(path, None);
                if matches(&entry) && seen.insert(entry.path.clone()) {
                    repositories.push(entry);
                }
                continue;
            }
            let Ok(children) = std::fs::read_dir(path) else {
                continue;
            };
            for child in children.flatten() {
                if child.file_name() != ".git" && child.file_type().is_ok_and(|kind| kind.is_dir())
                {
                    pending.insert(child.path());
                }
            }
        }
        ListRepositoriesResponse {
            recent,
            repositories,
            roots,
        }
    }
}

#[cfg(all(test, feature = "local-agents"))]
mod tests {
    use super::host::{RecentProjects, list};
    use super::*;

    #[test]
    fn repositories_history_keeps_latest_creation_and_omits_removed_directories() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let earlier = DateTime::from_timestamp_millis(1000).unwrap();
        let later = DateTime::from_timestamp_millis(2000).unwrap();
        let mut history = RecentProjects::load(temp.path());
        history.record(&first, later);
        history.record(&second, earlier);
        // Resuming an older session in the same directory cannot move it backward.
        history.record(&first, earlier);
        let reloaded = RecentProjects::load(temp.path());
        assert_eq!(reloaded.snapshot(), history.snapshot());
        assert_eq!(reloaded.snapshot()[0].last_used, Some(later));
        assert_eq!(reloaded.snapshot().len(), 2);
        std::fs::remove_dir(&first).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&second, &first).unwrap();
        let result = list(Vec::new(), reloaded.snapshot(), None, 10);
        assert_eq!(result.recent.len(), 1);
        assert_eq!(result.recent[0].path, second.canonicalize().unwrap());
        assert_eq!(result.recent[0].last_used, Some(earlier));
        let wire: wire::ListRepositoriesResponse = result.clone().into();
        assert_eq!(ListRepositoriesResponse::try_from(wire).unwrap(), result);
    }

    #[test]
    fn repositories_bound_results_and_recognize_overlapping_roots() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..205 {
            std::fs::create_dir_all(temp.path().join(format!("repo-{index:03}/.git"))).unwrap();
        }
        let roots = vec![
            temp.path().into(),
            temp.path().join("repo-000"),
            temp.path().join("missing"),
        ];
        let result = list(roots, Vec::new(), None, u32::MAX);
        assert_eq!(result.repositories.len(), 200);
        assert_eq!(result.repositories[0].name, "repo-000");
        assert_eq!(result.repositories[199].name, "repo-199");
        let recent = result.repositories;
        let capped = list(Vec::new(), recent, None, 2);
        assert_eq!(capped.recent.len(), 2);
    }

    #[test]
    fn repositories_invalid_wire_timestamp_is_a_decode_error() {
        let result = ListRepositoriesResponse::try_from(wire::ListRepositoriesResponse {
            recent: vec![wire::ProjectEntry {
                path: "/project".into(),
                name: "project".into(),
                last_used_unix_ms: Some(i64::MAX),
            }],
            ..Default::default()
        });
        assert_eq!(
            result.unwrap_err(),
            "invalid ProjectEntry.last_used_unix_ms"
        );
    }

    #[test]
    fn repositories_config_defaults_empty_and_loads_explicit_roots() {
        let empty: crate::Config = serde_yaml::from_str("host_name: empty").unwrap();
        assert!(empty.repository_roots.is_empty());
        let configured: crate::Config =
            serde_yaml::from_str("repository_roots: [projects, work]").unwrap();
        assert_eq!(
            configured.repository_roots,
            vec![PathBuf::from("projects"), PathBuf::from("work")]
        );
    }
}
