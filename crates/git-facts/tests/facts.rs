//! The facts of real repositories made in temporary folders.

use std::path::Path;
use std::process::Command;

use git_facts::{ChangeTotals, GitFacts, facts};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(cwd: &Path, path: &str, lines: usize) {
    let text: String = (0..lines).map(|n| format!("line {n}\n")).collect();
    std::fs::write(cwd.join(path), text).unwrap();
}

/// A repository on `main` with one commit of a ten-line file.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "base.txt", 10);
    git(dir.path(), &["add", "base.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "start"]);
    dir
}

fn totals(files: u32, added: u32, removed: u32) -> Option<ChangeTotals> {
    Some(ChangeTotals {
        files,
        added,
        removed,
    })
}

#[tokio::test]
async fn a_folder_outside_a_repository_has_no_facts() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(facts(dir.path(), None).await.unwrap(), None);
}

#[tokio::test]
async fn a_clean_default_branch_has_nothing_uncommitted_and_no_branch_totals() {
    let repo = repository();
    assert_eq!(
        facts(repo.path(), None).await.unwrap(),
        Some(GitFacts {
            branch: Some("main".into()),
            base_branch: Some("main".into()),
            uncommitted: totals(0, 0, 0),
            on_branch: None,
        })
    );
}

#[tokio::test]
async fn a_branch_counts_its_commits_and_its_working_tree_from_where_it_left_its_base() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "feature"]);
    write(dir, "committed.txt", 4);
    git(dir, &["add", "committed.txt"]);
    git(dir, &["commit", "-q", "-m", "on the branch"]);
    // The base moves on after the branch left it; that does not count.
    git(dir, &["switch", "-q", "main"]);
    write(dir, "later-on-main.txt", 50);
    git(dir, &["add", "later-on-main.txt"]);
    git(dir, &["commit", "-q", "-m", "main moves on"]);
    git(dir, &["switch", "-q", "feature"]);
    // Uncommitted: a changed line, a staged new file and an untracked one.
    write(dir, "base.txt", 9);
    write(dir, "staged.txt", 2);
    git(dir, &["add", "staged.txt"]);
    write(dir, "untracked.txt", 3);
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("feature"));
    assert_eq!(found.base_branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(3, 5, 1));
    assert_eq!(
        found.on_branch,
        totals(4, 9, 1),
        "the commit's file and the three uncommitted ones"
    );
    assert_eq!(
        std::fs::read(dir.join(".git/index")).unwrap(),
        index,
        "the person's index is untouched"
    );
    assert_eq!(
        git(dir, &["status", "--porcelain", "--", "untracked.txt"]),
        "?? untracked.txt",
        "the untracked file is still untracked"
    );
}

#[tokio::test]
async fn a_recorded_base_wins_over_the_default() {
    let repo = repository();
    let dir = repo.path();
    git(dir, &["switch", "-q", "-c", "release"]);
    write(dir, "release.txt", 2);
    git(dir, &["add", "release.txt"]);
    git(dir, &["commit", "-q", "-m", "release"]);
    git(dir, &["switch", "-q", "-c", "fix"]);
    write(dir, "fix.txt", 1);

    let found = facts(dir, Some("release")).await.unwrap().unwrap();
    assert_eq!(found.base_branch.as_deref(), Some("release"));
    assert_eq!(found.on_branch, totals(1, 1, 0));
    let default = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(default.base_branch.as_deref(), Some("main"));
    assert_eq!(default.on_branch, totals(2, 3, 0));
}

#[tokio::test]
async fn a_clone_takes_its_default_branch_from_origin() {
    let upstream = repository();
    git(upstream.path(), &["branch", "-q", "-m", "main", "trunk"]);
    let place = tempfile::tempdir().unwrap();
    git(
        place.path(),
        &["clone", "-q", &upstream.path().to_string_lossy(), "clone"],
    );
    let clone = place.path().join("clone");
    git(&clone, &["switch", "-q", "-c", "work"]);
    write(&clone, "work.txt", 7);

    let found = facts(&clone, None).await.unwrap().unwrap();
    assert_eq!(found.base_branch.as_deref(), Some("trunk"));
    assert_eq!(found.on_branch, totals(1, 7, 0));
}

#[tokio::test]
async fn a_detached_head_has_no_branch_but_is_measured_from_its_base() {
    let repo = repository();
    let dir = repo.path();
    write(dir, "base.txt", 12);
    git(dir, &["commit", "-q", "-am", "two more lines"]);
    git(dir, &["switch", "-q", "--detach", "HEAD"]);
    git(dir, &["branch", "-q", "-f", "main", "HEAD~1"]);

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch, None);
    assert_eq!(found.on_branch, totals(1, 2, 0));
}

#[tokio::test]
async fn a_repository_without_commits_counts_everything_as_uncommitted() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    write(dir.path(), "first.txt", 5);

    let found = facts(dir.path(), None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(1, 5, 0));
    assert_eq!(found.on_branch, None);
}

#[tokio::test]
async fn untracked_paths_past_the_command_line_limit_still_count() {
    // More path text than one command line holds: 32 KiB on Windows, about
    // 1 MiB on macOS and usually 2 MiB on Linux.
    let (files, path_text) = if cfg!(windows) {
        (400, 64 * 1024)
    } else {
        (13_000, 5 * 1024 * 1024 / 2)
    };
    let repo = repository();
    let dir = repo.path();
    let padding = "x".repeat(200);
    let mut written = 0;
    for n in 0..files {
        let name = format!("{n:05}-{padding}.txt");
        written += name.len();
        std::fs::write(dir.join(&name), "one line\n").unwrap();
    }
    assert!(written > path_text, "{written} bytes of untracked paths");

    let found = facts(dir, None).await.unwrap().unwrap();
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.uncommitted, totals(files, files, 0));
}
