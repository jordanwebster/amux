//! The remembered fleet: what this device's store last knew about one
//! account's machines and agents, projected the way the running library
//! projects it.
//!
//! The account's runtime keeps its store current from
//! the fleet stream; a launch reads the same rows back before it has a
//! runtime at all, so the screen a launch draws and the screen a connection
//! replaces it with come from one source through one projection.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::{fs, io};

use model::{DisconnectReason, HostId, ProfileId, RelayConnection};
use store::{Fleet, Store, StoreError};
use ui_state::{Effect, Model, Msg, ProfileGeneration, StoreMsg, StoreOp, update};

use crate::projection::{Event, Projection};

/// The name of the signpost span a launch measures around its store read:
/// opening the account's store and selecting its fleet before the first frame.
pub const STORE_READ_SPAN: &str = "amux.store.read";

/// The file name an account's store is kept under.
///
/// An account identifier is the application's — normally an email address —
/// so it cannot be used as a path component as it stands: it may hold a
/// separator, and two addresses differing only in case would be one file on a
/// phone, whose filesystem does not distinguish them. Everything outside a
/// lowercase, unambiguous set is therefore escaped rather than replaced, so
/// distinct accounts always name distinct files.
pub fn file_name(account: &str) -> String {
    let mut name = String::with_capacity(account.len() + 8);
    for byte in account.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => name.push(byte as char),
            _ => name.push_str(&format!("%{byte:02x}")),
        }
    }
    name.push_str(".sqlite");
    name
}

/// Where one profile keeps its SQLite database. The private directory also
/// contains its WAL, locks and corruption quarantine, so removing a profile
/// removes all of its cached data without touching another profile.
pub fn store_path(cache_dir: &Path, account: &str) -> PathBuf {
    cache_dir
        .join("store")
        .join(file_name(account))
        .join("store.sqlite")
}

/// Where the account-to-profile directory is kept, beside the fleets it
/// explains.
fn directory_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("fleet").join("profiles.json")
}

/// The key an account is recorded under. A device with nobody signed in still
/// has a profile and still remembers a fleet, so the empty key names it.
fn directory_key(account: Option<&str>) -> String {
    account.unwrap_or_default().to_owned()
}

/// Record which profile each account is on, and which one this device uses
/// with nobody signed in.
///
/// A launch has rows to draw before it has started anything, and a profile
/// identifier is made by the installation rather than chosen by the
/// application, so an application cannot name the file its own fleet is in.
/// This is how it finds out: written by the process that opened the profiles,
/// read by the next launch before one exists.
pub fn remember_profiles(
    cache_dir: &Path,
    profiles: &BTreeMap<String, ProfileId>,
) -> io::Result<()> {
    let path = directory_path(cache_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec(profiles)?)
}

/// Where one profile's downloaded artifacts are cached.
pub fn artifacts_dir(cache_dir: &Path, profile: ProfileId) -> PathBuf {
    cache_dir.join("artifacts").join(profile.to_string())
}

/// Delete everything this device cached for one profile: the fleet it
/// remembered, the artifacts it downloaded, and every entry in the directory
/// that pointed an account at it. Anything already gone is not an error.
pub fn forget_profile(cache_dir: &Path, profile: ProfileId) -> io::Result<()> {
    fn gone(result: io::Result<()>) -> io::Result<()> {
        match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
    let store = store_path(cache_dir, &profile.to_string());
    gone(fs::remove_dir_all(
        store.parent().expect("profile store directory"),
    ))?;
    gone(fs::remove_dir_all(artifacts_dir(cache_dir, profile)))?;
    let path = directory_path(cache_dir);
    let Ok(bytes) = fs::read(&path) else {
        return Ok(());
    };
    let Ok(mut profiles) = serde_json::from_slice::<BTreeMap<String, ProfileId>>(&bytes) else {
        return Ok(());
    };
    let before = profiles.len();
    profiles.retain(|_, remembered| *remembered != profile);
    if profiles.len() == before {
        return Ok(());
    }
    fs::write(&path, serde_json::to_vec(&profiles)?)
}

/// Which profile an account's remembered fleet is under, as the last run left
/// it. `None` asks for the profile this device uses signed out.
pub fn remembered_profile(cache_dir: &Path, account: Option<&str>) -> Option<ProfileId> {
    let bytes = fs::read(directory_path(cache_dir)).ok()?;
    let profiles: BTreeMap<String, ProfileId> = serde_json::from_slice(&bytes).ok()?;
    profiles.get(&directory_key(account)).copied()
}

/// The fleet the store remembers, as the Fleet event a running library
/// delivers before any machine has answered: every card awaiting its machine,
/// the fleet unreconciled, and this device itself left out of the machines.
pub async fn cached_fleet(store: &Store) -> Result<Event, StoreError> {
    let generations = store
        .generations()
        .for_provider("codex")
        .ok_or(StoreError::Corrupt)?;
    let fleet = store.fleet(generations).await?;
    let (kind, key) = ui_runtime::LOCAL_HOST_VIEW;
    let local = match store.view_get(kind, key).await {
        Ok(value) => value.and_then(|value| value.parse().ok()),
        Err(StoreError::RecoveryRequired) => None,
        Err(error) => return Err(error),
    };
    Ok(remembered(fleet, generations, local))
}

/// Resolve the profile this account last used before reading its SQLite fleet.
/// An account never opened on this device has no remembered rows.
pub async fn read_account_cached_fleet(cache_dir: &Path, account: &str) -> Result<Event, String> {
    if cache_dir.is_file() {
        return read_cached_fleet(cache_dir, account).await;
    }
    let Some(profile) =
        remembered_profile(cache_dir, Some(account).filter(|value| !value.is_empty()))
    else {
        return Ok(empty_fleet());
    };
    read_cached_fleet(cache_dir, &profile.to_string()).await
}

/// Opens an account's store, reads its remembered fleet and closes it again.
/// A missing store is a fleet with no rows because the cache is disposable.
/// Any store that exists but cannot be used is an error with the same cause
/// and remedy the running client reports.
pub async fn read_cached_fleet(cache_dir: &Path, account: &str) -> Result<Event, String> {
    let path = store_path(cache_dir, account);
    match tokio::fs::metadata(cache_dir).await {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(ui_runtime::store_failure_message(&path, StoreError::Io));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(empty_fleet());
        }
        Err(error) => return Err(open_failure_message(&path, &error)),
    }
    let store_dir = path.parent().expect("an account store has a directory");
    match tokio::fs::metadata(store_dir).await {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(ui_runtime::store_failure_message(&path, StoreError::Io));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(empty_fleet());
        }
        Err(error) => return Err(open_failure_message(&path, &error)),
    }
    match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(empty_fleet());
        }
        Err(error) => return Err(open_failure_message(&path, &error)),
    }
    let store = match open_phone_store(&path).await {
        Ok(store) => store,
        Err(error) => {
            let quarantined = complete_corruption_quarantine(&path, error).await;
            return Err(ui_runtime::phone_store_open_failure_message(
                &path,
                error,
                quarantined,
            ));
        }
    };
    let fleet = cached_fleet(&store).await;
    store.close().await;
    match fleet {
        Ok(fleet) => {
            remember_first_frame(&path);
            Ok(fleet)
        }
        Err(error) => Err(ui_runtime::phone_store_open_failure_message(
            &path,
            error,
            complete_corruption_quarantine(&path, error).await,
        )),
    }
}

async fn open_phone_store(path: &Path) -> Result<Store, StoreError> {
    let store = Store::open(path).await?;
    let report = store.quarantine_report().await?;
    if report.is_empty() {
        return Ok(store);
    }
    store.close().await;
    Store::resolve_quarantine(path, &report).await?;
    Store::open(path).await
}

fn first_frames() -> &'static Mutex<HashSet<PathBuf>> {
    static FIRST_FRAMES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    FIRST_FRAMES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn remember_first_frame(path: &Path) {
    first_frames()
        .lock()
        .expect("phone first-frame set poisoned")
        .insert(path.to_owned());
}

pub(crate) fn take_first_frame(path: &Path) -> bool {
    first_frames()
        .lock()
        .expect("phone first-frame set poisoned")
        .remove(path)
}

async fn complete_corruption_quarantine(path: &Path, error: StoreError) -> bool {
    if error != StoreError::Corrupt {
        return false;
    }
    match Store::open(path).await {
        Ok(store) => {
            store.close().await;
            true
        }
        Err(_) => false,
    }
}

fn open_failure_message(path: &Path, error: &std::io::Error) -> String {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        StoreError::Permission
    } else {
        StoreError::Io
    };
    ui_runtime::store_failure_message(path, kind)
}

fn empty_fleet() -> Event {
    Event::Fleet {
        epoch: 0,
        agents: vec![],
        hosts: vec![],
        reconciled: false,
    }
}

/// Installs the stored fleet into a fresh reducer exactly as the running
/// runtime's store startup does, then projects it.
fn remembered(fleet: Fleet, generations: store::Generations, local: Option<HostId>) -> Event {
    let profile = ProfileGeneration(0);
    let mut model = Model::default();
    let op = update(
        &mut model,
        Msg::StoreStartup {
            profile,
            generations,
            window_max_entries: ui_state::store::PHONE_WINDOW_MAX_ENTRIES,
        },
    )
    .into_iter()
    .find_map(|effect| match effect {
        Effect::Store(StoreOp::FleetLoad { op, .. }) => Some(op),
        _ => None,
    })
    .expect("store startup reads the fleet");
    update(
        &mut model,
        Msg::Store(StoreMsg::FleetLoaded { profile, op, fleet }),
    );
    let mut events = Vec::new();
    Projection::default().collect(
        &model,
        &RelayConnection::Disconnected {
            reason: DisconnectReason::Unreachable,
        },
        &mut events,
    );
    let mut fleet = events
        .into_iter()
        .find(|event| matches!(event, Event::Fleet { .. }))
        .expect("a projection always carries its first fleet");
    if let Event::Fleet { hosts, .. } = &mut fleet {
        hosts.retain(|host| Some(host.entry.id) != local);
    }
    fleet
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use rusqlite::Connection;
    use store::{FleetDelta, FleetSnapshot};
    use uuid::Uuid;

    use super::*;

    const PHONE: Uuid = Uuid::from_u128(9);

    fn host(id: u128, trust: model::HostTrustStatus) -> model::HostEntry {
        model::HostEntry {
            signed_in: Some(true),
            via: model::HostVia::Direct,
            id: Uuid::from_u128(id),
            name: format!("host-{id}"),
            online: true,
            version: None,
            capabilities: None,
            trust_status: trust,
            last_dial_error: None,
            platform: None,
        }
    }

    fn agent(id: u128, host: u128, revision: u64) -> model::Agent {
        model::Agent {
            last_activity: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            id: Uuid::from_u128(id),
            host_id: Uuid::from_u128(host),
            name: Some(format!("agent-{id}")),
            command: "cat".into(),
            working_dir: "/work".into(),
            kind: model::AgentKind::TestAgent,
            readonly: false,
            args: vec![],
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            parent: None,
            working_on: None,
            summary: None,
            progress: None,
            inventory_revision: revision,
        }
    }

    fn cards(fleet: &Event) -> Vec<(u128, bool)> {
        let Event::Fleet {
            agents, reconciled, ..
        } = fleet
        else {
            panic!("Fleet expected")
        };
        assert!(!reconciled, "nothing remembered is reconciled");
        agents
            .iter()
            .map(|card| (card.agent.id.as_u128(), card.awaiting))
            .collect()
    }

    fn hosts(fleet: &Event) -> Vec<u128> {
        let Event::Fleet { hosts, .. } = fleet else {
            panic!("Fleet expected")
        };
        hosts.iter().map(|host| host.entry.id.as_u128()).collect()
    }

    async fn seeded(root: &Path, account: &str, deltas: Vec<FleetDelta>) {
        let path = store_path(root, account);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let store = Store::open(&path).await.unwrap();
        let generations = store.generations().for_provider("codex").unwrap();
        for delta in deltas {
            store.apply_fleet(generations, delta).await.unwrap();
        }
        let (kind, key) = ui_runtime::LOCAL_HOST_VIEW;
        store.view_set(kind, key, &PHONE.to_string()).await.unwrap();
        store.close().await;
    }

    #[tokio::test]
    async fn remembered_rows_are_unconfirmed_and_exclude_this_device_and_offers() {
        let root = tempfile::tempdir().unwrap();
        seeded(
            root.path(),
            "owner",
            vec![
                FleetDelta::Host {
                    host: host(9, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
                FleetDelta::Host {
                    host: host(1, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
                FleetDelta::Host {
                    host: host(2, model::HostTrustStatus::UntrustedButOnline),
                    revision: 0,
                },
                FleetDelta::AgentUp {
                    agent: agent(11, 1, 1),
                    revision: 1,
                },
                FleetDelta::AgentUp {
                    agent: agent(12, 1, 2),
                    revision: 2,
                },
                // Remembered from before this device's pairing with host 2
                // was withdrawn.
                FleetDelta::AgentUp {
                    agent: agent(21, 2, 1),
                    revision: 1,
                },
            ],
        )
        .await;
        let fleet = read_cached_fleet(root.path(), "owner").await.unwrap();
        let mut remembered = cards(&fleet);
        remembered.sort();
        assert_eq!(remembered, [(11, true), (12, true)]);
        assert_eq!(hosts(&fleet), [1]);
    }

    /// A remembered agent is drawn with the standing its machine last
    /// published and that standing's own age, not the agent's creation time.
    /// Only a machine last known to be offline withholds its agents' standing.
    #[tokio::test]
    async fn remembered_agents_keep_their_stored_standing_and_age() {
        let created = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        // Three days after the agents were created, as a launch would read them.
        let minutes = |minutes: i64| {
            created + chrono::TimeDelta::days(3) - chrono::TimeDelta::minutes(minutes)
        };
        let standing = |id: u128, host: u128, attention, phase, at| {
            let mut agent = agent(id, host, 1);
            agent.kind = model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            };
            agent.created_at = created;
            agent.summary = Some(model::SummaryEnvelope {
                through: 1,
                producer_version: ui_state::summary_producer_version(
                    model::StructuredProtocol::ClaudePtyTranscript,
                ),
                observed_at: at,
                stale: false,
                revision: 1,
                summary: model::Summary {
                    attention,
                    phase,
                    last_activity: Some(at),
                    todo: None,
                    context: None,
                    model: None,
                    unknown: vec![],
                },
            });
            FleetDelta::AgentUp {
                agent,
                revision: id as u64,
            }
        };
        let mut away = host(2, model::HostTrustStatus::Trusted);
        away.online = false;
        let root = tempfile::tempdir().unwrap();
        seeded(
            root.path(),
            "owner",
            vec![
                FleetDelta::Host {
                    host: host(1, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
                FleetDelta::Host {
                    host: away,
                    revision: 0,
                },
                standing(
                    11,
                    1,
                    model::Attention::NeedsYou {
                        why: model::Why::Permission,
                    },
                    model::AgentPhase::Running,
                    minutes(2),
                ),
                standing(
                    12,
                    1,
                    model::Attention::Working,
                    model::AgentPhase::Running,
                    minutes(1),
                ),
                standing(
                    13,
                    1,
                    model::Attention::Idle,
                    model::AgentPhase::Exited { exit_code: Some(0) },
                    minutes(14),
                ),
                standing(
                    21,
                    2,
                    model::Attention::Working,
                    model::AgentPhase::Running,
                    minutes(30),
                ),
            ],
        )
        .await;

        let Event::Fleet { agents, .. } = read_cached_fleet(root.path(), "owner").await.unwrap()
        else {
            panic!("Fleet expected")
        };
        let mut drawn: Vec<_> = agents
            .iter()
            .map(|card| {
                (
                    card.agent.id.as_u128(),
                    card.attention,
                    card.phase.clone(),
                    card.last_activity,
                    card.awaiting,
                )
            })
            .collect();
        drawn.sort_by_key(|(id, ..)| *id);
        assert_eq!(
            drawn,
            [
                (
                    11,
                    model::Attention::NeedsYou {
                        why: model::Why::Permission
                    },
                    model::AgentPhase::Running,
                    minutes(2),
                    true,
                ),
                (
                    12,
                    model::Attention::Working,
                    model::AgentPhase::Running,
                    minutes(1),
                    true,
                ),
                (
                    13,
                    model::Attention::Idle,
                    model::AgentPhase::Exited { exit_code: Some(0) },
                    minutes(14),
                    true,
                ),
                (
                    21,
                    model::Attention::Unknown,
                    model::AgentPhase::Running,
                    minutes(30),
                    true,
                ),
            ]
        );
    }

    #[tokio::test]
    async fn an_authoritative_removal_is_not_remembered() {
        let root = tempfile::tempdir().unwrap();
        seeded(
            root.path(),
            "owner",
            vec![
                FleetDelta::Host {
                    host: host(1, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
                FleetDelta::AgentUp {
                    agent: agent(11, 1, 1),
                    revision: 1,
                },
                FleetDelta::AgentUp {
                    agent: agent(12, 1, 2),
                    revision: 2,
                },
                FleetDelta::Snapshot(FleetSnapshot {
                    host_id: Uuid::from_u128(1),
                    through_revision: 3,
                    agents: vec![(agent(12, 1, 2), 2)],
                }),
            ],
        )
        .await;
        assert_eq!(
            cards(&read_cached_fleet(root.path(), "owner").await.unwrap()),
            [(12, true)]
        );
    }

    #[tokio::test]
    async fn each_account_reads_its_own_store_and_only_a_missing_one_is_empty() {
        let root = tempfile::tempdir().unwrap();
        seeded(
            root.path(),
            "Personal@example.com",
            vec![
                FleetDelta::Host {
                    host: host(1, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
                FleetDelta::AgentUp {
                    agent: agent(11, 1, 1),
                    revision: 1,
                },
            ],
        )
        .await;
        assert_eq!(
            cards(
                &read_cached_fleet(root.path(), "Personal@example.com")
                    .await
                    .unwrap()
            ),
            [(11, true)]
        );
        assert_ne!(
            store_path(root.path(), "Personal@example.com"),
            store_path(root.path(), "personal@example.com")
        );
        assert!(cards(&read_cached_fleet(root.path(), "work").await.unwrap()).is_empty());

        let corrupt = store_path(root.path(), "corrupt");
        std::fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
        std::fs::write(&corrupt, b"not a database").unwrap();
        let failure = read_cached_fleet(root.path(), "corrupt").await.unwrap_err();
        assert!(failure.contains("it is corrupt"), "{failure}");
        assert!(failure.contains("has been quarantined"), "{failure}");
        assert!(
            failure.contains("nothing the daemon still retains is lost"),
            "{failure}"
        );
        let empty = read_cached_fleet(root.path(), "corrupt").await.unwrap();
        assert!(cards(&empty).is_empty());
        assert!(hosts(&empty).is_empty());
    }

    #[tokio::test]
    async fn unresolved_quarantine_keeps_the_remembered_fleet_with_unknown_local_host() {
        let root = tempfile::tempdir().unwrap();
        let path = store_path(root.path(), "owner");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not a database").unwrap();
        let failure = read_cached_fleet(root.path(), "owner").await.unwrap_err();
        assert!(failure.contains("has been quarantined"), "{failure}");

        let store = Store::open(&path).await.unwrap();
        let (kind, key) = ui_runtime::LOCAL_HOST_VIEW;
        assert_eq!(
            store.view_get(kind, key).await,
            Err(StoreError::RecoveryRequired)
        );
        let generations = store.generations().for_provider("codex").unwrap();
        store
            .apply_fleet(
                generations,
                FleetDelta::Host {
                    host: host(1, model::HostTrustStatus::Trusted),
                    revision: 0,
                },
            )
            .await
            .unwrap();
        store
            .apply_fleet(
                generations,
                FleetDelta::AgentUp {
                    agent: agent(11, 1, 1),
                    revision: 1,
                },
            )
            .await
            .unwrap();
        store.close().await;

        let fleet = read_cached_fleet(root.path(), "owner").await.unwrap();
        assert_eq!(cards(&fleet), [(11, true)]);
        assert_eq!(hosts(&fleet), [1]);
    }

    #[tokio::test]
    async fn an_unusable_cache_root_is_not_mistaken_for_a_missing_store() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, "file").unwrap();
        let failure = read_cached_fleet(&file, "owner").await.unwrap_err();
        assert!(
            failure.contains("reading or writing it failed"),
            "{failure}"
        );
        assert!(
            failure.contains("delete the file to start with an empty cache"),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn a_cold_start_frame_enables_phone_store_maintenance() {
        let root = tempfile::tempdir().unwrap();
        let path = store_path(root.path(), "owner");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let store = Store::open(&path).await.unwrap();
        store.close().await;

        let raw = Connection::open(&path).unwrap();
        let id = model::AgentId::from_u128(77).to_string();
        raw.execute(
            "INSERT INTO chat_state(agent_id,revision,content_revision,segment_high_water,
                previous_through,needs_baseline,retiring) VALUES (?1,0,0,1,NULL,0,0)",
            [&id],
        )
        .unwrap();
        raw.execute(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<512)
             INSERT INTO claude_sdk_entry(agent_id,key,segment,order_seq,order_slot,
                revision_seq,revision_fence,revision_ordinal,kind,text,bytes,body)
             SELECT ?1,printf('cold-%05d',x),1,x,0,x,0,0,'prompt',NULL,4096,
                zeroblob(4096) FROM n",
            [&id],
        )
        .unwrap();
        drop(raw);
        let before = store_disk_bytes(&path);

        read_cached_fleet(root.path(), "owner").await.unwrap();
        let mut runtime = ui_runtime::Runtime::start(
            Box::new(|| Box::pin(std::future::pending())),
            ui_runtime::RuntimeOptions {
                store_path: Some(path.clone()),
                store_maintenance_budget: store::Budget {
                    store_target_bytes: before / 2,
                    ..store::Budget::phone()
                },
                store_recovery: ui_runtime::StoreRecovery::Relaunch,
                store_first_frame_seen: take_first_frame(&path),
                ..Default::default()
            },
        );
        assert!(runtime.next_message().await);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store_disk_bytes(&path) >= before {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("phone maintenance did not reclaim after its cold-start frame");
    }

    fn store_disk_bytes(path: &Path) -> u64 {
        [
            path.to_path_buf(),
            PathBuf::from(format!("{}-wal", path.display())),
            PathBuf::from(format!("{}-shm", path.display())),
        ]
        .into_iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
    }
}
