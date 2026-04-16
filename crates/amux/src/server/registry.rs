use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::message::Host;
use crate::protocol::route::Route;

/// Centralized agent tracking with bidirectional name<->UUID mapping.
/// Tracks both local and remote agents. Routes for remote agents are derived
/// from the host table at read time.
pub(super) struct AgentRegistry {
    name_to_uuid: HashMap<String, Uuid>,
    uuid_to_name: HashMap<Uuid, String>,
    entries: HashMap<Uuid, Agent>,
}

#[derive(Debug, Error)]
pub(super) enum AgentRegistryError {
    #[error("Cannot update remote agent: {0}")]
    IsRemote(String),
    #[error("Agent not found: {0}")]
    NotFound(String),
    #[error("Agent already exists: {0}")]
    AlreadyExists(String),
}

impl AgentRegistry {
    pub(super) fn new() -> Self {
        Self {
            name_to_uuid: HashMap::new(),
            uuid_to_name: HashMap::new(),
            entries: HashMap::new(),
        }
    }

    /// Register a local agent. Errors if UUID exists or name is taken.
    pub(super) fn register_local(&mut self, info: Agent) -> Result<(), AgentRegistryError> {
        if self.entries.contains_key(&info.id) {
            return Err(AgentRegistryError::AlreadyExists(info.id.to_string()));
        }
        if let Some(ref name) = info.name {
            if self.name_to_uuid.contains_key(name) {
                return Err(AgentRegistryError::AlreadyExists(name.clone()));
            }
            self.name_to_uuid.insert(name.clone(), info.id);
            self.uuid_to_name.insert(info.id, name.clone());
        }
        self.entries.insert(info.id, info);
        Ok(())
    }

    /// Register a remote agent. Upserts by UUID; clears old name on re-announce.
    /// Stores the latest metadata for the UUID, but keeps alias ownership unique:
    /// a colliding alias stays with its current owner.
    pub(super) fn register_remote(&mut self, info: Agent) -> Result<(), AgentRegistryError> {
        // On re-announce (same UUID), clear the old name mapping
        if let Some(old_name) = self.uuid_to_name.remove(&info.id) {
            // Only remove from name_to_uuid if it still points to this UUID
            if self.name_to_uuid.get(&old_name) == Some(&info.id) {
                self.name_to_uuid.remove(&old_name);
            }
        }

        // Try to claim the new name (first-one-wins)
        if let Some(ref name) = info.name
            && !self.name_to_uuid.contains_key(name)
        {
            self.name_to_uuid.insert(name.clone(), info.id);
            self.uuid_to_name.insert(info.id, name.clone());
        }
        // else: silently skip — name is taken by another agent

        self.entries.insert(info.id, info);
        Ok(())
    }

    /// Update an existing local agent entry in place.
    ///
    /// The entry must already exist locally. Alias ownership stays unique:
    /// renaming to a colliding alias fails without mutating the registry.
    pub(super) fn update_local(&mut self, info: Agent) -> Result<(), AgentRegistryError> {
        let Some(existing) = self.entries.get(&info.id) else {
            return Err(AgentRegistryError::NotFound(info.id.to_string()));
        };
        if existing.is_remote() {
            return Err(AgentRegistryError::IsRemote(info.id.to_string()));
        }

        if let Some(ref new_name) = info.name
            && let Some(owner) = self.name_to_uuid.get(new_name)
            && owner != &info.id
        {
            return Err(AgentRegistryError::AlreadyExists(new_name.clone()));
        }

        if let Some(old_name) = self.uuid_to_name.remove(&info.id)
            && self.name_to_uuid.get(&old_name) == Some(&info.id)
        {
            self.name_to_uuid.remove(&old_name);
        }

        if let Some(ref name) = info.name {
            self.name_to_uuid.insert(name.clone(), info.id);
            self.uuid_to_name.insert(info.id, name.clone());
        }

        self.entries.insert(info.id, info);
        Ok(())
    }

    /// Remove an agent by UUID. Returns the removed entry if found.
    pub(super) fn remove(&mut self, uuid: &Uuid) -> Option<Agent> {
        let info = self.entries.remove(uuid)?;
        if let Some(name) = self.uuid_to_name.remove(uuid)
            && self.name_to_uuid.get(&name) == Some(uuid)
        {
            self.name_to_uuid.remove(&name);
        }
        Some(info)
    }

    /// Remove all remote agents whose host route matches a predicate.
    /// Returns removed UUIDs.
    ///
    /// `host_route` resolves a host_id to its route; agents whose host is
    /// unknown (returns `None`) are skipped.
    pub(super) fn remove_where(
        &mut self,
        host_route: impl Fn(Uuid) -> Option<Route>,
        predicate: impl Fn(&Route) -> bool,
    ) -> Vec<Uuid> {
        let removed: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.is_remote() && host_route(e.host_id).as_ref().is_some_and(&predicate)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &removed {
            self.remove(id);
        }
        removed
    }

    /// Resolve an identifier to an Agent.
    ///
    /// Resolution order:
    /// 1. "route:id" — parse route, resolve id recursively, return with explicit route
    /// 2. UUID — look up directly
    /// 3. Name — look up via name_to_uuid
    pub(super) fn resolve(&self, hosts: &HashMap<Uuid, Host>, identifier: &str) -> Option<Agent> {
        // Try splitting on last ':' for route:id format
        match identifier.rsplit_once(':') {
            Some((route_str, id)) => {
                let deserializer =
                    serde::de::value::StrDeserializer::<serde::de::value::Error>::new(route_str);
                // A malformed route prefix (e.g. "bad..hop:agent") is a lookup miss, not a bug.
                let supplied_route: Route = Route::deserialize(deserializer).ok()?;
                match self.resolve_inner(hosts, id) {
                    // If a fully qualified route is supplied, it must match the one that exists
                    // in the routing table.
                    Some(info) if info.route == supplied_route => Some(info),
                    _ => None,
                }
            }
            None => self.resolve_inner(hosts, identifier),
        }
    }

    /// Resolve by UUID or name (no route: prefix handling)
    fn resolve_inner(&self, hosts: &HashMap<Uuid, Host>, identifier: &str) -> Option<Agent> {
        let uuid = match Uuid::parse_str(identifier) {
            Ok(uuid) => uuid,
            Err(_) => *self.name_to_uuid.get(identifier)?,
        };

        self.materialize(hosts, &uuid)
    }

    /// Get a stored entry by UUID.
    pub(super) fn get(&self, uuid: &Uuid) -> Option<&Agent> {
        self.entries.get(uuid)
    }

    /// Materialize an entry with its effective route.
    pub(super) fn materialize(&self, hosts: &HashMap<Uuid, Host>, uuid: &Uuid) -> Option<Agent> {
        let mut info = self.entries.get(uuid)?.clone();
        if info.is_remote() {
            info.route = hosts.get(&info.host_id)?.route.clone();
        } else {
            info.route = Route::empty();
        }
        Some(info)
    }

    /// Check if a UUID is registered
    pub(super) fn contains(&self, uuid: &Uuid) -> bool {
        self.entries.contains_key(uuid)
    }

    /// Check if an name is taken
    pub(super) fn name_taken(&self, name: &str) -> bool {
        self.name_to_uuid.contains_key(name)
    }

    /// List all agents with effective routes populated.
    pub(super) fn list_all(&self, hosts: &HashMap<Uuid, Host>) -> Vec<Agent> {
        self.entries
            .keys()
            .filter_map(|uuid| self.materialize(hosts, uuid))
            .collect()
    }

    /// Count of remote agents
    pub(super) fn count_remote(&self) -> usize {
        self.entries.values().filter(|e| e.is_remote()).count()
    }

    /// Iterate over all entries (for send_initial_announcements)
    pub(super) fn iter_entries(&self) -> impl Iterator<Item = (&Uuid, &Agent)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use chrono::Utc;

    use super::*;
    use crate::protocol::link::Link;

    fn make_info(uuid: Uuid, name: Option<&str>) -> Agent {
        Agent {
            id: uuid,
            host_id: Uuid::new_v4(),
            name: name.map(|s| s.to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
            agent_type: "claude".to_string(),
            structured_protocol: Some("claude_pty_v1".to_string()),
            readonly: false,
            args: vec![],
            created_at: Utc::now(),
        }
    }

    fn make_remote_info(uuid: Uuid, host_id: Uuid, name: Option<&str>, route: Route) -> Agent {
        Agent {
            id: uuid,
            host_id,
            name: name.map(|s| s.to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route,
            agent_type: "claude".to_string(),
            structured_protocol: Some("claude_pty_v1".to_string()),
            readonly: false,
            args: vec![],
            created_at: Utc::now(),
        }
    }

    fn hosts(routes: &[(Uuid, Route)]) -> HashMap<Uuid, Host> {
        routes
            .iter()
            .map(|(id, route)| {
                (
                    *id,
                    Host {
                        id: *id,
                        name: format!("host-{id}"),
                        route: route.clone(),
                        version: "0.1.0".to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn register_local_and_resolve_by_uuid() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("myagent"))).unwrap();

        let info = reg.resolve(&hosts, &id.to_string()).unwrap();
        assert_eq!(info.id, id);
        assert!(info.route.is_empty());
    }

    #[test]
    fn register_local_and_resolve_by_name() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("myagent"))).unwrap();

        let info = reg.resolve(&hosts, "myagent").unwrap();
        assert_eq!(info.id, id);
    }

    #[test]
    fn register_local_duplicate_uuid_errors() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("a"))).unwrap();
        assert!(reg.register_local(make_info(id, Some("b"))).is_err());
    }

    #[test]
    fn register_local_duplicate_name_errors() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        reg.register_local(make_info(id1, Some("taken"))).unwrap();
        assert!(reg.register_local(make_info(id2, Some("taken"))).is_err());
    }

    #[test]
    fn update_local_replaces_alias_and_cleans_up_old_mapping() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("old-name"))).unwrap();

        reg.update_local(make_info(id, Some("new-name"))).unwrap();

        assert!(reg.resolve(&hosts, "old-name").is_none());
        let info = reg.resolve(&hosts, "new-name").unwrap();
        assert_eq!(info.id, id);
        assert_eq!(info.name.as_deref(), Some("new-name"));
    }

    #[test]
    fn update_local_collision_leaves_existing_mapping_intact() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let owner = Uuid::new_v4();
        let candidate = Uuid::new_v4();
        reg.register_local(make_info(owner, Some("taken"))).unwrap();
        reg.register_local(make_info(candidate, Some("candidate")))
            .unwrap();

        let err = reg
            .update_local(make_info(candidate, Some("taken")))
            .unwrap_err();

        assert!(matches!(err, AgentRegistryError::AlreadyExists(ref name) if name == "taken"));
        assert_eq!(reg.resolve(&hosts, "taken").unwrap().id, owner);
        assert_eq!(reg.resolve(&hosts, "candidate").unwrap().id, candidate);
        assert_eq!(
            reg.get(&candidate).and_then(|info| info.name.as_deref()),
            Some("candidate")
        );
    }

    #[test]
    fn register_remote_and_resolve() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let hosts = hosts(&[(host_id, Route::from_link(Link::new("peer-a").unwrap()))]);
        reg.register_remote(make_remote_info(
            id,
            host_id,
            Some("remote1"),
            Route::from_link(Link::new("peer-a").unwrap()),
        ))
        .unwrap();

        let info = reg.resolve(&hosts, "remote1").unwrap();
        assert_eq!(info.id, id);
        assert!(info.is_remote());
    }

    #[test]
    fn register_remote_name_first_one_wins() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let host1 = Uuid::new_v4();
        let host2 = Uuid::new_v4();
        let hosts = hosts(&[
            (host1, Route::from_link(Link::new("a").unwrap())),
            (host2, Route::from_link(Link::new("b").unwrap())),
        ]);
        reg.register_remote(make_remote_info(
            id1,
            host1,
            Some("shared"),
            Route::from_link(Link::new("a").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id2,
            host2,
            Some("shared"),
            Route::from_link(Link::new("b").unwrap()),
        ))
        .unwrap();

        // Alias resolves to first one
        let info = reg.resolve(&hosts, "shared").unwrap();
        assert_eq!(info.id, id1);

        // Second is still accessible by UUID
        let info2 = reg.resolve(&hosts, &id2.to_string()).unwrap();
        assert_eq!(info2.id, id2);
    }

    #[test]
    fn reannounce_frees_old_name() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let hosts = hosts(&[(host_id, Route::from_link(Link::new("a").unwrap()))]);
        reg.register_remote(make_remote_info(
            id,
            host_id,
            Some("old-name"),
            Route::from_link(Link::new("a").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id,
            host_id,
            Some("new-name"),
            Route::from_link(Link::new("a").unwrap()),
        ))
        .unwrap();

        assert!(reg.resolve(&hosts, "old-name").is_none());
        let info = reg.resolve(&hosts, "new-name").unwrap();
        assert_eq!(info.id, id);
        assert_eq!(
            reg.get(&id).and_then(|info| info.name.as_deref()),
            Some("new-name")
        );
    }

    #[test]
    fn reannounce_updates_remote_display_name_without_stealing_alias() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let local_id = Uuid::new_v4();
        let remote_id = Uuid::new_v4();
        let remote_host_id = Uuid::new_v4();

        reg.register_local(make_info(local_id, Some("taken")))
            .unwrap();
        reg.register_remote(make_remote_info(
            remote_id,
            remote_host_id,
            Some("old-remote"),
            Route::from_link(Link::new("peer-a").unwrap()),
        ))
        .unwrap();

        reg.register_remote(make_remote_info(
            remote_id,
            remote_host_id,
            Some("taken"),
            Route::from_link(Link::new("peer-a").unwrap()),
        ))
        .unwrap();

        assert_eq!(reg.resolve(&hosts, "taken").unwrap().id, local_id);
        assert!(reg.resolve(&hosts, "old-remote").is_none());
        assert_eq!(
            reg.get(&remote_id).and_then(|info| info.name.as_deref()),
            Some("taken")
        );
    }

    #[test]
    fn remove_cleans_up_name() {
        let mut reg = AgentRegistry::new();
        let hosts = HashMap::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("cleanup"))).unwrap();

        reg.remove(&id);
        assert!(!reg.contains(&id));
        assert!(!reg.name_taken("cleanup"));
        assert!(reg.resolve(&hosts, "cleanup").is_none());
    }

    #[test]
    fn remove_where_bulk_removes() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let host1 = Uuid::new_v4();
        let host2 = Uuid::new_v4();
        let host3 = Uuid::new_v4();
        let hosts = hosts(&[
            (host1, Route::from_link(Link::new("dead").unwrap())),
            (host2, Route::from_link(Link::new("dead").unwrap())),
            (host3, Route::from_link(Link::new("alive").unwrap())),
        ]);
        reg.register_remote(make_remote_info(
            id1,
            host1,
            Some("a1"),
            Route::from_link(Link::new("dead").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id2,
            host2,
            Some("a2"),
            Route::from_link(Link::new("dead").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id3,
            host3,
            Some("a3"),
            Route::from_link(Link::new("alive").unwrap()),
        ))
        .unwrap();

        let prefix = Route::from_link(Link::new("dead").unwrap());
        let removed = reg.remove_where(
            |hid| hosts.get(&hid).map(|h| h.route.clone()),
            |r| r.starts_with_route(&prefix),
        );
        assert_eq!(removed.len(), 2);
        assert!(!reg.contains(&id1));
        assert!(!reg.contains(&id2));
        assert!(reg.contains(&id3));
    }

    #[test]
    fn resolve_with_route_prefix() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let hosts = hosts(&[(host_id, Route::from_link(Link::new("host-b").unwrap()))]);
        reg.register_remote(make_remote_info(
            id,
            host_id,
            Some("myagent"),
            Route::from_link(Link::new("host-b").unwrap()),
        ))
        .unwrap();

        // "host-b:myagent" should resolve when route matches
        let info = reg.resolve(&hosts, "host-b:myagent").unwrap();
        assert_eq!(info.id, id);
        assert_eq!(info.route.peek(), Some(&Link::new("host-b").unwrap()));
    }

    #[test]
    fn resolve_with_route_prefix_mismatch() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let hosts = hosts(&[(host_id, Route::from_link(Link::new("host-a").unwrap()))]);
        reg.register_remote(make_remote_info(
            id,
            host_id,
            Some("myagent"),
            Route::from_link(Link::new("host-a").unwrap()),
        ))
        .unwrap();

        // "host-b:myagent" should not resolve when route doesn't match
        assert!(reg.resolve(&hosts, "host-b:myagent").is_none());
    }

    #[test]
    fn resolve_with_multi_hop_route() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let mut route = Route::from_link(Link::new("host-b").unwrap());
        route.push(Link::new("host-a").unwrap());
        let hosts = hosts(&[(host_id, route.clone())]);
        reg.register_remote(make_remote_info(id, host_id, Some("agent1"), route))
            .unwrap();

        let info = reg.resolve(&hosts, "host-a.host-b:agent1").unwrap();
        assert_eq!(info.id, id);
        let mut route = info.route;
        assert_eq!(route.pop(), Some(Link::new("host-a").unwrap()));
        assert_eq!(route.pop(), Some(Link::new("host-b").unwrap()));
    }

    #[test]
    fn resolve_nonexistent_returns_none() {
        let reg = AgentRegistry::new();
        assert!(reg.resolve(&HashMap::new(), "doesntexist").is_none());
    }

    #[test]
    fn list_all_includes_route() {
        let mut reg = AgentRegistry::new();
        let local_id = Uuid::new_v4();
        let remote_id = Uuid::new_v4();
        let remote_host_id = Uuid::new_v4();
        let hosts = hosts(&[(remote_host_id, Route::from_link(Link::new("peer").unwrap()))]);
        reg.register_local(make_info(local_id, Some("local")))
            .unwrap();
        reg.register_remote(make_remote_info(
            remote_id,
            remote_host_id,
            Some("remote"),
            Route::from_link(Link::new("peer").unwrap()),
        ))
        .unwrap();

        let all = reg.list_all(&hosts);
        assert_eq!(all.len(), 2);

        let local = all.iter().find(|a| a.id == local_id).unwrap();
        assert!(local.route.is_empty());

        let remote = all.iter().find(|a| a.id == remote_id).unwrap();
        assert!(remote.is_remote());
    }

    #[test]
    fn count_remote() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        reg.register_local(make_info(id1, None)).unwrap();
        reg.register_remote(make_remote_info(
            id2,
            host_id,
            None,
            Route::from_link(Link::new("a").unwrap()),
        ))
        .unwrap();
        assert_eq!(reg.count_remote(), 1);
    }

    #[test]
    fn contains_and_name_taken() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        assert!(!reg.contains(&id));
        assert!(!reg.name_taken("test"));

        reg.register_local(make_info(id, Some("test"))).unwrap();
        assert!(reg.contains(&id));
        assert!(reg.name_taken("test"));
    }

    #[test]
    fn remove_where_single_hop() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let host1 = Uuid::new_v4();
        let host2 = Uuid::new_v4();
        let host3 = Uuid::new_v4();
        let hosts = hosts(&[
            (host1, Route::from_link(Link::new("host-a").unwrap())),
            (host2, Route::from_link(Link::new("host-a").unwrap())),
            (host3, Route::from_link(Link::new("host-b").unwrap())),
        ]);
        reg.register_remote(make_remote_info(
            id1,
            host1,
            Some("a1"),
            Route::from_link(Link::new("host-a").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id2,
            host2,
            Some("a2"),
            Route::from_link(Link::new("host-a").unwrap()),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id3,
            host3,
            Some("a3"),
            Route::from_link(Link::new("host-b").unwrap()),
        ))
        .unwrap();

        let prefix = Route::from_link(Link::new("host-a").unwrap());
        let removed = reg.remove_where(
            |hid| hosts.get(&hid).map(|h| h.route.clone()),
            |r| r.starts_with_route(&prefix),
        );
        assert_eq!(removed.len(), 2);
        assert!(!reg.contains(&id1));
        assert!(!reg.contains(&id2));
        assert!(reg.contains(&id3));
        assert!(!reg.name_taken("a1"));
        assert!(!reg.name_taken("a2"));
    }

    #[test]
    fn remove_where_multi_hop() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let host1 = Uuid::new_v4();
        let host2 = Uuid::new_v4();

        let mut route1 = Route::from_link(Link::new("host-b").unwrap());
        route1.push(Link::new("host-a").unwrap());
        let hosts = hosts(&[
            (host1, route1.clone()),
            (host2, Route::from_link(Link::new("host-a").unwrap())),
        ]);
        reg.register_remote(make_remote_info(id1, host1, Some("deep"), route1))
            .unwrap();

        reg.register_remote(make_remote_info(
            id2,
            host2,
            Some("shallow"),
            Route::from_link(Link::new("host-a").unwrap()),
        ))
        .unwrap();

        // Prefix "host-a" matches both (route starts with host-a)
        let prefix = Route::from_link(Link::new("host-a").unwrap());
        let removed = reg.remove_where(
            |hid| hosts.get(&hid).map(|h| h.route.clone()),
            |r| r.starts_with_route(&prefix),
        );
        assert_eq!(removed.len(), 2);
        assert!(!reg.contains(&id1));
        assert!(!reg.contains(&id2));
    }

    #[test]
    fn remove_where_skips_local() {
        let mut reg = AgentRegistry::new();
        let local_id = Uuid::new_v4();
        let remote_id = Uuid::new_v4();
        let remote_host_id = Uuid::new_v4();
        let hosts = hosts(&[(
            remote_host_id,
            Route::from_link(Link::new("host-a").unwrap()),
        )]);
        reg.register_local(make_info(local_id, Some("local")))
            .unwrap();
        reg.register_remote(make_remote_info(
            remote_id,
            remote_host_id,
            Some("remote"),
            Route::from_link(Link::new("host-a").unwrap()),
        ))
        .unwrap();

        let prefix = Route::empty();
        let removed = reg.remove_where(
            |hid| hosts.get(&hid).map(|h| h.route.clone()),
            |r| r.starts_with_route(&prefix),
        );
        // Empty prefix matches all remote agents but not local ones
        assert_eq!(removed.len(), 1);
        assert!(reg.contains(&local_id));
        assert!(!reg.contains(&remote_id));
    }
}
