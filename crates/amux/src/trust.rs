use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::HostId;
use crate::identity::{
    ED25519_PUBKEY_LEN, IdentityError, atomic_replace_private, create_private_file,
    ensure_private_file_mode, validate_len,
};

const TRUST_FILE: &str = "trust.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Reachability {
    Cloud,
    Ssh { target: String },
    DirectTcp { addr: SocketAddr },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrustEntry {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) name: String,
    pub(crate) paired_at: DateTime<Utc>,
    pub(crate) reachabilities: Vec<Reachability>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TrustStore {
    entries: BTreeMap<HostId, TrustEntry>,
    replacement_pending: BTreeSet<HostId>,
}

pub(crate) type SharedTrustStore = std::sync::Arc<std::sync::RwLock<TrustStore>>;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrustStorePairingUpdate {
    Inserted,
    Updated,
    PubkeyReplacementRequired,
    ReplacedPubkey,
}

impl TrustStore {
    pub(crate) fn load_in(data_dir: &Path) -> Result<Self, IdentityError> {
        load_trust_store_from_path(&trust_path(data_dir))
    }

    pub(crate) fn load_or_create_in(data_dir: &Path) -> Result<Self, IdentityError> {
        let path = trust_path(data_dir);
        match load_trust_store_from_path(&path) {
            Ok(store) => Ok(store),
            Err(IdentityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let store = Self::default();
                match create_private_file(&path, &serde_json::to_vec_pretty(&store)?) {
                    Ok(()) => Ok(store),
                    Err(IdentityError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        load_trust_store_from_path(&path)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn save_in(&self, data_dir: &Path) -> Result<(), IdentityError> {
        self.save_to_path(&trust_path(data_dir))
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) fn entry(&self, host_id: HostId) -> Option<&TrustEntry> {
        self.entries.get(&host_id)
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (HostId, &TrustEntry)> + '_ {
        self.entries
            .iter()
            .map(|(host_id, entry)| (*host_id, entry))
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) fn insert_for_test(&mut self, host_id: HostId, entry: TrustEntry) {
        self.entries.insert(host_id, entry);
    }

    pub(crate) fn host_id_for_pubkey(&self, pubkey: &[u8]) -> Option<HostId> {
        self.entries
            .iter()
            .filter(|(host_id, _)| !self.replacement_pending.contains(host_id))
            .find_map(|(host_id, entry)| (entry.pubkey == pubkey).then_some(*host_id))
    }

    pub(crate) fn pubkey_for_host(&self, host_id: HostId) -> Option<&[u8]> {
        if self.replacement_pending.contains(&host_id) {
            return None;
        }
        self.entries
            .get(&host_id)
            .map(|entry| entry.pubkey.as_slice())
    }

    pub(crate) fn upsert_paired_peer(
        &mut self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<TrustStorePairingUpdate, IdentityError> {
        let reachability = reachability.into();
        validate_len("TrustEntry.pubkey", &pubkey, ED25519_PUBKEY_LEN)?;
        if let Some(existing_host_id) = self.other_host_for_pubkey(host_id, &pubkey) {
            return Err(IdentityError::DuplicatePubkey(existing_host_id));
        }
        if self.replacement_pending.contains(&host_id) {
            return Ok(TrustStorePairingUpdate::PubkeyReplacementRequired);
        }
        match self.entries.get_mut(&host_id) {
            Some(entry) if entry.pubkey == pubkey => {
                entry.name = name;
                entry.paired_at = paired_at;
                if let Some(reachability) = reachability {
                    append_reachability(&mut entry.reachabilities, reachability);
                }
                Ok(TrustStorePairingUpdate::Updated)
            }
            Some(_) => {
                self.replacement_pending.insert(host_id);
                Ok(TrustStorePairingUpdate::PubkeyReplacementRequired)
            }
            None => {
                self.entries.insert(
                    host_id,
                    TrustEntry {
                        pubkey,
                        name,
                        paired_at,
                        reachabilities: reachability.into_iter().collect(),
                    },
                );
                Ok(TrustStorePairingUpdate::Inserted)
            }
        }
    }

    pub(crate) fn replace_paired_peer_after_teardown(
        &mut self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<TrustStorePairingUpdate, IdentityError> {
        let reachability = reachability.into();
        validate_len("TrustEntry.pubkey", &pubkey, ED25519_PUBKEY_LEN)?;
        if let Some(existing_host_id) = self.other_host_for_pubkey(host_id, &pubkey) {
            return Err(IdentityError::DuplicatePubkey(existing_host_id));
        }
        match self.entries.get_mut(&host_id) {
            Some(entry) if entry.pubkey == pubkey => {
                self.replacement_pending.remove(&host_id);
                entry.name = name;
                entry.paired_at = paired_at;
                if let Some(reachability) = reachability {
                    append_reachability(&mut entry.reachabilities, reachability);
                }
                Ok(TrustStorePairingUpdate::Updated)
            }
            Some(entry) => {
                self.replacement_pending.remove(&host_id);
                entry.pubkey = pubkey;
                entry.name = name;
                entry.paired_at = paired_at;
                entry.reachabilities = reachability.into_iter().collect();
                Ok(TrustStorePairingUpdate::ReplacedPubkey)
            }
            None => {
                self.replacement_pending.remove(&host_id);
                self.entries.insert(
                    host_id,
                    TrustEntry {
                        pubkey,
                        name,
                        paired_at,
                        reachabilities: reachability.into_iter().collect(),
                    },
                );
                Ok(TrustStorePairingUpdate::Inserted)
            }
        }
    }

    pub(crate) fn remove(&mut self, host_id: HostId) -> Option<TrustEntry> {
        self.replacement_pending.remove(&host_id);
        self.entries.remove(&host_id)
    }

    fn other_host_for_pubkey(&self, host_id: HostId, pubkey: &[u8]) -> Option<HostId> {
        self.entries.iter().find_map(|(candidate_host_id, entry)| {
            (*candidate_host_id != host_id && entry.pubkey == pubkey).then_some(*candidate_host_id)
        })
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn save_to_path(&self, path: &Path) -> Result<(), IdentityError> {
        self.validate()?;
        let json = serde_json::to_vec_pretty(self)?;
        atomic_replace_private(path, &json)
    }

    fn validate(&self) -> Result<(), IdentityError> {
        for entry in self.entries.values() {
            validate_len("TrustEntry.pubkey", &entry.pubkey, ED25519_PUBKEY_LEN)?;
        }
        Ok(())
    }
}

impl Serialize for TrustStore {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrustStore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self {
            entries: BTreeMap::deserialize(deserializer)?,
            replacement_pending: BTreeSet::new(),
        })
    }
}

fn load_trust_store_from_path(path: &Path) -> Result<TrustStore, IdentityError> {
    ensure_private_file_mode(path)?;
    let bytes = fs::read(path)?;
    let store = serde_json::from_slice::<TrustStore>(&bytes)?;
    store.validate()?;
    Ok(store)
}

fn append_reachability(reachabilities: &mut Vec<Reachability>, reachability: Reachability) {
    if !reachabilities.contains(&reachability) {
        reachabilities.push(reachability);
    }
}

fn trust_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TRUST_FILE)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::str::FromStr;

    use super::*;

    fn temp_data_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn trust_store_is_created_as_empty_json_and_reloaded() {
        let dir = temp_data_dir();

        let store = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert!(store.is_empty());
        assert!(trust_path(dir.path()).exists());

        let loaded = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert!(loaded.is_empty());

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(trust_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn trust_store_can_hold_multiple_reachabilities() {
        let peer = HostId::from_u128(2);
        let mut store = TrustStore::default();
        store.entries.insert(
            peer,
            TrustEntry {
                pubkey: vec![8; 32],
                name: "second".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![
                    Reachability::DirectTcp {
                        addr: SocketAddr::from_str("127.0.0.1:9000").unwrap(),
                    },
                    Reachability::Cloud,
                ],
            },
        );

        let entry = store.entry(peer).unwrap();
        assert_eq!(entry.pubkey, vec![8; 32]);
        assert_eq!(entry.name, "second");
        assert_eq!(entry.reachabilities.len(), 2);
    }

    #[test]
    fn trust_store_pairing_upsert_inserts_and_deduplicates_reachability() {
        let peer = HostId::from_u128(2);
        let mut store = TrustStore::default();
        let paired_at = DateTime::<Utc>::from_timestamp(200, 0).unwrap();

        assert_eq!(
            store
                .upsert_paired_peer(
                    peer,
                    vec![9; 32],
                    "first".to_string(),
                    Reachability::Cloud,
                    paired_at,
                )
                .unwrap(),
            TrustStorePairingUpdate::Inserted
        );
        assert_eq!(
            store
                .upsert_paired_peer(
                    peer,
                    vec![9; 32],
                    "renamed".to_string(),
                    Reachability::Cloud,
                    DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
                )
                .unwrap(),
            TrustStorePairingUpdate::Updated
        );
        assert_eq!(
            store
                .upsert_paired_peer(
                    peer,
                    vec![9; 32],
                    "renamed".to_string(),
                    Reachability::Ssh {
                        target: "workstation".to_string(),
                    },
                    DateTime::<Utc>::from_timestamp(400, 0).unwrap(),
                )
                .unwrap(),
            TrustStorePairingUpdate::Updated
        );

        let entry = store.entry(peer).unwrap();
        assert_eq!(entry.name, "renamed");
        assert_eq!(entry.pubkey, vec![9; 32]);
        assert_eq!(entry.reachabilities.len(), 2);
        assert!(entry.reachabilities.contains(&Reachability::Cloud));
        assert!(entry.reachabilities.contains(&Reachability::Ssh {
            target: "workstation".to_string(),
        }));
    }

    #[test]
    fn trust_store_pairing_rejects_duplicate_pubkey_for_different_host() {
        let first = HostId::from_u128(2);
        let second = HostId::from_u128(3);
        let mut store = TrustStore::default();
        store
            .upsert_paired_peer(
                first,
                vec![9; 32],
                "first".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
            )
            .unwrap();

        assert!(matches!(
            store
                .upsert_paired_peer(
                    second,
                    vec![9; 32],
                    "second".to_string(),
                    Reachability::Cloud,
                    DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
                )
                .unwrap_err(),
            IdentityError::DuplicatePubkey(host_id) if host_id == first
        ));
        assert!(store.entry(second).is_none());
    }

    #[test]
    fn trust_store_pairing_upsert_requires_teardown_before_pubkey_replacement() {
        let peer = HostId::from_u128(2);
        let mut store = TrustStore::default();
        store
            .upsert_paired_peer(
                peer,
                vec![7; 32],
                "old".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
            )
            .unwrap();

        assert_eq!(
            store
                .upsert_paired_peer(
                    peer,
                    vec![8; 32],
                    "new".to_string(),
                    Reachability::DirectTcp {
                        addr: SocketAddr::from_str("127.0.0.1:9000").unwrap(),
                    },
                    DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
                )
                .unwrap(),
            TrustStorePairingUpdate::PubkeyReplacementRequired
        );

        let entry = store.entry(peer).unwrap();
        assert_eq!(entry.name, "old");
        assert_eq!(entry.pubkey, vec![7; 32]);
        assert_eq!(entry.reachabilities, vec![Reachability::Cloud]);
        assert!(store.pubkey_for_host(peer).is_none());
        assert!(store.host_id_for_pubkey(&[7; 32]).is_none());

        assert_eq!(
            store
                .replace_paired_peer_after_teardown(
                    peer,
                    vec![8; 32],
                    "new".to_string(),
                    Reachability::DirectTcp {
                        addr: SocketAddr::from_str("127.0.0.1:9000").unwrap(),
                    },
                    DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
                )
                .unwrap(),
            TrustStorePairingUpdate::ReplacedPubkey
        );

        let entry = store.entry(peer).unwrap();
        assert_eq!(entry.name, "new");
        assert_eq!(entry.pubkey, vec![8; 32]);
        assert_eq!(
            entry.reachabilities,
            vec![Reachability::DirectTcp {
                addr: SocketAddr::from_str("127.0.0.1:9000").unwrap(),
            }]
        );
    }

    #[test]
    fn remove_deletes_entry_and_persists() {
        let dir = temp_data_dir();
        let peer = HostId::from_u128(2);
        let other = HostId::from_u128(3);
        let mut store = TrustStore::default();
        store
            .upsert_paired_peer(
                peer,
                vec![7; 32],
                "peer".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
            )
            .unwrap();
        store
            .upsert_paired_peer(
                other,
                vec![8; 32],
                "other".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
            )
            .unwrap();

        let removed = store.remove(peer).unwrap();
        assert_eq!(removed.name, "peer");
        assert!(store.entry(peer).is_none());
        assert!(store.entry(other).is_some());

        store.save_in(dir.path()).unwrap();
        let loaded = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert!(loaded.entry(peer).is_none());
        assert_eq!(loaded.entry(other).unwrap().name, "other");
    }

    #[test]
    fn trust_store_persists_reachabilities_as_json() {
        let dir = temp_data_dir();
        let peer = HostId::from_u128(2);
        let mut store = TrustStore::default();
        store.entries.insert(
            peer,
            TrustEntry {
                pubkey: vec![9; 32],
                name: "peer".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Ssh {
                    target: "workstation".to_string(),
                }],
            },
        );

        store.save_in(dir.path()).unwrap();
        let loaded = TrustStore::load_or_create_in(dir.path()).unwrap();

        assert_eq!(loaded.entry(peer).unwrap().name, "peer");
        assert!(
            fs::read_to_string(trust_path(dir.path()))
                .unwrap()
                .contains("workstation")
        );
    }

    #[test]
    fn trust_store_rejects_wrong_length_pubkey_on_load_and_save() {
        let dir = temp_data_dir();
        let peer = HostId::from_u128(2);
        let mut store = TrustStore::default();
        store.entries.insert(
            peer,
            TrustEntry {
                pubkey: vec![1, 2, 3],
                name: "peer".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );

        assert!(matches!(
            store.save_in(dir.path()),
            Err(IdentityError::InvalidLength {
                field: "TrustEntry.pubkey",
                expected: ED25519_PUBKEY_LEN,
                actual: 3
            })
        ));

        let json = format!(
            r#"{{
  "{peer}": {{
    "pubkey": [1, 2, 3],
    "name": "peer",
    "paired_at": "1970-01-01T00:00:00Z",
    "reachabilities": [{{ "type": "cloud" }}]
  }}
}}"#
        );
        create_private_file(&trust_path(dir.path()), json.as_bytes()).unwrap();

        assert!(matches!(
            TrustStore::load_or_create_in(dir.path()),
            Err(IdentityError::InvalidLength {
                field: "TrustEntry.pubkey",
                expected: ED25519_PUBKEY_LEN,
                actual: 3
            })
        ));
    }
}
