//! Names for agents created without one: a memorable word pair such as
//! `quiet-otter`, lowercase and hyphenated so it also serves as a branch
//! name.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

const FIRST: &[&str] = &[
    "amber", "ample", "bold", "brave", "brisk", "calm", "clever", "cosy", "crisp", "curious",
    "deft", "eager", "early", "fair", "fond", "frank", "gentle", "glad", "golden", "grand",
    "happy", "hardy", "honest", "jolly", "keen", "kind", "lively", "lucky", "mellow", "merry",
    "mild", "nimble", "noble", "patient", "plucky", "polite", "proud", "quick", "quiet", "rapid",
    "ready", "steady", "sunny", "swift", "tidy", "witty", "young", "zesty",
];

const SECOND: &[&str] = &[
    "badger", "beaver", "bison", "crane", "cricket", "dolphin", "eagle", "falcon", "ferret",
    "finch", "fox", "gecko", "heron", "ibis", "jackal", "koala", "lark", "lemur", "lynx", "magpie",
    "marten", "moose", "newt", "ocelot", "orca", "osprey", "otter", "owl", "panda", "pelican",
    "puffin", "quail", "raven", "robin", "salmon", "seal", "sparrow", "stoat", "swan", "tapir",
    "tern", "toucan", "trout", "turtle", "walrus", "weasel", "wren", "yak",
];

/// Every word pair, in the order a search walks them.
pub fn word_pairs() -> impl Iterator<Item = String> {
    FIRST
        .iter()
        .flat_map(|first| SECOND.iter().map(move |second| format!("{first}-{second}")))
}

/// A memorable word pair `taken` does not claim. The search starts at a
/// random pair and walks every pair from there; only when all are taken
/// does a number follow the pair.
pub fn assign_name(taken: &dyn Fn(&str) -> bool) -> String {
    let pairs: Vec<String> = word_pairs().collect();
    let start = (uuid::Uuid::new_v4().as_u128() % pairs.len() as u128) as usize;
    let walk = || pairs[start..].iter().chain(&pairs[..start]);
    if let Some(free) = walk().find(|pair| !taken(pair)) {
        return free.clone();
    }
    (2u32..)
        .flat_map(|n| walk().map(move |pair| format!("{pair}-{n}")))
        .find(|name| !taken(name))
        .expect("an unbounded search finds a free name")
}

/// The names a new branch in `cwd`'s repository could not take: every local
/// branch, and each folder a branch sits under (`a/b` rules out `a`). Empty
/// when `cwd` is not in a repository or git cannot be run.
pub async fn branch_names(cwd: &Path) -> HashSet<String> {
    let output = tokio::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .await;
    let Ok(output) = output else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }
    let mut names = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(branch) = line.strip_prefix("refs/heads/") else {
            continue;
        };
        let mut prefix = String::new();
        for part in branch.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            names.insert(prefix.to_lowercase());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pair_is_distinct_and_branch_safe() {
        let pairs: Vec<String> = word_pairs().collect();
        let distinct: HashSet<&String> = pairs.iter().collect();
        assert_eq!(distinct.len(), FIRST.len() * SECOND.len());
        assert!(
            pairs
                .iter()
                .all(|pair| { pair.chars().all(|c| c.is_ascii_lowercase() || c == '-') })
        );
    }

    #[test]
    fn a_number_follows_the_pair_only_when_every_pair_is_taken() {
        let pairs: HashSet<String> = word_pairs().collect();
        let name = assign_name(&|name| pairs.contains(name));
        let (pair, number) = name.rsplit_once('-').unwrap();
        assert!(pairs.contains(pair), "{name}");
        assert_eq!(number, "2");
    }
}
