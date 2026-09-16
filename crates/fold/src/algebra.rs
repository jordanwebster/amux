//! Reusable merge primitives for provider entries.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Entry, EntryKey, MergeDefect, Mutation, Patch, Revision, Seq};

/// Descriptive alias for [`Patch`] when it appears inside a partial entry.
pub type FieldPatch<T> = Patch<T>;

impl<T> Patch<T> {
    pub fn set(value: T, revision: Revision) -> Self {
        Self::Set { value, revision }
    }

    pub fn clear(revision: Revision) -> Self {
        Self::Clear { revision }
    }

    pub fn revision(&self) -> Option<Revision> {
        match self {
            Self::Unchanged => None,
            Self::Set { revision, .. } | Self::Clear { revision } => Some(*revision),
        }
    }
}

/// A nullable entry field and the revision that established its knowledge.
///
/// `revision == None` is unknown. A known clear is represented by a revision
/// and `value == None`, so it remains distinct from an unobserved field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedField<T> {
    revision: Option<Revision>,
    value: Option<T>,
}

impl<T> VersionedField<T> {
    pub fn revision(&self) -> Option<Revision> {
        self.revision
    }

    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut()
    }

    pub fn is_known(&self) -> bool {
        self.revision.is_some()
    }

    pub fn into_value(self) -> Option<T> {
        self.value
    }

    /// Alias merge rule: the target remains authoritative when it is known;
    /// an unknown target is filled from the source, including a known clear.
    pub fn fill_unknown_from(&mut self, source: &Self)
    where
        T: Clone,
    {
        if self.revision.is_none() {
            self.clone_from(source);
        }
    }
}

impl<T: Clone + Eq> VersionedField<T> {
    /// Apply one independently revisioned field patch.
    pub fn merge(&mut self, field: &str, patch: &FieldPatch<T>) -> Result<bool, MergeDefect> {
        let Some(incoming_revision) = patch.revision() else {
            return Ok(false);
        };

        let incoming_value = match patch {
            Patch::Unchanged => unreachable!("an unchanged patch has no revision"),
            Patch::Set { value, .. } => Some(value.clone()),
            Patch::Clear { .. } => None,
        };

        match self.revision {
            Some(current_revision) if incoming_revision < current_revision => Ok(false),
            Some(current_revision) if incoming_revision == current_revision => {
                if self.value == incoming_value {
                    Ok(false)
                } else {
                    Err(MergeDefect::EqualRevisionDisagreement {
                        field: field.to_owned(),
                        revision: incoming_revision,
                    })
                }
            }
            _ => {
                self.revision = Some(incoming_revision);
                self.value = incoming_value;
                Ok(true)
            }
        }
    }
}

/// Stable identity for one streamed or multi-row component.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ComponentSource {
    Sequence { seq: Seq, slot: u16 },
    Native { id: String, slot: u16 },
}

/// One independently retransmittable component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component<T> {
    pub source: ComponentSource,
    /// Original provider position, used for final replacement and clipping.
    pub observed_at: Seq,
    /// Relative-order evidence for providers whose delivery sequence may be
    /// republished under a new number.
    pub after: Vec<ComponentSource>,
    pub value: T,
}

/// Bounded component union with final-replacement provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Components<T> {
    values: Vec<Component<T>>,
    final_revision: Option<Revision>,
    final_through: Option<Seq>,
    clipped: bool,
}

impl<T> Components<T> {
    pub fn values(&self) -> &[Component<T>] {
        &self.values
    }

    pub fn final_through(&self) -> Option<Seq> {
        self.final_through
    }

    pub fn is_clipped(&self) -> bool {
        self.clipped
    }
}

impl<T: Clone + Eq> Components<T> {
    /// Union a delta by source. Reapplying the same delta is inert.
    pub fn merge(&mut self, component: Component<T>) -> Result<bool, MergeDefect> {
        if self
            .final_through
            .is_some_and(|through| component.observed_at <= through)
        {
            return Ok(false);
        }

        match self
            .values
            .binary_search_by(|present| present.source.cmp(&component.source))
        {
            Ok(index) => {
                let present = &mut self.values[index];
                if present.value != component.value {
                    return Err(MergeDefect::ComponentDisagreement {
                        source: format!("{:?}", component.source),
                    });
                }
                let before = present.after.len();
                let before_observed_at = present.observed_at;
                present.observed_at = present.observed_at.min(component.observed_at);
                present.after.extend(component.after);
                present.after.sort();
                present.after.dedup();
                Ok(present.after.len() != before || present.observed_at != before_observed_at)
            }
            Err(index) => {
                let mut component = component;
                component.after.sort();
                component.after.dedup();
                self.values.insert(index, component);
                Ok(true)
            }
        }
    }

    /// Replace every component at or below `through` with an authoritative
    /// final set. A repeated final replacement is idempotent.
    pub fn replace_final(
        &mut self,
        through: Seq,
        revision: Revision,
        mut replacement: Vec<Component<T>>,
    ) -> Result<bool, MergeDefect> {
        replacement.sort_by(|left, right| left.source.cmp(&right.source));
        let mut normalized = Vec::<Component<T>>::with_capacity(replacement.len());
        for mut component in replacement {
            component.after.sort();
            component.after.dedup();
            if let Some(previous) = normalized.last()
                && previous.source == component.source
            {
                if previous != &component {
                    return Err(MergeDefect::ComponentDisagreement {
                        source: format!("{:?}", component.source),
                    });
                }
                continue;
            }
            normalized.push(component);
        }
        let replacement = normalized;

        match self.final_revision {
            Some(current) if revision < current => return Ok(false),
            Some(current) if revision == current => {
                let mut expected = self
                    .values
                    .iter()
                    .filter(|component| component.observed_at > through)
                    .cloned()
                    .collect::<Vec<_>>();
                for component in replacement {
                    match expected.binary_search_by(|present| present.source.cmp(&component.source))
                    {
                        Ok(index) => expected[index] = component,
                        Err(index) => expected.insert(index, component),
                    }
                }
                if self.final_through == Some(through) && self.values == expected {
                    return Ok(false);
                }
                return Err(MergeDefect::EqualRevisionDisagreement {
                    field: "components.final".to_owned(),
                    revision,
                });
            }
            _ => {}
        }

        self.values.retain(|value| value.observed_at > through);
        for component in replacement {
            match self
                .values
                .binary_search_by(|present| present.source.cmp(&component.source))
            {
                Ok(index) => self.values[index] = component,
                Err(index) => self.values.insert(index, component),
            }
        }
        self.final_revision = Some(revision);
        self.final_through = Some(through);
        Ok(true)
    }

    /// Drop oldest components until both whole-entry component limits hold.
    pub fn clip_by(
        &mut self,
        max_components: usize,
        max_bytes: usize,
        mut component_bytes: impl FnMut(&Component<T>) -> usize,
    ) {
        let mut bytes = self.values.iter().map(&mut component_bytes).sum::<usize>();
        while self.values.len() > max_components || bytes > max_bytes {
            let Some((index, _)) = self
                .values
                .iter()
                .enumerate()
                .min_by_key(|(_, component)| (&component.observed_at, &component.source))
            else {
                break;
            };
            bytes = bytes.saturating_sub(component_bytes(&self.values[index]));
            self.values.remove(index);
            self.clipped = true;
        }
    }
}

/// Mutations that must be committed atomically for one final canonical key.
#[derive(Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct MutationGroup<E: Entry> {
    pub canonical: EntryKey,
    pub mutations: Vec<Mutation<E>>,
}

/// Resolve aliases introduced by a batch and retain source order within one
/// atomic group per final canonical key.
pub fn coalesce<E: Entry>(
    mutations: &[Mutation<E>],
    existing_redirects: &[(EntryKey, EntryKey)],
) -> Result<Vec<MutationGroup<E>>, MergeDefect> {
    let mut redirects = existing_redirects
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();

    for mutation in mutations {
        if let Mutation::Alias { from, to, .. } = mutation {
            if from == to || resolves_to(&redirects, to, from)? {
                return Err(MergeDefect::AliasCycle {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
            redirects.insert(from.clone(), to.clone());
            resolve(&redirects, from)?;
        }
    }

    let mut groups = Vec::<MutationGroup<E>>::new();
    for mutation in mutations {
        let key = match mutation {
            Mutation::Upsert { key, .. } | Mutation::Delete { key, .. } => key,
            Mutation::Alias { to, .. } => to,
        };
        let canonical = resolve(&redirects, key)?;
        if let Some(group) = groups.iter_mut().find(|group| group.canonical == canonical) {
            group.mutations.push(mutation.clone());
        } else {
            groups.push(MutationGroup {
                canonical,
                mutations: vec![mutation.clone()],
            });
        }
    }
    Ok(groups)
}

pub(crate) fn resolve(
    redirects: &BTreeMap<EntryKey, EntryKey>,
    key: &EntryKey,
) -> Result<EntryKey, MergeDefect> {
    let mut current = key.clone();
    let mut visited = BTreeSet::new();
    while let Some(next) = redirects.get(&current) {
        if !visited.insert(current.clone()) {
            return Err(MergeDefect::AliasCycle {
                from: key.clone(),
                to: next.clone(),
            });
        }
        current = next.clone();
    }
    Ok(current)
}

fn resolves_to(
    redirects: &BTreeMap<EntryKey, EntryKey>,
    start: &EntryKey,
    wanted: &EntryKey,
) -> Result<bool, MergeDefect> {
    Ok(resolve(redirects, start)? == *wanted)
}
