//! Publishing the daemon's release feed: the signing key and the channel
//! manifests. The private key is a 32-byte Ed25519 seed that lives only in
//! this Mac's login keychain; the workflow that builds releases never sees
//! it, and neither does the server that serves manifests, so neither can
//! sign a manifest machines would take. What is signed is what the
//! workflow built: the key vouches for the digests GitHub published, not
//! for the build, a trust kept deliberately and written up on the release
//! page.
//!
//! A release is two acts at two times. `cut` makes the version exist: the
//! number goes into the manifests, the commit is tagged and pushed, and the
//! tag's workflow builds the binaries into a GitHub Release. `deploy` puts
//! that release in front of machines: a channel and a rollout are chosen,
//! the binaries' checksums are signed, and the channel manifest is uploaded
//! to the release and handed to the operator's publish script, which puts
//! it where amux.sh serves it as `/releases/<channel>.json`.
//! Deploying repeats against the same release (a wider rollout, preview
//! promoted to stable); cutting does not.
//!
//! ```text
//! xtask release key generate        make a seed, keep it, print its public half
//! xtask release key public          print the keychain seed's public half
//! xtask release cut VERSION         bump, lock, release-check, commit, tag, push
//! xtask release deploy VERSION [--channel stable|preview] [--rollout N]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use release::{Manifest, Release};

/// The keychain item that holds the seed, as `security` names it.
const KEYCHAIN_SERVICE: &str = "amux-release-key";
/// The workflow whose `env` names the key the published binaries trust.
const WORKFLOW: &str = ".github/workflows/release.yml";
const PUBLIC_KEY_VARIABLE: &str = "AMUX_RELEASE_PUBLIC_KEY";
/// The file the Release workflow publishes for each target it builds.
const ASSETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-gnu", "amux-linux-x86_64"),
    ("aarch64-apple-darwin", "amux-macos-arm64"),
    ("x86_64-pc-windows-msvc", "amux-windows-x86_64.exe"),
];
const CHANNELS: &[&str] = &["stable", "preview"];

pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["key", "generate"] => generate(),
        ["key", "public"] => {
            println!("{}", release::hex(&release::public_key(&keychain_seed()?)));
            Ok(())
        }
        ["cut", version] => cut(version),
        ["deploy", version, rest @ ..] => deploy(version, rest),
        _ => Err("usage: xtask release <key generate|key public|cut VERSION|deploy VERSION [--channel stable|preview] [--rollout N]>".into()),
    }
}

/// Makes a seed from the system's randomness and keeps it in the login
/// keychain. Refuses when one is there: a key is rotated by removing the old
/// item deliberately, never by a second run overwriting it.
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    if keychain_seed().is_ok() {
        return Err(format!(
            "the login keychain already holds an item for service {KEYCHAIN_SERVICE}; \
             delete it with `security delete-generic-password -s {KEYCHAIN_SERVICE}` \
             to make another key"
        )
        .into());
    }
    let seed = release::new_seed();
    let hex = release::hex(&seed);
    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            &account(),
            "-w",
            &hex,
        ])
        .status()?;
    if !status.success() {
        return Err("security add-generic-password failed".into());
    }
    println!("{}", release::hex(&release::public_key(&seed)));
    Ok(())
}

fn account() -> String {
    std::env::var("USER").unwrap_or_else(|_| "amux".into())
}

/// The seed from the login keychain.
fn keychain_seed() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "no release key in the login keychain (service {KEYCHAIN_SERVICE}); \
             `xtask release key generate` makes one"
        )
        .into());
    }
    release::parse_key(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "the keychain item is not 64 hex digits".into())
}

/// The key the workflow compiles into the binaries it publishes.
pub fn workflow_key(workflow: &str) -> Option<[u8; 32]> {
    workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PUBLIC_KEY_VARIABLE))
        .filter_map(|rest| rest.trim().strip_prefix(':'))
        .find_map(|value| release::parse_key(value.trim().trim_matches(|c| c == '"' || c == '\'')))
}

/// The manifests that carry the version. The daemon announces its own
/// crate's version to peers, so a release that moved only the CLI would
/// have `amux --version` and the machine a person sees in their fleet
/// disagree.
const VERSIONED: &[&str] = &["crates/amux/Cargo.toml", "crates/node/Cargo.toml"];

/// Makes the version exist: the number in the manifests and the lock,
/// the release build checked, one commit tagged `v<version>`, branch and
/// tag pushed. The tag starts the Release workflow. Refuses a tree with
/// other changes in it, a version that is not above the current one, and
/// a Mac without the release key, since a cut nobody can deploy is a tag
/// for nothing.
fn cut(version: &str) -> Result<(), Box<dyn std::error::Error>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let wanted = semver::Version::parse(version)?;
    let root = repository_root();
    let status = git(&root, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(
            format!("the tree has changes; a cut commits only the version:\n{status}").into(),
        );
    }
    let workflow = std::fs::read_to_string(root.join(WORKFLOW))?;
    let key = release::public_key(&keychain_seed()?);
    if workflow_key(&workflow) != Some(key) {
        return Err(format!(
            "{WORKFLOW} does not name this Mac's release key, so the release could not be deployed"
        )
        .into());
    }
    let current = current_version(&std::fs::read_to_string(root.join(VERSIONED[0]))?)?;
    if wanted <= current {
        return Err(format!("{wanted} is not above the current version {current}").into());
    }
    for manifest in VERSIONED {
        let path = root.join(manifest);
        let text = std::fs::read_to_string(&path)?;
        let bumped = bump_version(&text, &current, &wanted)
            .ok_or_else(|| format!("{manifest} does not carry version {current}"))?;
        std::fs::write(&path, bumped)?;
    }
    run(
        &root,
        "cargo",
        &["update", "--offline", "-p", "amux", "-p", "node"],
    )?;
    run(&root, "just", &["release-check"])?;
    let mut add = vec!["add"];
    add.extend(VERSIONED);
    add.push("Cargo.lock");
    git(&root, &add)?;
    let tag = format!("v{wanted}");
    git(&root, &["commit", "-q", "-m", &tag])?;
    git(&root, &["tag", &tag])?;
    git(&root, &["push"])?;
    git(&root, &["push", "origin", &tag])?;
    println!("cut {tag}; the Release workflow is building it. `just deploy {wanted}` when it has.");
    Ok(())
}

/// The version the first versioned manifest carries.
pub fn current_version(manifest: &str) -> Result<semver::Version, Box<dyn std::error::Error>> {
    let line = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .ok_or("the manifest has no version line")?;
    Ok(semver::Version::parse(line.trim_end_matches('"'))?)
}

/// The manifest with its own version line moved from `from` to `to`, or
/// None when it does not carry `from`.
pub fn bump_version(
    manifest: &str,
    from: &semver::Version,
    to: &semver::Version,
) -> Option<String> {
    let old = format!("version = \"{from}\"");
    let new = format!("version = \"{to}\"");
    manifest
        .contains(&old)
        .then(|| manifest.replacen(&old, &new, 1))
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(format!("{program} {} failed: {status}", args.join(" ")).into());
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

struct Options {
    channel: String,
    rollout: Option<u8>,
}

fn options(rest: &[&str]) -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        channel: "stable".into(),
        rollout: None,
    };
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "--channel" => {
                let channel = words.next().ok_or("--channel needs a value")?;
                if !CHANNELS.contains(channel) {
                    return Err(format!(
                        "no channel {channel}; the channels are stable and preview"
                    )
                    .into());
                }
                options.channel = (*channel).into();
            }
            "--rollout" => {
                let percent: u8 = words.next().ok_or("--rollout needs a value")?.parse()?;
                if percent > 100 {
                    return Err("--rollout is a percentage".into());
                }
                options.rollout = Some(percent);
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    Ok(options)
}

/// Puts a cut release in front of a channel: waits for the tag's workflow
/// to have published the binaries, signs their checksums, verifies every
/// signature against the key the workflow compiles in, uploads
/// `<channel>.json` to the release, replacing one already there, and runs
/// the publish script that puts it where amux.sh serves it.
fn deploy(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let options = options(rest)?;
    let version = version.strip_prefix('v').unwrap_or(version);
    semver::Version::parse(version)?;
    let tag = format!("v{version}");
    wait_for_release(&tag)?;

    let seed = keychain_seed()?;
    let key = release::public_key(&seed);
    let root = repository_root();
    let workflow = std::fs::read_to_string(root.join(WORKFLOW))?;
    match workflow_key(&workflow) {
        Some(trusted) if trusted == key => {}
        Some(_) => {
            return Err(format!(
                "{WORKFLOW} names a different {PUBLIC_KEY_VARIABLE} than the keychain seed's \
                 public half {}; the binaries would refuse this manifest",
                release::hex(&key)
            )
            .into());
        }
        None => return Err(format!("{WORKFLOW} sets no {PUBLIC_KEY_VARIABLE}").into()),
    }

    let repository = gh(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "--jq",
        ".nameWithOwner",
    ])?;
    let repository = repository.trim();
    let checksums = gh(&[
        "release",
        "download",
        &tag,
        "--pattern",
        "checksums.txt",
        "--output",
        "-",
    ])?;
    let sizes = gh(&[
        "release",
        "view",
        &tag,
        "--json",
        "assets",
        "--jq",
        r#".assets[] | "\(.name) \(.size)""#,
    ])?;
    let mut targets = BTreeMap::new();
    for (target, asset) in ASSETS {
        let sha256 = checksum(&checksums, asset)
            .ok_or_else(|| format!("checksums.txt of {tag} has no line for {asset}"))?;
        let size = asset_size(&sizes, asset)
            .ok_or_else(|| format!("the release {tag} lists no size for {asset}"))?;
        let entry = Release {
            version: version.into(),
            url: format!("https://github.com/{repository}/releases/download/{tag}/{asset}"),
            sha256,
            size,
        };
        targets.insert((*target).to_owned(), entry);
    }
    let mut manifest = Manifest {
        channel: options.channel.clone(),
        rollout: options.rollout,
        targets,
        signature: String::new(),
    };
    manifest.signature = release::sign(&seed, &manifest);
    release::verify_manifest(&manifest, &options.channel, &key)?;
    let json = serde_json::to_string_pretty(&manifest)?;

    let dir = root.join("target/release-manifests");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.json", options.channel));
    std::fs::write(&file, format!("{json}\n"))?;
    println!("{json}");
    gh(&[
        "release",
        "upload",
        &tag,
        &file.to_string_lossy(),
        "--clobber",
    ])?;
    println!(
        "deployed {tag} to {}{} on the release; publishing to amux.sh",
        options.channel,
        options
            .rollout
            .map(|percent| format!(" at {percent}%"))
            .unwrap_or_default(),
    );
    publish(&options.channel, version, &file)?;
    Ok(())
}

/// Where the operator's publish script lives: `~/scripts/<PUBLISH_SCRIPT>`.
/// The script is not in this repository. Machines read manifests from
/// amux.sh, and how a manifest gets there (which host, which path, which
/// key) is the operator's to keep with the rest of the host configuration;
/// this repository only says that it happens, with these arguments.
const PUBLISH_SCRIPT: &str = "amux-publish-manifest";

/// Runs the publish script as `<script> <channel> <version> <manifest>`.
/// A deploy that stops at the GitHub upload is a silent no-op for every
/// machine, so a missing script is an error, not a skipped step.
fn publish(
    channel: &str,
    version: &str,
    manifest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let script = Path::new(&home).join("scripts").join(PUBLISH_SCRIPT);
    if !script.is_file() {
        return Err(format!(
            "{} is uploaded to the release but not published: {} is missing, and machines \
             read https://amux.sh/releases/{channel}.json, not the release",
            manifest.display(),
            script.display()
        )
        .into());
    }
    let status = Command::new(&script)
        .arg(channel)
        .arg(version)
        .arg(manifest)
        .status()?;
    if !status.success() {
        return Err(format!("{} failed with {status}", script.display()).into());
    }
    Ok(())
}

/// Waits until the tag's GitHub Release holds `checksums.txt`: the Release
/// workflow is still building when a deploy follows a cut closely, and a
/// release that never appears is reported as that after a bounded wait.
fn wait_for_release(tag: &str) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45 * 60);
    let mut said = false;
    loop {
        let assets = gh(&[
            "release",
            "view",
            tag,
            "--json",
            "assets",
            "--jq",
            ".assets[].name",
        ])
        .unwrap_or_default();
        if assets.lines().any(|name| name.trim() == "checksums.txt") {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("{tag} has no release with checksums.txt after 45 minutes").into());
        }
        if !said {
            eprintln!("waiting for the Release workflow to publish {tag}");
            said = true;
        }
        std::thread::sleep(std::time::Duration::from_secs(20));
    }
}

/// The hex digest `checksums.txt` records for `asset`, as `sha256sum` writes
/// it: the digest, whitespace, the file name.
/// The size of `asset` in a listing of `<name> <size>` lines, as `gh
/// release view --json assets` is asked to print it.
pub fn asset_size(listing: &str, asset: &str) -> Option<u64> {
    listing.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let name = words.next()?;
        let size = words.next()?.parse().ok()?;
        (name == asset).then_some(size)
    })
}

pub fn checksum(checksums: &str, asset: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let digest = words.next()?;
        let name = words.next()?.trim_start_matches('*');
        (name == asset && digest.len() == 64).then(|| digest.to_ascii_lowercase())
    })
}

fn gh(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("gh").args(args).output()?;
    if !output.status.success() {
        return Err(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("xtask lives under crates/")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_workflows_key() {
        let workflow = "env:\n  CARGO_TERM_COLOR: always\n  AMUX_RELEASE_PUBLIC_KEY: \"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\"\n";
        assert_eq!(workflow_key(workflow).map(|k| k[0]), Some(0x00));
        assert_eq!(workflow_key(workflow).map(|k| k[15]), Some(0xff));
        assert_eq!(workflow_key("env:\n  CARGO_TERM_COLOR: always\n"), None);
    }

    #[test]
    fn reads_a_checksum_line() {
        let text = "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  amux-macos-arm64\n\
                    0000000000000000000000000000000000000000000000000000000000000000 *amux-windows-x86_64.exe\n";
        assert_eq!(
            checksum(text, "amux-macos-arm64").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert!(checksum(text, "amux-windows-x86_64.exe").is_some());
        assert_eq!(checksum(text, "amux-linux-x86_64"), None);
    }

    #[test]
    fn a_cut_moves_the_crates_version_line_and_nothing_else() {
        let manifest = "[package]\nname = \"amux\"\nversion = \"0.7.0\"\n\n[dependencies]\nnode = { version = \"0.7.0\" }\n";
        let from = current_version(manifest).unwrap();
        assert_eq!(from.to_string(), "0.7.0");
        let to = semver::Version::parse("0.8.0").unwrap();
        let bumped = bump_version(manifest, &from, &to).unwrap();
        assert!(bumped.contains("version = \"0.8.0\"\n\n[dependencies]"));
        assert!(
            bumped.contains("node = { version = \"0.7.0\" }"),
            "a dependency's version is not the crate's"
        );
        assert!(bump_version(manifest, &to, &from).is_none());
    }

    /// The committed workflow names a real key, so the release binaries it
    /// builds trust something.
    #[test]
    fn the_workflow_names_a_key() {
        let workflow = std::fs::read_to_string(repository_root().join(WORKFLOW)).unwrap();
        assert!(
            workflow_key(&workflow).is_some(),
            "{WORKFLOW} sets no {PUBLIC_KEY_VARIABLE}"
        );
    }
}
