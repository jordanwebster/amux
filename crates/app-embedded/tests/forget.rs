//! Removing an account from a device takes everything that device held for
//! it: the profile the account ran on, with its key and the machines it
//! paired with, and the fleet and artifacts cached for it. The account that
//! stays keeps its own profile untouched.

use std::collections::BTreeMap;
use std::path::Path;

use app_embedded::{Embedded, StartConfig};
use model::ProfileId;
use serde_json::json;
use testnet::TestNet;
use tokio::sync::mpsc;

/// A data root short enough for a profile's socket path, which every profile
/// allocates even though an embedded one never listens on it.
fn test_root() -> tempfile::TempDir {
    #[cfg(unix)]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let parent = std::env::temp_dir();
    tempfile::Builder::new()
        .prefix("af")
        .tempdir_in(parent)
        .expect("create a short test root")
}

fn config(
    root: &Path,
    relay: String,
    accounts: &[(&str, &str)],
    active: &str,
    forget: &[&str],
) -> StartConfig {
    serde_json::from_value(json!({
        "data_dir": root.join("data"), "cache_dir": root.join("cache"),
        "log_path": root.join("app.log"), "device_name": "phone",
        "relay": {"url": relay, "tls": "PlainLoopback"},
        "accounts": accounts.iter()
            .map(|(id, token)| json!({"id": id, "token": {"Static": {"bearer": token}}}))
            .collect::<Vec<_>>(),
        "active": active,
        "forget": forget,
    }))
    .unwrap()
}

fn profile_of(embedded: &Embedded, account: &str) -> ProfileId {
    embedded
        .sessions
        .sessions
        .iter()
        .find(|session| session.account.as_deref() == Some(account))
        .map(|session| session.profile)
        .unwrap_or_else(|| panic!("{account} has no session"))
}

fn remembered(root: &Path) -> BTreeMap<String, ProfileId> {
    let bytes = std::fs::read(root.join("cache/fleet/profiles.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_account_leaves_nothing_of_its_profile_behind() {
    let net = TestNet::builder()
        .cloud()
        .daemon("home")
        .cloud_only()
        .cloud_user("personal")
        .cloud_user("work")
        .start()
        .await;
    let (_, personal) = net.user_credentials("personal");
    let (_, work) = net.user_credentials("work");
    let relay = format!("http://{}", net.relay_addr());
    let root = test_root();
    let data = root.path().join("data/installation/profiles");
    let cache = root.path().join("cache");

    // Both accounts on the device, each on a profile of its own, with
    // something cached for each.
    let both = config(
        root.path(),
        relay.clone(),
        &[("personal", &personal), ("work", &work)],
        "personal",
        &[],
    );
    let (requests, _receive) = mpsc::channel(1);
    let mut embedded = Embedded::open(&both, requests).await.unwrap();
    let kept = profile_of(&embedded, "personal");
    let removed = profile_of(&embedded, "work");
    embedded.shutdown().await;
    for profile in [kept, removed] {
        std::fs::create_dir_all(cache.join("fleet")).unwrap();
        std::fs::write(cache.join(format!("fleet/{profile}.json")), b"{}").unwrap();
        std::fs::create_dir_all(cache.join(format!("artifacts/{profile}"))).unwrap();
        std::fs::write(cache.join(format!("artifacts/{profile}/blob")), b"bytes").unwrap();
    }
    assert!(data.join(removed.to_string()).is_dir());
    assert_eq!(remembered(root.path()).get("work"), Some(&removed));

    // The work account removed. The next start deletes its profile before
    // opening anything, and the personal account is exactly where it was.
    let without = config(
        root.path(),
        relay.clone(),
        &[("personal", &personal)],
        "personal",
        &["work"],
    );
    let (requests, _receive) = mpsc::channel(1);
    let mut embedded = Embedded::open(&without, requests).await.unwrap();
    assert_eq!(profile_of(&embedded, "personal"), kept);
    assert!(
        !data.join(removed.to_string()).exists(),
        "the removed account's profile, key and pairings are still on the device"
    );
    assert!(!cache.join(format!("fleet/{removed}.json")).exists());
    assert!(!cache.join(format!("artifacts/{removed}")).exists());
    assert!(!remembered(root.path()).values().any(|id| *id == removed));
    assert!(data.join(kept.to_string()).is_dir());
    assert!(cache.join(format!("fleet/{kept}.json")).exists());
    assert!(cache.join(format!("artifacts/{kept}/blob")).exists());
    embedded.shutdown().await;

    // Said again on a later start, it is not an error: there is nothing left.
    let (requests, _receive) = mpsc::channel(1);
    let mut embedded = Embedded::open(&without, requests).await.unwrap();
    embedded.shutdown().await;

    // Signing the same account in again is a new device to its machines.
    let again = config(
        root.path(),
        relay,
        &[("personal", &personal), ("work", &work)],
        "work",
        &[],
    );
    let (requests, _receive) = mpsc::channel(1);
    let mut embedded = Embedded::open(&again, requests).await.unwrap();
    assert_ne!(profile_of(&embedded, "work"), removed);
    assert_eq!(profile_of(&embedded, "personal"), kept);
    embedded.shutdown().await;
}

#[test]
fn an_account_cannot_be_opened_and_forgotten_at_once() {
    let root = test_root();
    let relay = "http://127.0.0.1:1".to_owned();
    let opened = config(
        root.path(),
        relay.clone(),
        &[("work", "t")],
        "work",
        &["work"],
    );
    assert!(opened.endpoint().is_err());
    let signed_out: StartConfig = serde_json::from_value(json!({
        "data_dir": root.path().join("data"), "cache_dir": root.path().join("cache"),
        "log_path": root.path().join("app.log"), "device_name": "phone",
        "active": "work", "forget": ["work"],
    }))
    .unwrap();
    assert!(signed_out.endpoint().is_err());
}
