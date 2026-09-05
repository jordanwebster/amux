//! Last displayed inventory, independent of the live reducer and its send gates.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use amux_ui::Model;

use crate::projection::Event;

pub struct FleetCache {
    path: PathBuf,
    fleet: Event,
}

impl FleetCache {
    pub fn open(directory: &Path) -> Self {
        let path = directory.join("fleet.json");
        // The cache is disposable across schema changes or interrupted writes.
        let mut fleet = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(|event| matches!(event, Event::Fleet { .. }))
            .unwrap_or(Event::Fleet {
                epoch: 0,
                agents: vec![],
                hosts: vec![],
                reconciled: false,
            });
        let Event::Fleet { reconciled, .. } = &mut fleet else {
            unreachable!()
        };
        *reconciled = false;
        Self { path, fleet }
    }

    pub fn initial(&self) -> Event {
        self.fleet.clone()
    }

    pub fn update(&mut self, event: &mut Event, model: &Model) -> io::Result<()> {
        let Event::Fleet {
            agents,
            hosts,
            reconciled,
            ..
        } = event
        else {
            return Ok(());
        };
        let Event::Fleet {
            agents: previous,
            hosts: previous_hosts,
            ..
        } = &self.fleet
        else {
            unreachable!()
        };
        let mut awaiting = BTreeSet::new();
        // Only an authenticated remote inventory or the complete paired-host
        // list can disprove cached membership. Online status is not authority.
        for card in previous {
            let host_id = card.agent.host_id;
            let unpaired =
                model.is_synchronized() && !hosts.iter().any(|host| host.entry.id == host_id);
            let removed = model
                .remote_inventories()
                .get(&host_id)
                .is_some_and(|ids| !ids.contains(&card.agent.id));
            if !unpaired && !removed && !agents.iter().any(|live| live.agent.id == card.agent.id) {
                awaiting.insert(card.agent.id);
                agents.push(card.clone());
            }
        }
        agents.sort_by_key(|card| {
            previous
                .iter()
                .position(|old| old.agent.id == card.agent.id)
                .unwrap_or(usize::MAX)
        });
        for host in previous_hosts {
            if !model.is_synchronized()
                && awaiting.iter().any(|id| {
                    previous
                        .iter()
                        .any(|card| card.agent.id == *id && card.agent.host_id == host.entry.id)
                })
                && !hosts.iter().any(|live| live.entry.id == host.entry.id)
            {
                let mut host = host.clone();
                host.entry.online = false;
                hosts.push(host);
            }
        }
        hosts.sort_by_key(|host| {
            previous_hosts
                .iter()
                .position(|old| old.entry.id == host.entry.id)
                .unwrap_or(usize::MAX)
        });
        *reconciled &= awaiting.is_empty();
        self.fleet = event.clone();
        self.write()
    }

    fn write(&self) -> io::Result<()> {
        fs::create_dir_all(self.path.parent().expect("cache directory"))?;
        let temporary = self.path.with_extension("json.tmp");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, &self.fleet)?;
        file.flush()?;
        fs::rename(temporary, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use amux::RelayConnection;
    use amux_ui::{Msg, ServerMsg, update};
    use uuid::Uuid;

    use super::*;
    use crate::projection::Projection;

    fn host(id: u128, online: bool) -> ServerMsg {
        ServerMsg::HostUpserted {
            host: amux::HostEntry {
                id: Uuid::from_u128(id),
                name: format!("host-{id}"),
                online,
                version: None,
                capabilities: None,
                trust_status: amux::HostTrustStatus::Trusted,
                last_dial_error: None,
            },
        }
    }

    fn agent(id: u128, host: u128) -> ServerMsg {
        ServerMsg::AgentUpserted {
            agent: amux::Agent {
                id: Uuid::from_u128(id),
                host_id: Uuid::from_u128(host),
                name: Some(format!("agent-{id}")),
                command: "cat".into(),
                working_dir: "/work".into(),
                kind: amux::AgentKind::TestAgent,
                readonly: false,
                args: vec![],
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                parent: None,
                working_on: None,
            },
        }
    }

    fn collect(cache: &mut FleetCache, projection: &mut Projection, model: &Model) -> Event {
        let mut events = vec![];
        projection.collect(model, &RelayConnection::Connected, &mut events);
        let fleet = events.iter_mut().find(|event| matches!(event, Event::Fleet { .. }))
            .expect("inventory authority must trigger a fleet callback even if live rows did not change");
        cache.update(fleet, model).unwrap();
        fleet.clone()
    }

    fn check(fleet: &Event, ids: &[u128], synchronized: bool) {
        let Event::Fleet {
            agents, reconciled, ..
        } = fleet
        else {
            panic!("Fleet expected")
        };
        assert_eq!(
            agents
                .iter()
                .map(|card| card.agent.id.as_u128())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(*reconciled, synchronized);
    }

    #[test]
    fn mobile_cache_authority_is_per_host_and_survives_frame_coalescing() {
        let root = tempfile::tempdir().unwrap();
        let mut model = Model::default();
        for msg in [
            ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(99)),
            },
            host(1, true),
            host(2, true),
            agent(11, 1),
            agent(12, 1),
            agent(21, 2),
            ServerMsg::HostsSynchronized,
            ServerMsg::AgentsSynchronized,
        ] {
            update(&mut model, Msg::Server(msg));
        }
        let mut cache = FleetCache::open(root.path());
        collect(&mut cache, &mut Projection::default(), &model);
        let mut cache = FleetCache::open(root.path());
        let mut projection = Projection::default();
        let mut model = Model::default();
        for msg in [
            ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(99)),
            },
            host(1, true),
            host(2, false),
            ServerMsg::HostsSynchronized,
            ServerMsg::AgentsSynchronized,
        ] {
            update(&mut model, Msg::Server(msg));
        }
        check(
            &collect(&mut cache, &mut projection, &model),
            &[11, 12, 21],
            false,
        );
        assert_eq!(model.agent_count(), 0);
        update(&mut model, Msg::Server(agent(12, 1)));
        check(
            &collect(&mut cache, &mut projection, &model),
            &[11, 12, 21],
            false,
        );
        update(
            &mut model,
            Msg::Server(ServerMsg::HostInventory {
                host_id: Uuid::from_u128(1),
                agent_ids: vec![Uuid::from_u128(12)],
            }),
        );
        check(
            &collect(&mut cache, &mut projection, &model),
            &[12, 21],
            false,
        );
        // An empty snapshot changes no live row. It still resolves the cached row.
        update(
            &mut model,
            Msg::Server(ServerMsg::HostInventory {
                host_id: Uuid::from_u128(2),
                agent_ids: vec![],
            }),
        );
        check(&collect(&mut cache, &mut projection, &model), &[12], true);
        // Reachability removals preserve known membership even when they share a frame.
        update(&mut model, Msg::Server(host(1, false)));
        update(
            &mut model,
            Msg::Server(ServerMsg::AgentRemoved {
                id: Uuid::from_u128(12),
            }),
        );
        check(&collect(&mut cache, &mut projection, &model), &[12], false);
        // HostRemoved signifies removal from the paired set, including while offline.
        update(
            &mut model,
            Msg::Server(ServerMsg::HostRemoved {
                id: Uuid::from_u128(1),
            }),
        );
        check(&collect(&mut cache, &mut projection, &model), &[], true);
        assert_eq!(model.agent_count(), 0);
        let Event::Fleet { hosts, .. } = cache.initial() else {
            unreachable!()
        };
        assert!(hosts.iter().all(|host| host.entry.id != Uuid::from_u128(1)));
        update(&mut model, Msg::Server(host(1, true)));
        update(&mut model, Msg::Server(agent(12, 1)));
        check(&collect(&mut cache, &mut projection, &model), &[12], true);
        update(
            &mut model,
            Msg::Server(ServerMsg::AgentRemoved {
                id: Uuid::from_u128(12),
            }),
        );
        let ServerMsg::HostUpserted { mut host } = host(1, true) else {
            unreachable!()
        };
        host.trust_status = amux::HostTrustStatus::UntrustedButOnline;
        update(&mut model, Msg::Server(ServerMsg::HostUpserted { host }));
        check(&collect(&mut cache, &mut projection, &model), &[], true);
    }
}
