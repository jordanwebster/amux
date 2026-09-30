//! Publishing the daemon's release feed: the signing key and the channel
//! manifests. The private key is a 32-byte Ed25519 seed that lives only in
//! this Mac's login keychain; the workflow that builds releases never sees
//! it, and neither does the server that serves manifests, so a compromised
//! build runner or cloud cannot sign a binary that machines would install.
//!
//! ```text
//! xtask release key generate        make a seed, keep it, print its public half
//! xtask release key public          print the keychain seed's public half
//! xtask release manifest VERSION [--channel stable|preview] [--rollout N] [--publish]
//! ```
//!
//! `manifest` takes the binaries a GitHub Release already holds, signs each
//! one's checksum, verifies the signatures against the key the workflow
//! compiles into those binaries, writes `<channel>.json`, and with
//! `--publish` uploads it to the same release, where amux.sh serves it as
//! `/releases/<channel>.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use node::release::{self, Manifest, Release};

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
        ["manifest", version, rest @ ..] => manifest(version, rest),
        _ => Err("usage: xtask release <key generate|key public|manifest VERSION [--channel stable|preview] [--rollout N] [--publish]>".into()),
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

struct Options {
    channel: String,
    rollout: Option<u8>,
    publish: bool,
}

fn options(rest: &[&str]) -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        channel: "stable".into(),
        rollout: None,
        publish: false,
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
            "--publish" => options.publish = true,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    Ok(options)
}

fn manifest(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let options = options(rest)?;
    let version = version.strip_prefix('v').unwrap_or(version);
    semver::Version::parse(version)?;
    let tag = format!("v{version}");

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
    let mut targets = BTreeMap::new();
    for (target, asset) in ASSETS {
        let sha256 = checksum(&checksums, asset)
            .ok_or_else(|| format!("checksums.txt of {tag} has no line for {asset}"))?;
        let entry = Release {
            version: version.into(),
            url: format!("https://github.com/{repository}/releases/download/{tag}/{asset}"),
            sha256: sha256.clone(),
            signature: release::sign(&seed, target, version, &sha256),
        };
        release::verify(&entry, target, &sha256, &key)?;
        targets.insert((*target).to_owned(), entry);
    }
    let manifest = Manifest {
        rollout: options.rollout,
        targets,
    };
    let json = serde_json::to_string_pretty(&manifest)?;

    let dir = root.join("target/release-manifests");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.json", options.channel));
    std::fs::write(&file, format!("{json}\n"))?;
    println!("{json}");
    eprintln!("wrote {}", file.display());
    if options.publish {
        gh(&[
            "release",
            "upload",
            &tag,
            &file.to_string_lossy(),
            "--clobber",
        ])?;
        eprintln!(
            "uploaded {}.json to {tag}; amux.sh serves it as /releases/{}.json",
            options.channel, options.channel
        );
    }
    Ok(())
}

/// The hex digest `checksums.txt` records for `asset`, as `sha256sum` writes
/// it: the digest, whitespace, the file name.
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
