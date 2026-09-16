use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use fold::{
    Baseline, BaselineReason, Boundary, BoundaryAt, CommitOutcome, CommitResult,
    DESKTOP_ENTRY_MAX_BYTES, Entry, EntryKey, ExpectedHead, Generations, Head, HeadState,
    JsonBytes, Mutation, Order, Page, PageToken, Placement, Promotion, ProviderFold, RedirectState,
    Revision, SegmentId, SegmentTransition, StoreError, Stored, WindowBudget, WindowInterest,
    coalesce,
};
use model::{AgentId, Progress, StructuredProtocol, Summary, SummaryEnvelope};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::map_sqlite_error;
use crate::families::{CHAT_SHAPE, CLAUDE_PTY, CLAUDE_SDK, CODEX, FLEET_SHAPE};

#[derive(Clone, Copy)]
struct ProviderTables {
    family: &'static str,
    tip: &'static str,
    entry: &'static str,
    tombstone: &'static str,
    alias: &'static str,
    protocol: i64,
    entry_shape: u32,
}

impl ProviderTables {
    fn for_fold<F: ProviderFold>() -> Self {
        match F::PROTOCOL {
            StructuredProtocol::ClaudePtyTranscript => Self {
                family: "claude_pty",
                tip: "claude_pty_tip",
                entry: "claude_pty_entry",
                tombstone: "claude_pty_tombstone",
                alias: "claude_pty_alias",
                protocol: 0,
                entry_shape: CLAUDE_PTY.shape,
            },
            StructuredProtocol::ClaudeSdk => Self {
                family: "claude_sdk",
                tip: "claude_sdk_tip",
                entry: "claude_sdk_entry",
                tombstone: "claude_sdk_tombstone",
                alias: "claude_sdk_alias",
                protocol: 1,
                entry_shape: CLAUDE_SDK.shape,
            },
            StructuredProtocol::Codex => Self {
                family: "codex",
                tip: "codex_tip",
                entry: "codex_entry",
                tombstone: "codex_tombstone",
                alias: "codex_alias",
                protocol: 2,
                entry_shape: CODEX.shape,
            },
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ChatState {
    fence: u64,
    content_revision: u64,
    segment_high_water: SegmentId,
    previous_through: Option<u64>,
    needs_baseline: Option<BaselineReason>,
}

#[derive(Clone)]
struct HeadMeta {
    version: u64,
    protocol: i64,
    segment: SegmentId,
    baseline: Baseline,
    through: u64,
    tip_version: u32,
    entry_version: u32,
    observed_at: DateTime<Utc>,
    summary: Vec<u8>,
}

pub(crate) fn load<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    window: WindowBudget,
) -> Result<fold::Loaded<F>, StoreError> {
    let transaction = connection.transaction().map_err(map_sqlite_error)?;
    let loaded = load_in_transaction::<F>(&transaction, agent, window)?;
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(loaded)
}

pub(crate) fn page<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    token: PageToken,
    n: usize,
) -> Result<Page<F::Entry>, StoreError> {
    let transaction = connection.transaction().map_err(map_sqlite_error)?;
    let tables = ProviderTables::for_fold::<F>();
    let generations = checked_generations(&transaction, tables)?;
    if generations != token.generations {
        return Err(StoreError::GenerationMoved);
    }
    let state = load_state(&transaction, agent)?;
    if state.content_revision != token.content_revision {
        return Err(StoreError::GenerationMoved);
    }

    let mut entries =
        load_entries_before::<F::Entry>(&transaction, tables, agent, &token.before, n)?;
    let has_older = entries
        .first()
        .map(|entry| has_entry_before(&transaction, tables, agent, position(entry)))
        .transpose()?
        .unwrap_or(false);
    let next = if has_older {
        entries.first().map(|entry| PageToken {
            generations,
            content_revision: state.content_revision,
            view_epoch: token.view_epoch,
            before: position(entry),
        })
    } else {
        None
    };
    let first_segment = entries
        .first()
        .map_or(token.before.0, |entry| entry.segment);
    let boundaries = load_boundaries(&transaction, tables, agent, first_segment, token.before.0)?;
    entries.sort_by_key(position);
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(Page {
        entries,
        boundaries,
        next,
        content_revision: state.content_revision,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the store transaction takes the complete optimistic commit contract"
)]
pub(crate) fn commit<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    generations: Generations,
    expected: ExpectedHead,
    head: Head<F>,
    transition: Option<SegmentTransition>,
    mutations: Vec<Mutation<F::Entry>>,
    interest: WindowInterest,
) -> CommitOutcome<F> {
    match commit_inner(
        connection,
        agent,
        generations,
        expected,
        head,
        transition,
        mutations,
        interest,
    ) {
        Ok(outcome) => outcome,
        Err(error) => CommitOutcome::Refused(error),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the transaction implementation preserves the public commit contract explicitly"
)]
fn commit_inner<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    generations: Generations,
    expected: ExpectedHead,
    head: Head<F>,
    transition: Option<SegmentTransition>,
    mutations: Vec<Mutation<F::Entry>>,
    interest: WindowInterest,
) -> Result<CommitOutcome<F>, StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_error)?;
    let tables = ProviderTables::for_fold::<F>();
    let current_generations = checked_generations(&transaction, tables)?;
    if current_generations != generations {
        return Err(StoreError::GenerationMoved);
    }
    let state = load_state(&transaction, agent)?;
    let present = load_head_meta(&transaction, agent)?;
    refuse_newer_head::<F>(present.as_ref(), tables)?;
    let matches_expected = expected_matches(
        expected,
        state,
        present.as_ref(),
        F::TIP_VERSION,
        F::ENTRY_VERSION,
    );
    if !matches_expected || (matches!(expected, ExpectedHead::Absent { .. }) && present.is_some()) {
        let loaded = load_in_transaction::<F>(
            &transaction,
            agent,
            WindowBudget::desktop(interest.view_epoch),
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        return Ok(CommitOutcome::Conflict(loaded));
    }
    validate_head::<F>(&head, tables)?;
    validate_mutations(&mutations, head.through)?;
    validate_derivation(state, present.as_ref(), &head, transition.as_ref())?;

    apply_transition(
        &transaction,
        agent,
        state,
        present.as_ref(),
        transition.as_ref(),
    )?;

    let aliases = load_redirects(&transaction, tables, agent)?;
    let existing_pairs = aliases
        .values()
        .map(|redirect| (redirect.from.clone(), redirect.to.clone()))
        .collect::<Vec<_>>();
    let groups = coalesce(&mutations, &existing_pairs).map_err(merge_error)?;
    let mut materializer =
        Materializer::<F::Entry>::new(&transaction, tables, agent, head.segment, aliases);
    for group in groups {
        materializer.apply(&group.mutations)?;
    }
    let materialized = materializer.finish(&interest)?;
    let changed_boundaries = transition
        .as_ref()
        .and_then(|value| {
            baseline_boundary(value.baseline).map(|boundary| (value.successor, boundary))
        })
        .map(|(segment, boundary)| {
            boundary_for_segment(&transaction, tables, agent, segment, boundary)
        })
        .transpose()?
        .into_iter()
        .collect();

    let tip = encode(&head.tip)?;
    let summary = encode(&head.summary)?;
    transaction
        .execute(
            &format!(
                "INSERT INTO {}(agent_id,tip) VALUES (?1,?2) \
                 ON CONFLICT(agent_id) DO UPDATE SET tip=excluded.tip",
                tables.tip
            ),
            params![agent.to_string(), tip],
        )
        .map_err(map_sqlite_error)?;
    let new_version = match present.as_ref() {
        Some(value) => value
            .version
            .checked_add(1)
            .ok_or(StoreError::UnsupportedFormat)?,
        None => 1,
    };
    let (baseline_kind, baseline_seq) = encode_baseline(head.baseline)?;
    transaction
        .execute(
            "INSERT INTO chat_head(
                agent_id,version,protocol,segment,baseline_kind,baseline_seq,through,
                tip_version,entry_version,observed_at,tip_bytes,summary)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(agent_id) DO UPDATE SET
                version=excluded.version,protocol=excluded.protocol,segment=excluded.segment,
                baseline_kind=excluded.baseline_kind,baseline_seq=excluded.baseline_seq,
                through=excluded.through,tip_version=excluded.tip_version,
                entry_version=excluded.entry_version,observed_at=excluded.observed_at,
                tip_bytes=excluded.tip_bytes,summary=excluded.summary",
            params![
                agent.to_string(),
                to_i64(new_version)?,
                tables.protocol,
                i64::from(head.segment),
                baseline_kind,
                baseline_seq,
                to_i64(head.through)?,
                i64::from(head.tip_version),
                i64::from(head.entry_version),
                head.observed_at.timestamp_millis(),
                to_i64(head.tip.tip_bytes() as u64)?,
                summary,
            ],
        )
        .map_err(map_sqlite_error)?;
    let new_fence = state
        .fence
        .checked_add(1)
        .ok_or(StoreError::UnsupportedFormat)?;
    let new_content = state
        .content_revision
        .checked_add(1)
        .ok_or(StoreError::UnsupportedFormat)?;
    let high_water = transition
        .as_ref()
        .map_or(state.segment_high_water.max(head.segment), |value| {
            value.successor
        });
    write_state(
        &transaction,
        agent,
        ChatState {
            fence: new_fence,
            content_revision: new_content,
            segment_high_water: high_water,
            previous_through: Some(head.through),
            needs_baseline: None,
        },
    )?;
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(CommitOutcome::Committed(CommitResult {
        expected: ExpectedHead::Present {
            fence: new_fence,
            version: new_version,
        },
        content_revision: new_content,
        placed: materialized.placed,
        bodies: materialized.bodies,
        deleted: materialized.deleted,
        redirected: materialized.redirected,
        boundaries: changed_boundaries,
    }))
}

pub(crate) fn invalidate<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    generations: Generations,
    expected: ExpectedHead,
    reason: BaselineReason,
) -> CommitOutcome<F> {
    match invalidate_inner::<F>(connection, agent, generations, expected, reason) {
        Ok(outcome) => outcome,
        Err(error) => CommitOutcome::Refused(error),
    }
}

fn invalidate_inner<F: ProviderFold>(
    connection: &mut Connection,
    agent: AgentId,
    generations: Generations,
    expected: ExpectedHead,
    reason: BaselineReason,
) -> Result<CommitOutcome<F>, StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_error)?;
    let tables = ProviderTables::for_fold::<F>();
    let current_generations = checked_generations(&transaction, tables)?;
    if current_generations != generations {
        return Err(StoreError::GenerationMoved);
    }
    let state = load_state(&transaction, agent)?;
    let present = load_head_meta(&transaction, agent)?;
    refuse_newer_head::<F>(present.as_ref(), tables)?;
    if state.needs_baseline.is_some()
        && present.is_none()
        && expected == (ExpectedHead::Absent { fence: state.fence })
    {
        let result = CommitResult {
            expected: ExpectedHead::Absent { fence: state.fence },
            content_revision: state.content_revision,
            placed: Vec::new(),
            bodies: Vec::new(),
            deleted: Vec::new(),
            redirected: Vec::new(),
            boundaries: Vec::new(),
        };
        transaction.commit().map_err(map_sqlite_error)?;
        return Ok(CommitOutcome::Committed(result));
    }
    if !expected_matches(
        expected,
        state,
        present.as_ref(),
        F::TIP_VERSION,
        F::ENTRY_VERSION,
    ) {
        let loaded = load_in_transaction::<F>(&transaction, agent, WindowBudget::desktop(0))?;
        transaction.commit().map_err(map_sqlite_error)?;
        return Ok(CommitOutcome::Conflict(loaded));
    }
    let Some(head) = present else {
        let loaded = load_in_transaction::<F>(&transaction, agent, WindowBudget::desktop(0))?;
        transaction.commit().map_err(map_sqlite_error)?;
        return Ok(CommitOutcome::Conflict(loaded));
    };
    let boundary = reason_boundary(reason);
    transaction
        .execute(
            "UPDATE segment SET last_seq=?3,closed_by=?4
             WHERE agent_id=?1 AND id=?2 AND last_seq IS NULL",
            params![
                agent.to_string(),
                i64::from(head.segment),
                to_i64(head.through)?,
                boundary_code(boundary),
            ],
        )
        .map_err(map_sqlite_error)?;
    transaction
        .execute(
            "DELETE FROM chat_head WHERE agent_id=?1",
            [agent.to_string()],
        )
        .map_err(map_sqlite_error)?;
    transaction
        .execute(
            &format!("DELETE FROM {} WHERE agent_id=?1", tables.tip),
            [agent.to_string()],
        )
        .map_err(map_sqlite_error)?;
    let new_fence = state
        .fence
        .checked_add(1)
        .ok_or(StoreError::UnsupportedFormat)?;
    let new_content = state
        .content_revision
        .checked_add(1)
        .ok_or(StoreError::UnsupportedFormat)?;
    write_state(
        &transaction,
        agent,
        ChatState {
            fence: new_fence,
            content_revision: new_content,
            segment_high_water: state.segment_high_water,
            previous_through: Some(head.through),
            needs_baseline: Some(reason),
        },
    )?;
    let boundary_at = BoundaryAt {
        segment: head.segment,
        before: None,
        boundary,
    };
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(CommitOutcome::Committed(CommitResult {
        expected: ExpectedHead::Absent { fence: new_fence },
        content_revision: new_content,
        placed: Vec::new(),
        bodies: Vec::new(),
        deleted: Vec::new(),
        redirected: Vec::new(),
        boundaries: vec![boundary_at],
    }))
}

fn load_in_transaction<F: ProviderFold>(
    transaction: &Transaction<'_>,
    agent: AgentId,
    window: WindowBudget,
) -> Result<fold::Loaded<F>, StoreError> {
    let tables = ProviderTables::for_fold::<F>();
    let generations = checked_generations(transaction, tables)?;
    let state = load_state(transaction, agent)?;
    let head_meta = load_head_meta(transaction, agent)?;
    let head = match head_meta {
        Some(meta) => load_head::<F>(transaction, tables, agent, &meta)?,
        None => match state.needs_baseline {
            Some(reason) => HeadState::NeedsBaseline {
                previous_through: state.previous_through.unwrap_or(0),
                reason,
            },
            None => HeadState::None,
        },
    };
    let mut window_entries = load_newest_entries::<F::Entry>(
        transaction,
        tables,
        agent,
        window.max_entries,
        window.max_bytes,
    )?;
    window_entries.sort_by_key(position);
    let first_page = window_entries
        .first()
        .map(|entry| {
            Ok(
                has_entry_before(transaction, tables, agent, position(entry))?.then(|| PageToken {
                    generations,
                    content_revision: state.content_revision,
                    view_epoch: window.view_epoch,
                    before: position(entry),
                }),
            )
        })
        .transpose()?
        .flatten();
    let first_segment = window_entries
        .first()
        .map_or(state.segment_high_water, |entry| entry.segment);
    let mut boundaries = if state.segment_high_water == 0 {
        Vec::new()
    } else {
        load_boundaries(
            transaction,
            tables,
            agent,
            first_segment,
            state.segment_high_water,
        )?
    };
    if matches!(&head, HeadState::NeedsBaseline { .. }) && state.segment_high_water > 0 {
        let pending = transaction
            .query_row(
                "SELECT closed_by FROM segment WHERE agent_id=?1 AND id=?2",
                params![agent.to_string(), i64::from(state.segment_high_water)],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(map_sqlite_error)?
            .flatten()
            .map(decode_boundary)
            .transpose()?;
        if let Some(boundary) = pending {
            let marker = BoundaryAt {
                segment: state.segment_high_water,
                before: None,
                boundary,
            };
            if !boundaries.contains(&marker) {
                boundaries.push(marker);
            }
        }
    }
    let window_keys = window_entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect::<BTreeSet<_>>();
    let aliases = load_redirects(transaction, tables, agent)?
        .into_values()
        .filter(|redirect| window_keys.contains(&redirect.to))
        .map(|redirect| (redirect.from, redirect.to))
        .collect();
    let host = load_host_summary(transaction, agent)?;
    let progress = load_progress(transaction, agent)?;

    Ok(fold::Loaded {
        generations,
        fence: state.fence,
        content_revision: state.content_revision,
        segment_high_water: state.segment_high_water,
        head,
        window: window_entries,
        boundaries,
        first_page,
        aliases,
        host,
        progress,
    })
}

fn load_head<F: ProviderFold>(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    meta: &HeadMeta,
) -> Result<HeadState<F>, StoreError> {
    if meta.protocol != tables.protocol
        || meta.tip_version > F::TIP_VERSION
        || meta.entry_version > F::ENTRY_VERSION
    {
        return Err(StoreError::UnsupportedFormat);
    }
    if meta.tip_version < F::TIP_VERSION || meta.entry_version < F::ENTRY_VERSION {
        return Ok(HeadState::NeedsBaseline {
            previous_through: meta.through,
            reason: BaselineReason::TipVersion,
        });
    }
    let tip = transaction
        .query_row(
            &format!("SELECT tip FROM {} WHERE agent_id=?1", tables.tip),
            [agent.to_string()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    let Some(tip) = tip else {
        return Ok(HeadState::NeedsBaseline {
            previous_through: meta.through,
            reason: BaselineReason::Corrupt,
        });
    };
    let Ok(tip) = postcard::from_bytes::<F>(&tip) else {
        return Ok(HeadState::NeedsBaseline {
            previous_through: meta.through,
            reason: BaselineReason::Corrupt,
        });
    };
    let Ok(summary) = postcard::from_bytes::<Summary>(&meta.summary) else {
        return Ok(HeadState::NeedsBaseline {
            previous_through: meta.through,
            reason: BaselineReason::Corrupt,
        });
    };
    Ok(HeadState::Usable(
        meta.version,
        Head {
            segment: meta.segment,
            baseline: meta.baseline,
            through: meta.through,
            tip_version: meta.tip_version,
            entry_version: meta.entry_version,
            tip,
            summary,
            observed_at: meta.observed_at,
        },
    ))
}

fn checked_generations(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
) -> Result<Generations, StoreError> {
    let mut values = BTreeMap::<String, (u32, u64)>::new();
    let mut statement = transaction
        .prepare(
            "SELECT family,shape,generation FROM family_shape
             WHERE family IN ('fleet','chat',?1)",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([tables.family], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    for (family, shape, generation) in rows {
        values.insert(
            family,
            (
                u32::try_from(shape).map_err(|_| StoreError::Corrupt)?,
                u64::try_from(generation).map_err(|_| StoreError::Corrupt)?,
            ),
        );
    }
    let read = |family: &str, supported: u32| -> Result<u64, StoreError> {
        let (shape, generation) = values.get(family).ok_or(StoreError::Corrupt)?;
        if *shape > supported {
            return Err(StoreError::UnsupportedFormat);
        }
        Ok(*generation)
    };
    Ok(Generations {
        fleet: read("fleet", FLEET_SHAPE)?,
        chat: read("chat", CHAT_SHAPE)?,
        provider: read(tables.family, tables.entry_shape)?,
    })
}

fn load_state(transaction: &Transaction<'_>, agent: AgentId) -> Result<ChatState, StoreError> {
    transaction
        .query_row(
            "SELECT revision,content_revision,segment_high_water,previous_through,needs_baseline
             FROM chat_state WHERE agent_id=?1",
            [agent.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(fence, content, high_water, previous, reason)| {
            Ok(ChatState {
                fence: from_i64(fence)?,
                content_revision: from_i64(content)?,
                segment_high_water: u32::try_from(high_water).map_err(|_| StoreError::Corrupt)?,
                previous_through: previous.map(from_i64).transpose()?,
                needs_baseline: decode_reason(reason)?,
            })
        })
        .transpose()
        .map(|state| state.unwrap_or_default())
}

fn load_head_meta(
    transaction: &Transaction<'_>,
    agent: AgentId,
) -> Result<Option<HeadMeta>, StoreError> {
    transaction
        .query_row(
            "SELECT version,protocol,segment,baseline_kind,baseline_seq,through,
                    tip_version,entry_version,observed_at,summary
             FROM chat_head WHERE agent_id=?1",
            [agent.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(
            |(
                version,
                protocol,
                segment,
                baseline_kind,
                baseline_seq,
                through,
                tip_version,
                entry_version,
                observed_at,
                summary,
            )| {
                Ok(HeadMeta {
                    version: from_i64(version)?,
                    protocol,
                    segment: u32::try_from(segment).map_err(|_| StoreError::Corrupt)?,
                    baseline: decode_baseline(baseline_kind, baseline_seq)?,
                    through: from_i64(through)?,
                    tip_version: u32::try_from(tip_version).map_err(|_| StoreError::Corrupt)?,
                    entry_version: u32::try_from(entry_version).map_err(|_| StoreError::Corrupt)?,
                    observed_at: timestamp(observed_at)?,
                    summary,
                })
            },
        )
        .transpose()
}

fn write_state(
    transaction: &Transaction<'_>,
    agent: AgentId,
    state: ChatState,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO chat_state(
                agent_id,revision,content_revision,segment_high_water,previous_through,needs_baseline)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(agent_id) DO UPDATE SET
                revision=excluded.revision,content_revision=excluded.content_revision,
                segment_high_water=excluded.segment_high_water,
                previous_through=excluded.previous_through,
                needs_baseline=excluded.needs_baseline",
            params![
                agent.to_string(),
                to_i64(state.fence)?,
                to_i64(state.content_revision)?,
                i64::from(state.segment_high_water),
                state.previous_through.map(to_i64).transpose()?,
                encode_reason(state.needs_baseline),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn load_newest_entries<E: Entry>(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    max_entries: usize,
    max_bytes: usize,
) -> Result<Vec<Stored<E>>, StoreError> {
    let mut statement = transaction
        .prepare(&format!(
            "SELECT key,segment,order_seq,order_slot,revision_seq,revision_fence,
                    revision_ordinal,body,bytes
             FROM {} WHERE agent_id=?1
             ORDER BY segment DESC,order_seq DESC,order_slot DESC,key DESC",
            tables.entry
        ))
        .map_err(map_sqlite_error)?;
    let mut rows = statement
        .query([agent.to_string()])
        .map_err(map_sqlite_error)?;
    let mut entries = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let encoded = row.get::<_, Vec<u8>>(7).map_err(map_sqlite_error)?;
        let row_bytes = usize::try_from(row.get::<_, i64>(8).map_err(map_sqlite_error)?)
            .map_err(|_| StoreError::Corrupt)?;
        if entries.len() >= max_entries || bytes.saturating_add(row_bytes) > max_bytes {
            break;
        }
        entries.push(decode_entry_row(row, &encoded)?);
        bytes = bytes.saturating_add(row_bytes);
    }
    Ok(entries)
}

fn load_entries_before<E: Entry>(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    before: &(SegmentId, Order, EntryKey),
    n: usize,
) -> Result<Vec<Stored<E>>, StoreError> {
    let mut statement = transaction
        .prepare(&format!(
            "SELECT key,segment,order_seq,order_slot,revision_seq,revision_fence,
                    revision_ordinal,body,bytes
             FROM {} WHERE agent_id=?1 AND
                (segment < ?2 OR
                 (segment = ?2 AND order_seq < ?3) OR
                 (segment = ?2 AND order_seq = ?3 AND order_slot < ?4) OR
                 (segment = ?2 AND order_seq = ?3 AND order_slot = ?4 AND key < ?5))
             ORDER BY segment DESC,order_seq DESC,order_slot DESC,key DESC LIMIT ?6",
            tables.entry
        ))
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            params![
                agent.to_string(),
                i64::from(before.0),
                to_i64(before.1.seq())?,
                i64::from(before.1.slot()),
                before.2.as_str(),
                i64::try_from(n).map_err(|_| StoreError::UnsupportedFormat)?,
            ],
            |row| {
                let body = row.get::<_, Vec<u8>>(7)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    body,
                ))
            },
        )
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter().map(decode_entry_tuple::<E>).collect()
}

fn decode_entry_row<E: Entry>(
    row: &rusqlite::Row<'_>,
    body: &[u8],
) -> Result<Stored<E>, StoreError> {
    decode_entry_tuple((
        row.get::<_, String>(0).map_err(map_sqlite_error)?,
        row.get::<_, i64>(1).map_err(map_sqlite_error)?,
        row.get::<_, i64>(2).map_err(map_sqlite_error)?,
        row.get::<_, i64>(3).map_err(map_sqlite_error)?,
        row.get::<_, i64>(4).map_err(map_sqlite_error)?,
        row.get::<_, i64>(5).map_err(map_sqlite_error)?,
        row.get::<_, i64>(6).map_err(map_sqlite_error)?,
        body.to_vec(),
    ))
}

type EntryTuple = (String, i64, i64, i64, i64, i64, i64, Vec<u8>);

fn decode_entry_tuple<E: Entry>(row: EntryTuple) -> Result<Stored<E>, StoreError> {
    Ok(Stored {
        key: EntryKey::new(row.0).map_err(|_| StoreError::Corrupt)?,
        segment: u32::try_from(row.1).map_err(|_| StoreError::Corrupt)?,
        order: Order::new(
            from_i64(row.2)?,
            u16::try_from(row.3).map_err(|_| StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Corrupt)?,
        revision: Revision {
            seq: from_i64(row.4)?,
            fence: from_i64(row.5)?,
            ordinal: u32::try_from(row.6).map_err(|_| StoreError::Corrupt)?,
        },
        entry: decode(&row.7)?,
    })
}

fn has_entry_before(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    before: (SegmentId, Order, EntryKey),
) -> Result<bool, StoreError> {
    transaction
        .query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE agent_id=?1 AND
                    (segment < ?2 OR
                     (segment = ?2 AND order_seq < ?3) OR
                     (segment = ?2 AND order_seq = ?3 AND order_slot < ?4) OR
                     (segment = ?2 AND order_seq = ?3 AND order_slot = ?4 AND key < ?5)))",
                tables.entry
            ),
            params![
                agent.to_string(),
                i64::from(before.0),
                to_i64(before.1.seq())?,
                i64::from(before.1.slot()),
                before.2.as_str(),
            ],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)
}

fn load_boundaries(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    first_segment: SegmentId,
    last_segment: SegmentId,
) -> Result<Vec<BoundaryAt>, StoreError> {
    if first_segment > last_segment {
        return Ok(Vec::new());
    }
    let mut statement = transaction
        .prepare(
            "SELECT id,baseline_kind FROM segment
             WHERE agent_id=?1 AND id BETWEEN ?2 AND ?3 ORDER BY id",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            params![
                agent.to_string(),
                i64::from(first_segment),
                i64::from(last_segment)
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    let mut boundaries = Vec::new();
    for (segment, kind) in rows {
        let segment = u32::try_from(segment).map_err(|_| StoreError::Corrupt)?;
        match kind {
            0 => {}
            1..=3 => boundaries.push(boundary_for_segment(
                transaction,
                tables,
                agent,
                segment,
                baseline_kind_boundary(kind).expect("validated boundary kind"),
            )?),
            _ => return Err(StoreError::Corrupt),
        }
    }
    Ok(boundaries)
}

fn boundary_for_segment(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
    segment: SegmentId,
    boundary: Boundary,
) -> Result<BoundaryAt, StoreError> {
    let before = transaction
        .query_row(
            &format!(
                "SELECT order_seq,order_slot,key FROM {} WHERE agent_id=?1 AND segment=?2
                 ORDER BY order_seq,order_slot,key LIMIT 1",
                tables.entry
            ),
            params![agent.to_string(), i64::from(segment)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(seq, slot, key)| {
            Ok((
                Order::new(
                    from_i64(seq)?,
                    u16::try_from(slot).map_err(|_| StoreError::Corrupt)?,
                )
                .map_err(|_| StoreError::Corrupt)?,
                EntryKey::new(key).map_err(|_| StoreError::Corrupt)?,
            ))
        })
        .transpose()?;
    Ok(BoundaryAt {
        segment,
        before,
        boundary,
    })
}

fn load_redirects(
    transaction: &Transaction<'_>,
    tables: ProviderTables,
    agent: AgentId,
) -> Result<BTreeMap<EntryKey, RedirectState>, StoreError> {
    let mut statement = transaction
        .prepare(&format!(
            "SELECT from_key,to_key,revision_seq,revision_fence,revision_ordinal,promotion
             FROM {} WHERE agent_id=?1 ORDER BY from_key",
            tables.alias
        ))
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([agent.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(|(from, to, seq, fence, ordinal, promotion)| {
            let from = EntryKey::new(from).map_err(|_| StoreError::Corrupt)?;
            Ok((
                from.clone(),
                RedirectState {
                    from,
                    to: EntryKey::new(to).map_err(|_| StoreError::Corrupt)?,
                    revision: Revision {
                        seq: from_i64(seq)?,
                        fence: from_i64(fence)?,
                        ordinal: u32::try_from(ordinal).map_err(|_| StoreError::Corrupt)?,
                    },
                    promote: decode_promotion(promotion)?,
                },
            ))
        })
        .collect()
}

fn load_host_summary(
    transaction: &Transaction<'_>,
    agent: AgentId,
) -> Result<Option<SummaryEnvelope>, StoreError> {
    transaction
        .query_row(
            "SELECT through,producer_version,observed_at,stale,revision,summary
             FROM host_summary WHERE agent_id=?1",
            [agent.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(
            |(through, producer_version, observed_at, stale, revision, summary)| {
                Ok(SummaryEnvelope {
                    through: from_i64(through)?,
                    producer_version: u32::try_from(producer_version)
                        .map_err(|_| StoreError::Corrupt)?,
                    observed_at: timestamp(observed_at)?,
                    stale: stale != 0,
                    revision: from_i64(revision)?,
                    summary: decode(&summary)?,
                })
            },
        )
        .transpose()
}

fn load_progress(
    transaction: &Transaction<'_>,
    agent: AgentId,
) -> Result<Option<Progress>, StoreError> {
    transaction
        .query_row(
            "SELECT through,at,revision FROM progress WHERE agent_id=?1",
            [agent.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(through, at, revision)| {
            Ok(Progress {
                through: from_i64(through)?,
                at: timestamp(at)?,
                revision: from_i64(revision)?,
            })
        })
        .transpose()
}

struct Materialized {
    placed: Vec<Placement>,
    bodies: Vec<Stored<JsonBytes>>,
    deleted: Vec<EntryKey>,
    redirected: Vec<(EntryKey, EntryKey)>,
}

struct Materializer<'a, E: Entry> {
    transaction: &'a Transaction<'a>,
    tables: ProviderTables,
    agent: AgentId,
    segment: SegmentId,
    redirects: BTreeMap<EntryKey, RedirectState>,
    touched: BTreeSet<EntryKey>,
    deleted: BTreeSet<EntryKey>,
    changed_redirects: BTreeSet<EntryKey>,
    marker: std::marker::PhantomData<E>,
}

impl<'a, E: Entry> Materializer<'a, E> {
    fn new(
        transaction: &'a Transaction<'a>,
        tables: ProviderTables,
        agent: AgentId,
        segment: SegmentId,
        redirects: BTreeMap<EntryKey, RedirectState>,
    ) -> Self {
        Self {
            transaction,
            tables,
            agent,
            segment,
            redirects,
            touched: BTreeSet::new(),
            deleted: BTreeSet::new(),
            changed_redirects: BTreeSet::new(),
            marker: std::marker::PhantomData,
        }
    }

    fn apply(&mut self, mutations: &[Mutation<E>]) -> Result<(), StoreError> {
        for mutation in mutations {
            match mutation {
                Mutation::Upsert {
                    key,
                    order,
                    revision,
                    entry,
                } => self.upsert(key, *order, *revision, entry)?,
                Mutation::Delete { key, revision } => self.delete(key, *revision)?,
                Mutation::Alias {
                    from,
                    to,
                    revision,
                    promote,
                } => self.alias(from, to, *revision, *promote)?,
            }
        }
        Ok(())
    }

    fn upsert(
        &mut self,
        key: &EntryKey,
        order: Order,
        revision: Revision,
        patch: &E::Partial,
    ) -> Result<(), StoreError> {
        let canonical = self.resolve(key)?;
        self.touched.insert(canonical.clone());
        if self
            .load_tombstone(&canonical)?
            .is_some_and(|deleted| revision <= deleted)
        {
            return Ok(());
        }
        let stored = self.load_entry(&canonical)?;
        let stored = if let Some(mut stored) = stored {
            stored.entry.merge(patch).map_err(merge_error)?;
            stored.entry.clip(DESKTOP_ENTRY_MAX_BYTES);
            if stored.entry.bytes() > DESKTOP_ENTRY_MAX_BYTES {
                return Err(StoreError::OverBudget);
            }
            Stored {
                revision: stored.revision.max(revision),
                ..stored
            }
        } else {
            let mut entry = E::from_partial(patch).map_err(merge_error)?;
            entry.clip(DESKTOP_ENTRY_MAX_BYTES);
            if entry.bytes() > DESKTOP_ENTRY_MAX_BYTES {
                return Err(StoreError::OverBudget);
            }
            Stored {
                key: canonical.clone(),
                segment: self.segment,
                order,
                revision,
                entry,
            }
        };
        self.save_entry(&stored)?;
        self.deleted.remove(&canonical);
        Ok(())
    }

    fn delete(&mut self, key: &EntryKey, revision: Revision) -> Result<(), StoreError> {
        let canonical = self.resolve(key)?;
        self.touched.insert(canonical.clone());
        let applies = self
            .load_entry(&canonical)?
            .is_none_or(|stored| stored.revision <= revision);
        if !applies {
            return Ok(());
        }
        self.transaction
            .execute(
                &format!(
                    "DELETE FROM {} WHERE agent_id=?1 AND key=?2",
                    self.tables.entry
                ),
                params![self.agent.to_string(), canonical.as_str()],
            )
            .map_err(map_sqlite_error)?;
        let tombstone = self
            .load_tombstone(&canonical)?
            .map_or(revision, |stored| stored.max(revision));
        self.save_tombstone(&canonical, tombstone)?;
        self.deleted.insert(canonical);
        Ok(())
    }

    fn alias(
        &mut self,
        from: &EntryKey,
        to: &EntryKey,
        revision: Revision,
        promote: Option<Promotion>,
    ) -> Result<(), StoreError> {
        if from == to {
            return Err(StoreError::Corrupt);
        }
        if let Some(present) = self.redirects.get(from).cloned() {
            if revision < present.revision {
                self.touched.insert(self.resolve(from)?);
                return Ok(());
            }
            if revision == present.revision {
                if present.to == *to && present.promote == promote {
                    self.touched.insert(self.resolve(to)?);
                    return Ok(());
                }
                return Err(StoreError::Corrupt);
            }
            if present.to == *to {
                let target = self.resolve(to)?;
                if let Some(mut entry) = self.load_entry(&target)? {
                    entry.entry.promote(promote).map_err(merge_error)?;
                    entry.revision = entry.revision.max(revision);
                    self.save_entry(&entry)?;
                    self.deleted.remove(&target);
                }
                self.store_redirect(from, &target, revision, promote)?;
                self.touched.insert(target);
                return Ok(());
            }
        }

        let source = self.resolve(from)?;
        let target = self.resolve(to)?;
        if source == target || self.path_contains(&target, &source)? {
            return Err(StoreError::Corrupt);
        }
        let source_entry = self.load_entry(&source)?;
        let target_entry = self.load_entry(&target)?;
        let merged = match (source_entry, target_entry) {
            (Some(mut source_entry), None) => {
                source_entry.key = target.clone();
                source_entry.revision = source_entry.revision.max(revision);
                source_entry.entry.promote(promote).map_err(merge_error)?;
                Some(source_entry)
            }
            (None, Some(mut target_entry)) => {
                target_entry.revision = target_entry.revision.max(revision);
                target_entry.entry.promote(promote).map_err(merge_error)?;
                Some(target_entry)
            }
            (Some(source_entry), Some(mut target_entry)) => {
                target_entry
                    .entry
                    .merge_alias(&source_entry.entry, promote)
                    .map_err(merge_error)?;
                target_entry.revision = target_entry
                    .revision
                    .max(source_entry.revision)
                    .max(revision);
                Some(target_entry)
            }
            (None, None) => None,
        };
        self.transaction
            .execute(
                &format!(
                    "DELETE FROM {} WHERE agent_id=?1 AND key=?2",
                    self.tables.entry
                ),
                params![self.agent.to_string(), source.as_str()],
            )
            .map_err(map_sqlite_error)?;
        if let Some(mut merged) = merged {
            merged.entry.clip(DESKTOP_ENTRY_MAX_BYTES);
            if merged.entry.bytes() > DESKTOP_ENTRY_MAX_BYTES {
                return Err(StoreError::OverBudget);
            }
            if self
                .load_tombstone(&target)?
                .is_none_or(|deleted| merged.revision > deleted)
            {
                self.save_entry(&merged)?;
                self.deleted.remove(&target);
            }
        }
        self.store_redirect(from, &target, revision, promote)?;
        let redirects_to_source = self
            .redirects
            .iter()
            .filter(|(_, redirect)| redirect.to == source)
            .map(|(key, redirect)| (key.clone(), redirect.revision, redirect.promote))
            .collect::<Vec<_>>();
        for (key, stored_revision, stored_promotion) in redirects_to_source {
            self.store_redirect(&key, &target, stored_revision, stored_promotion)?;
        }
        self.touched.insert(target);
        self.deleted.insert(source);
        Ok(())
    }

    fn finish(self, interest: &WindowInterest) -> Result<Materialized, StoreError> {
        let mut placed = Vec::new();
        let mut bodies = Vec::new();
        let mut result_bytes = 0usize;
        let mut emitted = BTreeSet::new();
        for key in &self.touched {
            let canonical = self.resolve(key)?;
            if !emitted.insert(canonical.clone()) {
                continue;
            }
            if let Some(stored) = self.load_entry(&canonical)? {
                if !interested(interest, &stored) {
                    continue;
                }
                let body = encode(&stored.entry)?;
                result_bytes = result_bytes
                    .checked_add(body.len())
                    .ok_or(StoreError::OverBudget)?;
                if result_bytes > interest.result_max_bytes {
                    return Err(StoreError::OverBudget);
                }
                placed.push(Placement {
                    key: stored.key.clone(),
                    segment: stored.segment,
                    order: stored.order,
                    revision: stored.revision,
                });
                bodies.push(Stored {
                    key: stored.key,
                    segment: stored.segment,
                    order: stored.order,
                    revision: stored.revision,
                    entry: JsonBytes(body),
                });
            }
        }
        placed.sort_by(|left, right| {
            (left.segment, left.order, &left.key).cmp(&(right.segment, right.order, &right.key))
        });
        bodies.sort_by(|left, right| {
            (left.segment, left.order, &left.key).cmp(&(right.segment, right.order, &right.key))
        });
        let mut redirected = self
            .changed_redirects
            .iter()
            .filter_map(|from| {
                self.redirects
                    .get(from)
                    .map(|redirect| (from.clone(), redirect.to.clone()))
            })
            .collect::<Vec<_>>();
        redirected.sort();
        Ok(Materialized {
            placed,
            bodies,
            deleted: self.deleted.into_iter().collect(),
            redirected,
        })
    }

    fn load_entry(&self, key: &EntryKey) -> Result<Option<Stored<E>>, StoreError> {
        self.transaction
            .query_row(
                &format!(
                    "SELECT key,segment,order_seq,order_slot,revision_seq,revision_fence,
                            revision_ordinal,body FROM {} WHERE agent_id=?1 AND key=?2",
                    self.tables.entry
                ),
                params![self.agent.to_string(), key.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Vec<u8>>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?
            .map(decode_entry_tuple::<E>)
            .transpose()
    }

    fn save_entry(&self, stored: &Stored<E>) -> Result<(), StoreError> {
        let body = encode(&stored.entry)?;
        self.transaction
            .execute(
                &format!(
                    "INSERT INTO {}(
                        agent_id,key,segment,order_seq,order_slot,revision_seq,revision_fence,
                        revision_ordinal,kind,text,bytes,body)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                     ON CONFLICT(agent_id,key) DO UPDATE SET
                        segment=excluded.segment,order_seq=excluded.order_seq,
                        order_slot=excluded.order_slot,revision_seq=excluded.revision_seq,
                        revision_fence=excluded.revision_fence,
                        revision_ordinal=excluded.revision_ordinal,kind=excluded.kind,
                        text=excluded.text,bytes=excluded.bytes,body=excluded.body",
                    self.tables.entry
                ),
                params![
                    self.agent.to_string(),
                    stored.key.as_str(),
                    i64::from(stored.segment),
                    to_i64(stored.order.seq())?,
                    i64::from(stored.order.slot()),
                    to_i64(stored.revision.seq)?,
                    to_i64(stored.revision.fence)?,
                    i64::from(stored.revision.ordinal),
                    stored.entry.kind(),
                    stored.entry.text(),
                    i64::try_from(body.len()).map_err(|_| StoreError::OverBudget)?,
                    body,
                ],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    fn load_tombstone(&self, key: &EntryKey) -> Result<Option<Revision>, StoreError> {
        self.transaction
            .query_row(
                &format!(
                    "SELECT revision_seq,revision_fence,revision_ordinal FROM {}
                     WHERE agent_id=?1 AND key=?2",
                    self.tables.tombstone
                ),
                params![self.agent.to_string(), key.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?
            .map(|(seq, fence, ordinal)| {
                Ok(Revision {
                    seq: from_i64(seq)?,
                    fence: from_i64(fence)?,
                    ordinal: u32::try_from(ordinal).map_err(|_| StoreError::Corrupt)?,
                })
            })
            .transpose()
    }

    fn save_tombstone(&self, key: &EntryKey, revision: Revision) -> Result<(), StoreError> {
        self.transaction
            .execute(
                &format!(
                    "INSERT INTO {}(
                        agent_id,key,revision_seq,revision_fence,revision_ordinal)
                     VALUES (?1,?2,?3,?4,?5)
                     ON CONFLICT(agent_id,key) DO UPDATE SET
                        revision_seq=excluded.revision_seq,
                        revision_fence=excluded.revision_fence,
                        revision_ordinal=excluded.revision_ordinal",
                    self.tables.tombstone
                ),
                params![
                    self.agent.to_string(),
                    key.as_str(),
                    to_i64(revision.seq)?,
                    to_i64(revision.fence)?,
                    i64::from(revision.ordinal),
                ],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    fn store_redirect(
        &mut self,
        from: &EntryKey,
        to: &EntryKey,
        revision: Revision,
        promote: Option<Promotion>,
    ) -> Result<(), StoreError> {
        self.transaction
            .execute(
                &format!(
                    "INSERT INTO {}(
                        agent_id,from_key,to_key,revision_seq,revision_fence,
                        revision_ordinal,promotion)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(agent_id,from_key) DO UPDATE SET
                        to_key=excluded.to_key,revision_seq=excluded.revision_seq,
                        revision_fence=excluded.revision_fence,
                        revision_ordinal=excluded.revision_ordinal,
                        promotion=excluded.promotion",
                    self.tables.alias
                ),
                params![
                    self.agent.to_string(),
                    from.as_str(),
                    to.as_str(),
                    to_i64(revision.seq)?,
                    to_i64(revision.fence)?,
                    i64::from(revision.ordinal),
                    encode_promotion(promote),
                ],
            )
            .map_err(map_sqlite_error)?;
        self.redirects.insert(
            from.clone(),
            RedirectState {
                from: from.clone(),
                to: to.clone(),
                revision,
                promote,
            },
        );
        self.changed_redirects.insert(from.clone());
        Ok(())
    }

    fn resolve(&self, key: &EntryKey) -> Result<EntryKey, StoreError> {
        let mut current = key.clone();
        let mut seen = BTreeSet::new();
        while let Some(redirect) = self.redirects.get(&current) {
            if !seen.insert(current.clone()) {
                return Err(StoreError::Corrupt);
            }
            current.clone_from(&redirect.to);
        }
        Ok(current)
    }

    fn path_contains(&self, start: &EntryKey, wanted: &EntryKey) -> Result<bool, StoreError> {
        let mut current = start.clone();
        let mut seen = BTreeSet::new();
        while let Some(redirect) = self.redirects.get(&current) {
            if !seen.insert(current.clone()) {
                return Err(StoreError::Corrupt);
            }
            if redirect.to == *wanted {
                return Ok(true);
            }
            current.clone_from(&redirect.to);
        }
        Ok(false)
    }
}

fn interested<E>(interest: &WindowInterest, stored: &Stored<E>) -> bool {
    if interest.held_keys.is_empty() && interest.feed_from.is_none() {
        return true;
    }
    interest.held_keys.contains(&stored.key)
        || interest
            .feed_from
            .as_ref()
            .is_some_and(|from| position(stored) >= from.clone())
}

fn expected_matches(
    expected: ExpectedHead,
    state: ChatState,
    present: Option<&HeadMeta>,
    tip_version: u32,
    entry_version: u32,
) -> bool {
    match expected {
        ExpectedHead::Present { fence, version } => {
            fence == state.fence
                && state.needs_baseline.is_none()
                && present.is_some_and(|head| {
                    head.version == version
                        && head.tip_version == tip_version
                        && head.entry_version == entry_version
                })
        }
        ExpectedHead::Absent { fence } => {
            fence == state.fence
                && (present.is_none()
                    || state.needs_baseline.is_some()
                    || present.is_some_and(|head| {
                        head.tip_version != tip_version || head.entry_version != entry_version
                    }))
        }
    }
}

fn refuse_newer_head<F: ProviderFold>(
    present: Option<&HeadMeta>,
    tables: ProviderTables,
) -> Result<(), StoreError> {
    if present.is_some_and(|head| {
        head.protocol != tables.protocol
            || head.tip_version > F::TIP_VERSION
            || head.entry_version > F::ENTRY_VERSION
    }) {
        Err(StoreError::UnsupportedFormat)
    } else {
        Ok(())
    }
}

fn validate_mutations<E: Entry>(mutations: &[Mutation<E>], through: u64) -> Result<(), StoreError> {
    let valid = mutations.iter().all(|mutation| match mutation {
        Mutation::Upsert {
            order, revision, ..
        } => order.seq() <= through && revision.seq <= through,
        Mutation::Delete { revision, .. } | Mutation::Alias { revision, .. } => {
            revision.seq <= through
        }
    });
    if valid {
        Ok(())
    } else {
        Err(StoreError::Corrupt)
    }
}

fn validate_head<F: ProviderFold>(
    head: &Head<F>,
    tables: ProviderTables,
) -> Result<(), StoreError> {
    if head.tip_version != F::TIP_VERSION
        || head.entry_version != F::ENTRY_VERSION
        || tables.protocol
            != match F::PROTOCOL {
                StructuredProtocol::ClaudePtyTranscript => 0,
                StructuredProtocol::ClaudeSdk => 1,
                StructuredProtocol::Codex => 2,
            }
    {
        return Err(StoreError::UnsupportedFormat);
    }
    if head.tip.tip_bytes() > F::TIP_BUDGET {
        return Err(StoreError::OverBudget);
    }
    if head.summary != head.tip.summary() {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn validate_derivation<F: ProviderFold>(
    state: ChatState,
    present: Option<&HeadMeta>,
    head: &Head<F>,
    transition: Option<&SegmentTransition>,
) -> Result<(), StoreError> {
    match transition {
        None => {
            let stored = present.ok_or(StoreError::Corrupt)?;
            if head.segment != stored.segment
                || head.baseline != stored.baseline
                || head.through < stored.through
            {
                return Err(StoreError::Corrupt);
            }
        }
        Some(transition) => {
            let expected_predecessor = present
                .map(|stored| stored.segment)
                .or_else(|| (state.segment_high_water > 0).then_some(state.segment_high_water));
            let expected_previous = present
                .map(|stored| stored.through)
                .or(state.previous_through)
                .unwrap_or(0);
            if transition.predecessor != expected_predecessor
                || transition.previous_through != expected_previous
                || transition.successor != state.segment_high_water.saturating_add(1)
                || transition.successor == 0
                || head.segment != transition.successor
                || head.baseline != transition.baseline
                || head.through != transition.replay_through
                || transition
                    .selected_from
                    .is_some_and(|first| first > transition.replay_through)
            {
                return Err(StoreError::Corrupt);
            }
            let baseline_matches_cut = match transition.baseline {
                Baseline::Start => {
                    expected_predecessor.is_none()
                        && (transition.replay_through == 0 || transition.selected_from == Some(1))
                }
                Baseline::Truncated { from } => {
                    expected_predecessor.is_none()
                        && (transition.selected_from == Some(from)
                            || (transition.selected_from.is_none()
                                && from == transition.replay_through.saturating_add(1)))
                }
                Baseline::Gap { after } | Baseline::VersionGap { after } => {
                    after == expected_previous
                }
            };
            let invalidation_matches = match state.needs_baseline {
                Some(BaselineReason::TipVersion) => {
                    transition.baseline
                        == (Baseline::VersionGap {
                            after: expected_previous,
                        })
                }
                Some(BaselineReason::Corrupt | BaselineReason::First) => {
                    transition.baseline
                        == (Baseline::Gap {
                            after: expected_previous,
                        })
                }
                None => true,
            };
            if !baseline_matches_cut || !invalidation_matches {
                return Err(StoreError::Corrupt);
            }
        }
    }
    Ok(())
}

fn apply_transition(
    transaction: &Transaction<'_>,
    agent: AgentId,
    state: ChatState,
    present: Option<&HeadMeta>,
    transition: Option<&SegmentTransition>,
) -> Result<(), StoreError> {
    let Some(transition) = transition else {
        return Ok(());
    };
    if let Some(predecessor) = transition.predecessor {
        transaction
            .execute(
                "UPDATE segment SET last_seq=?3,closed_by=?4
                 WHERE agent_id=?1 AND id=?2 AND last_seq IS NULL",
                params![
                    agent.to_string(),
                    i64::from(predecessor),
                    to_i64(transition.previous_through)?,
                    baseline_boundary(transition.baseline)
                        .map(boundary_code)
                        .unwrap_or(0),
                ],
            )
            .map_err(map_sqlite_error)?;
    } else if present.is_some() || state.segment_high_water != 0 {
        return Err(StoreError::Corrupt);
    }
    let (kind, sequence) = encode_baseline(transition.baseline)?;
    transaction
        .execute(
            "INSERT INTO segment(
                agent_id,id,predecessor,baseline_kind,baseline_seq,first_seq,last_seq,
                closed_by,opened_at)
             VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,?7)",
            params![
                agent.to_string(),
                i64::from(transition.successor),
                transition.predecessor.map(i64::from),
                kind,
                sequence,
                transition.selected_from.map(to_i64).transpose()?,
                transition.opened_at.timestamp_millis(),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn encode_baseline(baseline: Baseline) -> Result<(i64, Option<i64>), StoreError> {
    Ok(match baseline {
        Baseline::Start => (0, None),
        Baseline::Truncated { from } => (1, Some(to_i64(from)?)),
        Baseline::Gap { after } => (2, Some(to_i64(after)?)),
        Baseline::VersionGap { after } => (3, Some(to_i64(after)?)),
    })
}

fn decode_baseline(kind: i64, sequence: Option<i64>) -> Result<Baseline, StoreError> {
    match (kind, sequence) {
        (0, None) => Ok(Baseline::Start),
        (1, Some(value)) => Ok(Baseline::Truncated {
            from: from_i64(value)?,
        }),
        (2, Some(value)) => Ok(Baseline::Gap {
            after: from_i64(value)?,
        }),
        (3, Some(value)) => Ok(Baseline::VersionGap {
            after: from_i64(value)?,
        }),
        _ => Err(StoreError::Corrupt),
    }
}

fn baseline_boundary(baseline: Baseline) -> Option<Boundary> {
    match baseline {
        Baseline::Start => None,
        Baseline::Truncated { .. } => Some(Boundary::Truncated),
        Baseline::Gap { .. } => Some(Boundary::Gap),
        Baseline::VersionGap { .. } => Some(Boundary::VersionGap),
    }
}

fn baseline_kind_boundary(kind: i64) -> Option<Boundary> {
    match kind {
        1 => Some(Boundary::Truncated),
        2 => Some(Boundary::Gap),
        3 => Some(Boundary::VersionGap),
        _ => None,
    }
}

fn reason_boundary(reason: BaselineReason) -> Boundary {
    match reason {
        BaselineReason::TipVersion => Boundary::VersionGap,
        BaselineReason::Corrupt | BaselineReason::First => Boundary::Gap,
    }
}

fn boundary_code(boundary: Boundary) -> i64 {
    match boundary {
        Boundary::Truncated => 1,
        Boundary::Gap => 2,
        Boundary::VersionGap => 3,
        Boundary::Evicted => 4,
    }
}

fn decode_boundary(value: i64) -> Result<Boundary, StoreError> {
    match value {
        1 => Ok(Boundary::Truncated),
        2 => Ok(Boundary::Gap),
        3 => Ok(Boundary::VersionGap),
        4 => Ok(Boundary::Evicted),
        _ => Err(StoreError::Corrupt),
    }
}

fn encode_reason(reason: Option<BaselineReason>) -> i64 {
    match reason {
        None => 0,
        Some(BaselineReason::TipVersion) => 1,
        Some(BaselineReason::Corrupt) => 2,
        Some(BaselineReason::First) => 3,
    }
}

fn decode_reason(value: i64) -> Result<Option<BaselineReason>, StoreError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(BaselineReason::TipVersion)),
        2 => Ok(Some(BaselineReason::Corrupt)),
        3 => Ok(Some(BaselineReason::First)),
        _ => Err(StoreError::Corrupt),
    }
}

fn encode_promotion(promotion: Option<Promotion>) -> Option<i64> {
    promotion.map(|value| match value {
        Promotion::ToolToTask => 1,
    })
}

fn decode_promotion(value: Option<i64>) -> Result<Option<Promotion>, StoreError> {
    match value {
        None => Ok(None),
        Some(1) => Ok(Some(Promotion::ToolToTask)),
        _ => Err(StoreError::Corrupt),
    }
}

fn merge_error(error: fold::MergeDefect) -> StoreError {
    match error {
        fold::MergeDefect::EntryOverBudget { .. } => StoreError::OverBudget,
        _ => StoreError::Corrupt,
    }
}

fn position<E>(entry: &Stored<E>) -> (SegmentId, Order, EntryKey) {
    (entry.segment, entry.order, entry.key.clone())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    postcard::to_allocvec(value).map_err(|_| StoreError::Io)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    postcard::from_bytes(bytes).map_err(|_| StoreError::Corrupt)
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::UnsupportedFormat)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

fn timestamp(value: i64) -> Result<DateTime<Utc>, StoreError> {
    Utc.timestamp_millis_opt(value)
        .single()
        .ok_or(StoreError::Corrupt)
}
