//! Last displayed inventory, independent of the live reducer and its send gates.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use amux_ui::AgentId;

use crate::projection::Event;

pub struct FleetCache {
    path: PathBuf,
    fleet: Event,
    awaiting: BTreeSet<AgentId>,
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
        let Event::Fleet {
            agents, reconciled, ..
        } = &mut fleet
        else {
            unreachable!()
        };
        *reconciled = false;
        let awaiting = agents.iter().map(|card| card.agent.id).collect();
        Self {
            path,
            fleet,
            awaiting,
        }
    }

    pub fn initial(&self) -> Event {
        self.fleet.clone()
    }

    pub fn update(&mut self, event: &mut Event) -> io::Result<()> {
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
        for card in agents.iter() {
            self.awaiting.remove(&card.agent.id);
        }
        for card in previous {
            if !agents.iter().any(|live| live.agent.id == card.agent.id)
                && (!*reconciled
                    || !hosts
                        .iter()
                        .any(|host| host.entry.id == card.agent.host_id && host.entry.online))
            {
                self.awaiting.insert(card.agent.id);
            }
        }
        // A local snapshot can complete before the remote host inventory arrives.
        // Keep unconfirmed cached cards out of the reducer, but visible in place.
        for card in previous {
            if self.awaiting.contains(&card.agent.id) {
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
            if self.awaiting.iter().any(|id| {
                previous
                    .iter()
                    .any(|card| card.agent.id == *id && card.agent.host_id == host.entry.id)
            }) && !hosts.iter().any(|live| live.entry.id == host.entry.id)
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
        *reconciled &= self.awaiting.is_empty();
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
