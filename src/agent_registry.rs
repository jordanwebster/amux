use crate::route::Route;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// Information about a running agent
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub agent_id: Uuid,
    pub alias: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub route: Route,
}

impl AgentInfo {
    pub fn is_remote(&self) -> bool {
        self.route.peek().is_some()
    }
}

/// Centralized agent tracking with bidirectional alias<->UUID mapping.
/// Tracks both local and remote agents with route information.
pub(crate) struct AgentRegistry {
    alias_to_uuid: HashMap<String, Uuid>,
    uuid_to_alias: HashMap<Uuid, String>,
    entries: HashMap<Uuid, AgentInfo>,
}

#[derive(Debug, Error)]
pub enum AgentRegistryError {
    #[error("Agent must have empty route")]
    AgentNotLocal,
    #[error("Agent must not have empty route")]
    AgentNotRemote,
    #[error("Agent already exists: {0}")]
    AgentAlreadyExists(String),
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            alias_to_uuid: HashMap::new(),
            uuid_to_alias: HashMap::new(),
            entries: HashMap::new(),
        }
    }

    /// Register a local agent. Errors if UUID exists or alias is taken.
    pub fn register_local(&mut self, info: AgentInfo) -> Result<(), AgentRegistryError> {
        if !info.route.is_empty() {
            return Err(AgentRegistryError::AgentNotLocal);
        }
        if self.entries.contains_key(&info.agent_id) {
            return Err(AgentRegistryError::AgentAlreadyExists(
                info.agent_id.to_string(),
            ));
        }
        if let Some(ref alias) = info.alias {
            if self.alias_to_uuid.contains_key(alias) {
                return Err(AgentRegistryError::AgentAlreadyExists(alias.clone()));
            }
            self.alias_to_uuid.insert(alias.clone(), info.agent_id);
            self.uuid_to_alias.insert(info.agent_id, alias.clone());
        }
        self.entries.insert(info.agent_id, info);
        Ok(())
    }

    /// Register a remote agent. Upserts by UUID; clears old alias on re-announce.
    /// First-one-wins for alias (silently skips if taken by another agent).
    pub fn register_remote(&mut self, info: AgentInfo) -> Result<(), AgentRegistryError> {
        if info.route.is_empty() {
            return Err(AgentRegistryError::AgentNotRemote);
        }
        // On re-announce (same UUID), clear the old alias mapping
        if let Some(old_alias) = self.uuid_to_alias.remove(&info.agent_id) {
            // Only remove from alias_to_uuid if it still points to this UUID
            if self.alias_to_uuid.get(&old_alias) == Some(&info.agent_id) {
                self.alias_to_uuid.remove(&old_alias);
            }
        }

        // Try to claim the new alias (first-one-wins)
        if let Some(ref alias) = info.alias {
            if !self.alias_to_uuid.contains_key(alias) {
                self.alias_to_uuid.insert(alias.clone(), info.agent_id);
                self.uuid_to_alias.insert(info.agent_id, alias.clone());
            }
            // else: silently skip — alias is taken by another agent
        }

        self.entries.insert(info.agent_id, info);
        Ok(())
    }

    /// Remove an agent by UUID. Returns the removed entry if found.
    pub fn remove(&mut self, uuid: &Uuid) -> Option<AgentInfo> {
        let info = self.entries.remove(uuid)?;
        if let Some(alias) = self.uuid_to_alias.remove(uuid) {
            if self.alias_to_uuid.get(&alias) == Some(uuid) {
                self.alias_to_uuid.remove(&alias);
            }
        }
        Some(info)
    }

    /// Remove all remote agents learned from a given link. Returns removed UUIDs.
    pub fn remove_for_link(&mut self, dead_link: &str) -> Vec<Uuid> {
        let removed: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.route.peek(), Some(link) if link == dead_link))
            .map(|(id, _)| *id)
            .collect();
        for id in &removed {
            self.remove(id);
        }
        removed
    }

    /// Resolve an identifier to an AgentInfo.
    ///
    /// Resolution order:
    /// 1. "route:id" — parse route, resolve id recursively, return with explicit route
    /// 2. UUID — look up directly
    /// 3. Alias — look up via alias_to_uuid
    pub fn resolve(&self, identifier: &str) -> Option<AgentInfo> {
        // Try splitting on last ':' for route:id format
        match identifier.rsplit_once(':') {
            Some((route_str, id)) => {
                let deserializer =
                    serde::de::value::StrDeserializer::<serde::de::value::Error>::new(route_str);
                let supplied_route: Route =
                    Route::deserialize(deserializer).expect("Route deserialization cannot fail");
                match self.resolve_inner(id) {
                    // If a fully qualified route is supplied, it must match the one that exists
                    // in the routing table.
                    Some(info) if info.route == supplied_route => Some(info),
                    _ => None,
                }
            }
            None => self.resolve_inner(identifier),
        }
    }

    /// Resolve by UUID or alias (no route: prefix handling)
    fn resolve_inner(&self, identifier: &str) -> Option<AgentInfo> {
        let uuid = match Uuid::parse_str(identifier) {
            Ok(uuid) => uuid,
            Err(_) => self.alias_to_uuid.get(identifier)?.clone(),
        };

        self.entries.get(&uuid).cloned()
    }

    /// Get an entry by UUID
    pub fn get(&self, uuid: &Uuid) -> Option<&AgentInfo> {
        self.entries.get(uuid)
    }

    /// Check if a UUID is registered
    pub fn contains(&self, uuid: &Uuid) -> bool {
        self.entries.contains_key(uuid)
    }

    /// Check if an alias is taken
    pub fn alias_taken(&self, alias: &str) -> bool {
        self.alias_to_uuid.contains_key(alias)
    }

    /// List all agents with route populated
    pub fn list_all(&self) -> Vec<AgentInfo> {
        self.entries.values().cloned().collect()
    }

    /// Count of remote agents
    pub fn count_remote(&self) -> usize {
        self.entries.values().filter(|e| e.is_remote()).count()
    }

    /// Iterate over all entries (for send_initial_announcements)
    pub fn iter_entries(&self) -> impl Iterator<Item = (&Uuid, &AgentInfo)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_info(uuid: Uuid, alias: Option<&str>) -> AgentInfo {
        AgentInfo {
            agent_id: uuid,
            alias: alias.map(|s| s.to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
        }
    }

    fn make_remote_info(uuid: Uuid, alias: Option<&str>, route: Route) -> AgentInfo {
        AgentInfo {
            agent_id: uuid,
            alias: alias.map(|s| s.to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route,
        }
    }

    #[test]
    fn register_local_and_resolve_by_uuid() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("myagent"))).unwrap();

        let info = reg.resolve(&id.to_string()).unwrap();
        assert_eq!(info.agent_id, id);
        assert!(info.route.is_empty());
    }

    #[test]
    fn register_local_and_resolve_by_alias() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("myagent"))).unwrap();

        let info = reg.resolve("myagent").unwrap();
        assert_eq!(info.agent_id, id);
    }

    #[test]
    fn register_local_duplicate_uuid_errors() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("a"))).unwrap();
        assert!(reg.register_local(make_info(id, Some("b"))).is_err());
    }

    #[test]
    fn register_local_duplicate_alias_errors() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        reg.register_local(make_info(id1, Some("taken"))).unwrap();
        assert!(reg.register_local(make_info(id2, Some("taken"))).is_err());
    }

    #[test]
    fn register_remote_and_resolve() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_remote(make_remote_info(
            id,
            Some("remote1"),
            Route::from_link("peer-a"),
        ))
        .unwrap();

        let info = reg.resolve("remote1").unwrap();
        assert_eq!(info.agent_id, id);
        assert!(info.is_remote());
    }

    #[test]
    fn register_remote_alias_first_one_wins() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        reg.register_remote(make_remote_info(id1, Some("shared"), Route::from_link("a")))
            .unwrap();
        reg.register_remote(make_remote_info(id2, Some("shared"), Route::from_link("b")))
            .unwrap();

        // Alias resolves to first one
        let info = reg.resolve("shared").unwrap();
        assert_eq!(info.agent_id, id1);

        // Second is still accessible by UUID
        let info2 = reg.resolve(&id2.to_string()).unwrap();
        assert_eq!(info2.agent_id, id2);
    }

    #[test]
    fn reannounce_frees_old_alias() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_remote(make_remote_info(
            id,
            Some("old-name"),
            Route::from_link("a"),
        ))
        .unwrap();
        reg.register_remote(make_remote_info(
            id,
            Some("new-name"),
            Route::from_link("a"),
        ))
        .unwrap();

        assert!(reg.resolve("old-name").is_none());
        let info = reg.resolve("new-name").unwrap();
        assert_eq!(info.agent_id, id);
    }

    #[test]
    fn remove_cleans_up_alias() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_local(make_info(id, Some("cleanup"))).unwrap();

        reg.remove(&id);
        assert!(!reg.contains(&id));
        assert!(!reg.alias_taken("cleanup"));
        assert!(reg.resolve("cleanup").is_none());
    }

    #[test]
    fn remove_for_link_bulk_removes() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        reg.register_remote(make_remote_info(id1, Some("a1"), Route::from_link("dead")))
            .unwrap();
        reg.register_remote(make_remote_info(id2, Some("a2"), Route::from_link("dead")))
            .unwrap();
        reg.register_remote(make_remote_info(id3, Some("a3"), Route::from_link("alive")))
            .unwrap();

        let removed = reg.remove_for_link("dead");
        assert_eq!(removed.len(), 2);
        assert!(!reg.contains(&id1));
        assert!(!reg.contains(&id2));
        assert!(reg.contains(&id3));
    }

    #[test]
    fn resolve_with_route_prefix() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_remote(make_remote_info(
            id,
            Some("myagent"),
            Route::from_link("host-b"),
        ))
        .unwrap();

        // "host-b:myagent" should resolve when route matches
        let info = reg.resolve("host-b:myagent").unwrap();
        assert_eq!(info.agent_id, id);
        assert_eq!(info.route.peek(), Some("host-b"));
    }

    #[test]
    fn resolve_with_route_prefix_mismatch() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        reg.register_remote(make_remote_info(
            id,
            Some("myagent"),
            Route::from_link("host-a"),
        ))
        .unwrap();

        // "host-b:myagent" should not resolve when route doesn't match
        assert!(reg.resolve("host-b:myagent").is_none());
    }

    #[test]
    fn resolve_with_multi_hop_route() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        let mut route = Route::from_link("host-b");
        route.push("host-a");
        reg.register_remote(make_remote_info(id, Some("agent1"), route))
            .unwrap();

        let info = reg.resolve("host-a.host-b:agent1").unwrap();
        assert_eq!(info.agent_id, id);
        let mut route = info.route;
        assert_eq!(route.pop(), Some("host-a".to_string()));
        assert_eq!(route.pop(), Some("host-b".to_string()));
    }

    #[test]
    fn resolve_nonexistent_returns_none() {
        let reg = AgentRegistry::new();
        assert!(reg.resolve("doesntexist").is_none());
    }

    #[test]
    fn list_all_includes_route() {
        let mut reg = AgentRegistry::new();
        let local_id = Uuid::new_v4();
        let remote_id = Uuid::new_v4();
        reg.register_local(make_info(local_id, Some("local")))
            .unwrap();
        reg.register_remote(make_remote_info(
            remote_id,
            Some("remote"),
            Route::from_link("peer"),
        ))
        .unwrap();

        let all = reg.list_all();
        assert_eq!(all.len(), 2);

        let local = all.iter().find(|a| a.agent_id == local_id).unwrap();
        assert!(local.route.is_empty());

        let remote = all.iter().find(|a| a.agent_id == remote_id).unwrap();
        assert!(remote.is_remote());
    }

    #[test]
    fn count_remote() {
        let mut reg = AgentRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        reg.register_local(make_info(id1, None)).unwrap();
        reg.register_remote(make_remote_info(id2, None, Route::from_link("a")))
            .unwrap();
        assert_eq!(reg.count_remote(), 1);
    }

    #[test]
    fn contains_and_alias_taken() {
        let mut reg = AgentRegistry::new();
        let id = Uuid::new_v4();
        assert!(!reg.contains(&id));
        assert!(!reg.alias_taken("test"));

        reg.register_local(make_info(id, Some("test"))).unwrap();
        assert!(reg.contains(&id));
        assert!(reg.alias_taken("test"));
    }
}
