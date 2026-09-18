//! Store-free reference materialiser for transcript mutations.

use std::collections::{BTreeMap, BTreeSet};

use crate::algebra::{coalesce, resolve};
use crate::{
    Changes, ChatRevision, DESKTOP_ENTRY_MAX_BYTES, Entry, EntryKey, MergeDefect, Mutation,
    Placement, Promotion, Revision, SegmentId, Stored,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Redirect {
    to: EntryKey,
    revision: Revision,
    promote: Option<Promotion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectState {
    pub from: EntryKey,
    pub to: EntryKey,
    pub revision: Revision,
    pub promote: Option<Promotion>,
}

/// Reference implementation of the mutation algebra used by tests, the
/// reducer window and the SQLite store.
#[derive(Clone, Debug)]
pub struct MutationOracle<E: Entry> {
    segment: SegmentId,
    entry_budget: usize,
    entries: BTreeMap<EntryKey, Stored<E>>,
    tombstones: BTreeMap<EntryKey, Revision>,
    redirects: BTreeMap<EntryKey, Redirect>,
}

impl<E: Entry> Default for MutationOracle<E> {
    fn default() -> Self {
        Self::new(1, DESKTOP_ENTRY_MAX_BYTES)
    }
}

impl<E: Entry> MutationOracle<E> {
    pub fn new(segment: SegmentId, entry_budget: usize) -> Self {
        Self {
            segment,
            entry_budget,
            entries: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            redirects: BTreeMap::new(),
        }
    }

    /// Restore the canonical materialiser from persisted rows.
    pub fn from_state(
        segment: SegmentId,
        entry_budget: usize,
        entries: Vec<Stored<E>>,
        tombstones: Vec<(EntryKey, Revision)>,
        redirects: Vec<RedirectState>,
    ) -> Result<Self, MergeDefect> {
        let entries = entries
            .into_iter()
            .map(|entry| (entry.key.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let tombstones = tombstones.into_iter().collect::<BTreeMap<_, _>>();
        let redirects = redirects
            .into_iter()
            .map(|redirect| {
                (
                    redirect.from,
                    Redirect {
                        to: redirect.to,
                        revision: redirect.revision,
                        promote: redirect.promote,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let oracle = Self {
            segment,
            entry_budget,
            entries,
            tombstones,
            redirects,
        };
        for key in oracle.redirects.keys() {
            oracle.resolve_key(key)?;
        }
        Ok(oracle)
    }

    /// Apply one atomic ordered group. Any defect restores the prior state.
    pub fn apply(&mut self, group: &[Mutation<E>]) -> Result<Vec<Placement>, MergeDefect> {
        let before = self.clone();
        let result = self.apply_inner(group);
        if result.is_err() {
            *self = before;
        }
        result
    }

    /// Coalesce and atomically apply a provider batch.
    pub fn apply_changes(&mut self, changes: &Changes<E>) -> Result<Vec<Placement>, MergeDefect> {
        let before = self.clone();
        let redirects = self.redirects();
        let result = (|| {
            let groups = coalesce(&changes.mutations, &redirects)?;
            let mut placed = Vec::new();
            for group in groups {
                placed.extend(self.apply_inner(&group.mutations)?);
            }
            placed.sort_by(|left, right| {
                (left.segment, left.order, &left.key).cmp(&(right.segment, right.order, &right.key))
            });
            placed.dedup_by(|left, right| left.key == right.key);
            Ok(placed)
        })();
        if result.is_err() {
            *self = before;
        }
        result
    }

    pub fn entries(&self) -> Vec<Stored<E>> {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            (left.segment, left.order, &left.key).cmp(&(right.segment, right.order, &right.key))
        });
        entries
    }

    pub fn redirects(&self) -> Vec<(EntryKey, EntryKey)> {
        self.redirects
            .iter()
            .map(|(from, redirect)| (from.clone(), redirect.to.clone()))
            .collect()
    }

    pub fn redirect_states(&self) -> Vec<RedirectState> {
        self.redirects
            .iter()
            .map(|(from, redirect)| RedirectState {
                from: from.clone(),
                to: redirect.to.clone(),
                revision: redirect.revision,
                promote: redirect.promote,
            })
            .collect()
    }

    pub fn tombstones(&self) -> Vec<(EntryKey, Revision)> {
        self.tombstones
            .iter()
            .map(|(key, revision)| (key.clone(), *revision))
            .collect()
    }

    fn apply_inner(&mut self, group: &[Mutation<E>]) -> Result<Vec<Placement>, MergeDefect> {
        let mut touched = BTreeSet::new();
        for mutation in group {
            match mutation {
                Mutation::Upsert {
                    key,
                    order,
                    revision,
                    entry,
                } => {
                    let canonical = self.resolve_key(key)?;
                    touched.insert(canonical.clone());
                    if self
                        .tombstones
                        .get(&canonical)
                        .is_some_and(|deleted| revision <= deleted)
                    {
                        continue;
                    }

                    if let Some(stored) = self.entries.get_mut(&canonical) {
                        stored.entry.merge(entry)?;
                        stored.revision = stored.revision.max(*revision);
                        Self::enforce_budget(self.entry_budget, &canonical, &mut stored.entry)?;
                    } else {
                        let mut materialized = E::from_partial(entry)?;
                        Self::enforce_budget(self.entry_budget, &canonical, &mut materialized)?;
                        self.entries.insert(
                            canonical.clone(),
                            Stored {
                                key: canonical,
                                segment: self.segment,
                                order: *order,
                                revision: *revision,
                                entry: materialized,
                            },
                        );
                    }
                }
                Mutation::Delete { key, revision } => {
                    let canonical = self.resolve_key(key)?;
                    touched.insert(canonical.clone());
                    let delete_applies = self
                        .entries
                        .get(&canonical)
                        .is_none_or(|stored| stored.revision <= *revision);
                    if delete_applies {
                        self.entries.remove(&canonical);
                        let tombstone = self.tombstones.entry(canonical).or_insert(*revision);
                        *tombstone = (*tombstone).max(*revision);
                    }
                }
                Mutation::Alias {
                    from,
                    to,
                    revision,
                    promote,
                } => {
                    let canonical = self.apply_alias(from, to, *revision, *promote)?;
                    touched.insert(canonical);
                }
            }
        }

        Ok(touched
            .into_iter()
            .filter_map(|key| {
                self.entries.get(&key).map(|stored| Placement {
                    key,
                    segment: stored.segment,
                    order: stored.order,
                    revision: stored.revision,
                })
            })
            .collect())
    }

    fn apply_alias(
        &mut self,
        from: &EntryKey,
        to: &EntryKey,
        revision: Revision,
        promote: Option<Promotion>,
    ) -> Result<EntryKey, MergeDefect> {
        if from == to {
            return Err(MergeDefect::AliasCycle {
                from: from.clone(),
                to: to.clone(),
            });
        }

        if let Some(present) = self.redirects.get(from) {
            if revision < present.revision {
                return self.resolve_key(from);
            }
            if revision == present.revision {
                if present.to == *to && present.promote == promote {
                    return self.resolve_key(to);
                }
                return Err(MergeDefect::EqualRevisionDisagreement {
                    field: format!("redirect:{from}"),
                    revision,
                });
            }
            if present.to == *to {
                let target = self.resolve_key(to)?;
                if let Some(entry) = self.entries.get_mut(&target) {
                    entry.entry.promote(promote)?;
                    entry.revision = entry.revision.max(revision);
                }
                self.redirects.insert(
                    from.clone(),
                    Redirect {
                        to: target.clone(),
                        revision,
                        promote,
                    },
                );
                return Ok(target);
            }
        }

        let source = self.resolve_key(from)?;
        let target = self.resolve_key(to)?;
        if source == target || self.path_contains(&target, &source)? {
            return Err(MergeDefect::AliasCycle {
                from: from.clone(),
                to: to.clone(),
            });
        }

        let source_entry = self.entries.remove(&source);
        let target_entry = self.entries.remove(&target);
        let merged = match (source_entry, target_entry) {
            (Some(mut source_entry), None) => {
                source_entry.key = target.clone();
                source_entry.revision = source_entry.revision.max(revision);
                source_entry.entry.promote(promote)?;
                Some(source_entry)
            }
            (None, Some(mut target_entry)) => {
                target_entry.revision = target_entry.revision.max(revision);
                target_entry.entry.promote(promote)?;
                Some(target_entry)
            }
            (Some(source_entry), Some(mut target_entry)) => {
                target_entry
                    .entry
                    .merge_alias(&source_entry.entry, promote)?;
                target_entry.revision = target_entry
                    .revision
                    .max(source_entry.revision)
                    .max(revision);
                Some(target_entry)
            }
            (None, None) => None,
        };

        if let Some(mut merged) = merged {
            Self::enforce_budget(self.entry_budget, &target, &mut merged.entry)?;
            if self
                .tombstones
                .get(&target)
                .is_none_or(|deleted| merged.revision > *deleted)
            {
                self.entries.insert(target.clone(), merged);
            }
        }

        self.redirects.insert(
            from.clone(),
            Redirect {
                to: target.clone(),
                revision,
                promote,
            },
        );
        for redirect in self.redirects.values_mut() {
            if redirect.to == source {
                redirect.to.clone_from(&target);
            }
        }
        Ok(target)
    }

    fn resolve_key(&self, key: &EntryKey) -> Result<EntryKey, MergeDefect> {
        let redirects = self
            .redirects
            .iter()
            .map(|(from, redirect)| (from.clone(), redirect.to.clone()))
            .collect::<BTreeMap<_, _>>();
        resolve(&redirects, key)
    }

    fn path_contains(&self, start: &EntryKey, wanted: &EntryKey) -> Result<bool, MergeDefect> {
        let mut current = start.clone();
        let mut seen = BTreeSet::new();
        while let Some(redirect) = self.redirects.get(&current) {
            if !seen.insert(current.clone()) {
                return Err(MergeDefect::AliasCycle {
                    from: start.clone(),
                    to: redirect.to.clone(),
                });
            }
            if redirect.to == *wanted {
                return Ok(true);
            }
            current.clone_from(&redirect.to);
        }
        Ok(false)
    }

    fn enforce_budget(
        entry_budget: usize,
        key: &EntryKey,
        entry: &mut E,
    ) -> Result<(), MergeDefect> {
        let encoded_bytes = entry.clip(entry_budget);
        if encoded_bytes > entry_budget {
            return Err(MergeDefect::EntryOverBudget {
                key: key.clone(),
                encoded_bytes,
                budget: entry_budget,
            });
        }
        Ok(())
    }
}

/// Allocates lifecycle revisions after all row revisions at the same sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleRevisions {
    through: u64,
    fence: ChatRevision,
    next_ordinal: u32,
}

impl LifecycleRevisions {
    pub fn new(through: u64, accepted_fence: ChatRevision) -> Result<Self, MergeDefect> {
        if accepted_fence == 0 {
            return Err(MergeDefect::InvalidLifecycleFence);
        }
        Ok(Self {
            through,
            fence: accepted_fence,
            next_ordinal: 0,
        })
    }

    pub fn next_revision(&mut self) -> Result<Revision, MergeDefect> {
        let revision = Revision {
            seq: self.through,
            fence: self.fence,
            ordinal: self.next_ordinal,
        };
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(MergeDefect::RevisionExhausted)?;
        Ok(revision)
    }
}
