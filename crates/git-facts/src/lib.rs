//! What an agent's folder says about its git repository: the branch, the
//! branch it is measured against, and how much changed. The one piece of git
//! code the agent process and the daemon share.
//!
//! Everything is read by running `git` in the folder, with optional locks
//! off so a read never contends with the person's own git. The person's
//! index is never written: totals that need untracked files counted use a
//! temporary copy of it.

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::{Output, Stdio};

/// How much a comparison changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeTotals {
    /// Files added, removed or changed, binary ones included.
    pub files: u32,
    /// Lines added and removed, binary files counting none.
    pub added: u32,
    pub removed: u32,
}

/// What a fleet row shows about an agent's repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// None on a detached head.
    pub branch: Option<String>,
    /// The branch amux made the worktree from when it made one, else the
    /// repository's default branch; None when neither is known.
    pub base_branch: Option<String>,
    /// The working tree against HEAD, untracked files included; always
    /// present in a repository, where an unborn HEAD compares with the
    /// empty tree.
    pub uncommitted: Option<ChangeTotals>,
    /// From where the branch left its base to the working tree, so
    /// uncommitted work counts. None on the base branch itself, without a
    /// base, or when the branch shares no history with it.
    pub on_branch: Option<ChangeTotals>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("running git: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git {command} failed: {stderr}")]
    Failed { command: String, stderr: String },
    #[error("the temporary index: {0}")]
    Scratch(#[source] std::io::Error),
}

/// The facts for `cwd`, or None when it is not inside a git working tree.
/// `recorded_base` is the branch a worktree was made from when amux made
/// it; without one the base is the repository's default branch.
pub async fn facts(cwd: &Path, recorded_base: Option<&str>) -> Result<Option<GitFacts>, GitError> {
    let inside = run(cwd, None, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Ok(None);
    }
    let branch = answer(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let base_branch = match recorded_base {
        Some(base) => Some(base.to_owned()),
        None => default_branch(cwd).await?,
    };
    let against = match &head {
        Some(head) => head.clone(),
        None => empty_tree(cwd).await?,
    };
    let uncommitted = Some(totals(cwd, &against).await?);
    let on_branch = match (&base_branch, &head) {
        (Some(base), Some(_)) if branch.as_deref() != Some(base.as_str()) => {
            match fork_point(cwd, base).await? {
                Some(fork) => Some(totals(cwd, &fork).await?),
                None => None,
            }
        }
        _ => None,
    };
    Ok(Some(GitFacts {
        branch,
        base_branch,
        uncommitted,
        on_branch,
    }))
}

/// The repository's default branch: what `origin/HEAD` names, else the
/// configured default for new repositories, else `main` or `master`,
/// whichever exists.
async fn default_branch(cwd: &Path) -> Result<Option<String>, GitError> {
    if let Some(remote) = answer(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await?
    {
        return Ok(Some(
            remote
                .strip_prefix("origin/")
                .map_or(remote.clone(), str::to_owned),
        ));
    }
    let configured = answer(cwd, &["config", "--get", "init.defaultBranch"]).await?;
    for candidate in configured
        .iter()
        .map(String::as_str)
        .chain(["main", "master"])
    {
        if branch_ref(cwd, candidate).await?.is_some() {
            return Ok(Some(candidate.to_owned()));
        }
    }
    Ok(None)
}

/// The ref a base branch is read from: the local branch, else origin's.
async fn branch_ref(cwd: &Path, branch: &str) -> Result<Option<String>, GitError> {
    for name in [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    ] {
        if answer(cwd, &["rev-parse", "--verify", "--quiet", &name])
            .await?
            .is_some()
        {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Where HEAD left `base`: their merge base. None when the base does not
/// exist or shares no history with HEAD.
async fn fork_point(cwd: &Path, base: &str) -> Result<Option<String>, GitError> {
    let Some(base) = branch_ref(cwd, base).await? else {
        return Ok(None);
    };
    answer(cwd, &["merge-base", &base, "HEAD"]).await
}

/// The empty tree's id in this repository's hash.
async fn empty_tree(cwd: &Path) -> Result<String, GitError> {
    let output = checked(cwd, None, &["hash-object", "-t", "tree", "--stdin"]).await?;
    Ok(text(&output))
}

/// `against` compared with the working tree, untracked files included. A
/// temporary copy of the index gets intent-to-add entries for the untracked
/// files, so they count as added and the person's index is never touched.
async fn totals(cwd: &Path, against: &str) -> Result<ChangeTotals, GitError> {
    let scratch = tempfile::Builder::new()
        .prefix("amux-git-facts-")
        .tempfile()
        .map_err(GitError::Scratch)?;
    let index = text(
        &checked(
            cwd,
            None,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .await?,
    );
    match std::fs::copy(&index, scratch.path()) {
        Ok(_) => {}
        // A repository nothing was ever added to has no index yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::remove_file(scratch.path()).map_err(GitError::Scratch)?;
        }
        Err(error) => return Err(GitError::Scratch(error)),
    }
    let scratch_index = Some(scratch.path());
    let untracked = checked(
        cwd,
        scratch_index,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;
    let untracked: Vec<String> = untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    if !untracked.is_empty() {
        let mut args = vec!["--literal-pathspecs", "add", "--intent-to-add", "--"];
        args.extend(untracked.iter().map(String::as_str));
        checked(cwd, scratch_index, &args).await?;
    }
    let numstat = checked(
        cwd,
        scratch_index,
        &[
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            against,
            "--",
        ],
    )
    .await?;
    Ok(count(&numstat))
}

/// Sums `git diff --numstat -z --no-renames` records: `added TAB removed
/// TAB path NUL`, with `-` for both counts on a binary file.
fn count(numstat: &[u8]) -> ChangeTotals {
    let mut totals = ChangeTotals::default();
    for record in numstat.split(|byte| *byte == 0) {
        let record = String::from_utf8_lossy(record);
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(_path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        totals.files += 1;
        totals.added += added.parse::<u32>().unwrap_or(0);
        totals.removed += removed.parse::<u32>().unwrap_or(0);
    }
    totals
}

/// A git query's one-line answer, or None when git says no (exit status
/// non-zero), as `--quiet` lookups do for something that is not there.
async fn answer(cwd: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
    let output = run(cwd, None, args).await?;
    Ok(output
        .status
        .success()
        .then(|| text(&output.stdout))
        .filter(|answer| !answer.is_empty()))
}

/// Standard output of a git command that must succeed.
async fn checked(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let output = run(cwd, index, args).await?;
    if !output.status.success() {
        return Err(GitError::Failed {
            command: args.first().copied().unwrap_or_default().to_owned(),
            stderr: text(&output.stderr),
        });
    }
    Ok(output.stdout)
}

async fn run(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<Output, GitError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command.output().await.map_err(GitError::Spawn)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_lines_and_binary_files() {
        let totals = count(b"3\t1\tsrc/a.rs\x00-\t-\tlogo.png\x0010\t0\tnew file.txt\x00");
        assert_eq!(
            totals,
            ChangeTotals {
                files: 3,
                added: 13,
                removed: 1
            }
        );
        assert_eq!(count(b""), ChangeTotals::default());
    }
}
