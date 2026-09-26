//! What a build knows about releases: its own version, the target it was
//! built for, the key releases are signed with, and how a channel's
//! manifest names the build to install.
//!
//! A channel is one manifest, keyed by target triple, naming a version, an
//! artifact URL, the artifact's SHA-256 and an Ed25519 signature over
//! [`signed_message`]. The signature binds the version to the hash, so a
//! tampered manifest cannot relabel an old signed artifact as a newer
//! release. The stable manifest may carry a rollout percentage; a host is
//! inside it when hash(host id) mod 100 is under the number, so a host's
//! place in every rollout is fixed and nothing is stored.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The target triple this binary was built for; manifests are keyed by it.
pub const TARGET: &str = env!("AMUX_TARGET");

/// The public half of the key debug builds trust when the build names no
/// release key. Its private half is in the test suites; no release build
/// trusts it.
pub const TEST_RELEASE_KEY: [u8; 32] = [
    0x48, 0x9e, 0xa5, 0x33, 0xf0, 0x7a, 0x72, 0xdb, 0x76, 0xa9, 0x9c, 0xf4, 0x7b, 0xf4, 0x00, 0x8f,
    0xcc, 0xee, 0x4b, 0x8f, 0x20, 0x05, 0xb4, 0xcc, 0xa0, 0x83, 0x5d, 0xf1, 0x04, 0xa4, 0x7e, 0xa9,
];

/// The key releases are signed with, compiled in from
/// `AMUX_RELEASE_PUBLIC_KEY` (64 hex digits) at build time. A release build
/// without one never installs anything; a debug build without one trusts
/// [`TEST_RELEASE_KEY`].
pub fn release_key() -> Option<[u8; 32]> {
    match option_env!("AMUX_RELEASE_PUBLIC_KEY") {
        Some(hex) => Some(
            parse_hex::<32>(hex).expect("AMUX_RELEASE_PUBLIC_KEY is 64 hex digits at build time"),
        ),
        None if cfg!(debug_assertions) => Some(TEST_RELEASE_KEY),
        None => None,
    }
}

// The version lives in a fixed-size stamp behind a marker, so a test or a
// release tool can re-stamp a copy of a built binary with another version
// without rebuilding it: the previous release in an overlap test is the
// build under test with a lower stamp.
const STAMP_LEN: usize = 64;
const MARK_LEN: usize = 16;
const MARK: [u8; MARK_LEN] = *b"\0amux-stamp-v1\0\0";
// The marker spelled backwards, so searching for it does not put a second
// copy of it into the binary.
const MARK_REVERSED: [u8; MARK_LEN] = *b"\0\x001v-pmats-xuma\0";

const fn stamp(version: &str) -> [u8; STAMP_LEN] {
    let mut out = [0u8; STAMP_LEN];
    let mut i = 0;
    while i < MARK_LEN {
        out[i] = MARK[i];
        i += 1;
    }
    let bytes = version.as_bytes();
    assert!(
        bytes.len() < STAMP_LEN - MARK_LEN,
        "version too long to stamp"
    );
    let mut j = 0;
    while j < bytes.len() {
        out[MARK_LEN + j] = bytes[j];
        j += 1;
    }
    out
}

#[used]
static STAMP: [u8; STAMP_LEN] = stamp(env!("CARGO_PKG_VERSION"));

/// This binary's version, as stamped.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        // A volatile read, so the compiler reads the bytes a re-stamp
        // changed rather than the constant it was built with.
        let stamp = unsafe { std::ptr::read_volatile(&STAMP) };
        let body = &stamp[MARK_LEN..];
        let end = body
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(body.len());
        String::from_utf8_lossy(&body[..end]).into_owned()
    })
}

/// Copies the binary at `from` to `to` with `version` stamped in place of
/// its own. On macOS the copy is signed again ad hoc, since the change
/// invalidates the linker's signature.
pub fn restamp(from: &Path, to: &Path, version: &str) -> io::Result<()> {
    let mark: Vec<u8> = MARK_REVERSED.iter().rev().copied().collect();
    let mut bytes = std::fs::read(from)?;
    let mut found = bytes
        .windows(MARK_LEN)
        .enumerate()
        .filter(|(_, window)| *window == mark.as_slice())
        .map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err(io::Error::other(format!(
            "{} does not hold exactly one version stamp",
            from.display()
        )));
    };
    if version.len() >= STAMP_LEN - MARK_LEN {
        return Err(io::Error::other("version too long to stamp"));
    }
    let body = &mut bytes[at + MARK_LEN..at + STAMP_LEN];
    body.fill(0);
    body[..version.len()].copy_from_slice(version.as_bytes());
    let _ = std::fs::remove_file(to);
    std::fs::write(to, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(to)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "codesign {} failed",
                to.display()
            )));
        }
    }
    Ok(())
}

/// A channel's manifest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// The share of hosts, out of 100, that take this release; absent is
    /// every host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<u8>,
    pub targets: BTreeMap<String, Release>,
}

/// One target's build in a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// The artifact's SHA-256, hex.
    pub sha256: String,
    /// Ed25519 over [`signed_message`], base64.
    pub signature: String,
}

/// What a release's signature covers: the target, the version and the
/// artifact's hash.
pub fn signed_message(target: &str, version: &str, sha256_hex: &str) -> Vec<u8> {
    format!(
        "amux release\n{target}\n{version}\n{}\n",
        sha256_hex.to_ascii_lowercase()
    )
    .into_bytes()
}

/// Signs a release with the private key's 32-byte seed; the publishing side
/// of [`verify`].
pub fn sign(seed: &[u8; 32], target: &str, version: &str, sha256_hex: &str) -> String {
    use base64::Engine as _;
    let pair = Ed25519KeyPair::from_seed_unchecked(seed).expect("an Ed25519 seed is any 32 bytes");
    let signature = pair.sign(&signed_message(target, version, sha256_hex));
    base64::engine::general_purpose::STANDARD.encode(signature.as_ref())
}

pub fn sha256_hex(digest: &[u8]) -> String {
    digest.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn sha256_of(bytes: &[u8]) -> String {
    sha256_hex(&Sha256::digest(bytes))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("the artifact's hash {actual} is not the manifest's {expected}")]
    Hash { expected: String, actual: String },
    #[error("the release signature does not verify against this build's key")]
    Signature,
}

/// Checks a downloaded artifact's hash against the manifest and the
/// manifest's signature against `key`.
pub fn verify(
    release: &Release,
    target: &str,
    actual_sha256_hex: &str,
    key: &[u8; 32],
) -> Result<(), VerifyError> {
    use base64::Engine as _;
    if !release.sha256.eq_ignore_ascii_case(actual_sha256_hex) {
        return Err(VerifyError::Hash {
            expected: release.sha256.clone(),
            actual: actual_sha256_hex.to_owned(),
        });
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(release.signature.trim())
        .map_err(|_| VerifyError::Signature)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(
            &signed_message(target, &release.version, &release.sha256),
            &signature,
        )
        .map_err(|_| VerifyError::Signature)
}

/// Whether a host takes a release published to `percent` of hosts.
pub fn inside_rollout(host: Option<&Uuid>, percent: Option<u8>) -> bool {
    let Some(percent) = percent.filter(|percent| *percent < 100) else {
        return true;
    };
    // A host with no id yet has no fixed place, so it waits for a full
    // rollout rather than take a place at random.
    let Some(host) = host else {
        return false;
    };
    rollout_slot(host) < u64::from(percent)
}

/// hash(host id) mod 100: the host's fixed place in every rollout.
pub fn rollout_slot(host: &Uuid) -> u64 {
    let digest = Sha256::digest(host.hyphenated().to_string().as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head) % 100
}

/// What a manifest means for this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Install {
        release: Release,
        version: Version,
    },
    /// Nothing to do, and why.
    Skip(Skip),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    NoBuildForTarget,
    UnreadableVersion(String),
    NotNewer(Version),
    Rejected(Version),
    OutsideRollout,
}

/// Picks the build to install: only one newer than the running version, not
/// the build that was rolled back, and only inside the rollout.
pub fn choose(
    manifest: &Manifest,
    target: &str,
    running: &Version,
    rejected: Option<&Version>,
    host: Option<&Uuid>,
) -> Choice {
    let Some(release) = manifest.targets.get(target) else {
        return Choice::Skip(Skip::NoBuildForTarget);
    };
    let Ok(version) = Version::parse(&release.version) else {
        return Choice::Skip(Skip::UnreadableVersion(release.version.clone()));
    };
    if version <= *running {
        return Choice::Skip(Skip::NotNewer(version));
    }
    if rejected == Some(&version) {
        return Choice::Skip(Skip::Rejected(version));
    }
    if !inside_rollout(host, manifest.rollout) {
        return Choice::Skip(Skip::OutsideRollout);
    }
    Choice::Install {
        release: release.clone(),
        version,
    }
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.trim().as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in bytes.chunks(2).enumerate() {
        out[i] = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Some(out)
}
