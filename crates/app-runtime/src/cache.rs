//! The remembered fleet: what this device's store last knew about one
//! account's machines and agents, projected the way the running library
//! projects it.
//!
//! Nothing here writes. The account's runtime keeps its store current from
//! the fleet stream; a launch reads the same rows back before it has a
//! runtime at all, so the screen a launch draws and the screen a connection
//! replaces it with come from one source through one projection.

use std::path::{Path, PathBuf};

use model::{DisconnectReason, HostId, RelayConnection};
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

/// Where one account's store lives under an application's cache directory.
///
/// The remembered fleet is an account's, not the phone's: its rows are that
/// account's machines and that account's agents, and a phone signed in to two
/// accounts must never draw one of them under the other's name. Each account
/// therefore keeps a store of its own, named so a launch can find it from the
/// account alone.
pub fn store_path(cache_dir: &Path, account: &str) -> PathBuf {
    cache_dir.join("store").join(file_name(account))
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
    let local = store
        .view_get(kind, key)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.parse().ok());
    Ok(remembered(fleet, generations, local))
}

/// Opens an account's store, reads its remembered fleet and closes it again.
/// A store that is missing, unreadable or refused is a fleet with no rows:
/// the store is disposable, and a launch without one still has to draw.
pub async fn read_cached_fleet(cache_dir: &Path, account: &str) -> Event {
    let path = store_path(cache_dir, account);
    let fleet = match path.exists() {
        true => match Store::open(&path).await {
            Ok(store) => {
                let fleet = cached_fleet(&store).await.ok();
                store.close().await;
                fleet
            }
            Err(_) => None,
        },
        false => None,
    };
    fleet.unwrap_or(Event::Fleet {
        epoch: 0,
        agents: vec![],
        hosts: vec![],
        reconciled: false,
    })
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
    use store::{FleetDelta, FleetSnapshot};
    use uuid::Uuid;

    use super::*;

    const PHONE: Uuid = Uuid::from_u128(9);

    fn host(id: u128, trust: model::HostTrustStatus) -> model::HostEntry {
        model::HostEntry {
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
        store
            .view_set(kind, key, &PHONE.to_string())
            .await
            .unwrap();
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
        let fleet = read_cached_fleet(root.path(), "owner").await;
        let mut remembered = cards(&fleet);
        remembered.sort();
        assert_eq!(remembered, [(11, true), (12, true)]);
        assert_eq!(hosts(&fleet), [1]);
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
            cards(&read_cached_fleet(root.path(), "owner").await),
            [(12, true)]
        );
    }

    #[tokio::test]
    async fn each_account_reads_its_own_store_and_a_missing_or_corrupt_one_is_empty() {
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
            cards(&read_cached_fleet(root.path(), "Personal@example.com").await),
            [(11, true)]
        );
        assert_ne!(
            store_path(root.path(), "Personal@example.com"),
            store_path(root.path(), "personal@example.com")
        );
        assert!(cards(&read_cached_fleet(root.path(), "work").await).is_empty());

        let corrupt = store_path(root.path(), "corrupt");
        std::fs::write(&corrupt, b"not a database").unwrap();
        assert!(cards(&read_cached_fleet(root.path(), "corrupt").await).is_empty());
    }
}
