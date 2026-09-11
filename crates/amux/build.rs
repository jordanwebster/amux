use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest_dir.join("../..");
    let dot_git = repo.join(".git");
    println!("cargo:rerun-if-changed={}", dot_git.display());

    let sha = git_sha(&repo).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={sha}");
}

fn git_sha(repo: &Path) -> Option<String> {
    let git_dir = git_dir(repo)?;
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();
    if is_sha(head) {
        return Some(head.to_string());
    }

    let reference = head.strip_prefix("ref: ")?;
    let common_dir = common_dir(&git_dir).unwrap_or_else(|| git_dir.clone());
    let reference_path = common_dir.join(reference);
    println!("cargo:rerun-if-changed={}", reference_path.display());
    if let Ok(value) = fs::read_to_string(&reference_path) {
        let value = value.trim();
        if is_sha(value) {
            return Some(value.to_string());
        }
    }

    let packed_refs = common_dir.join("packed-refs");
    println!("cargo:rerun-if-changed={}", packed_refs.display());
    fs::read_to_string(packed_refs)
        .ok()?
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .find_map(|line| {
            let (sha, name) = line.split_once(' ')?;
            (name == reference && is_sha(sha)).then(|| sha.to_string())
        })
}

fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = fs::read_to_string(dot_git).ok()?;
    let path = PathBuf::from(pointer.trim().strip_prefix("gitdir: ")?);
    Some(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

fn common_dir(git_dir: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(fs::read_to_string(git_dir.join("commondir")).ok()?.trim());
    Some(if path.is_absolute() {
        path
    } else {
        git_dir.join(path)
    })
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
