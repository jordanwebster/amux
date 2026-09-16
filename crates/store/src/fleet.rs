use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use fold::{
    Fleet, FleetAgent, FleetDelta, FleetHost, FleetSnapshot, Generations, Membership, StoreError,
};
use model::{
    Agent, AgentId, AgentKind, AgentParent, ClaudeDriver, HostEntry, HostId, HostTrustStatus,
    Progress, StructuredProtocol, SummaryEnvelope, WorkingOn,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{admit_growth, map_sqlite_error};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetChange {
    Changed,
    Unchanged,
}

pub(crate) fn apply(
    connection: &mut Connection,
    generations: Generations,
    delta: FleetDelta,
    now: DateTime<Utc>,
) -> Result<FleetChange, StoreError> {
    let growth = postcard::to_allocvec(&delta)
        .map_err(|_| StoreError::Io)?
        .len();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_error)?;
    admit_growth(
        &transaction,
        u64::try_from(growth).map_err(|_| StoreError::DiskFull)?,
    )?;
    check_generation(&transaction, generations)?;
    let changed = match delta {
        FleetDelta::Host { host, revision } => apply_host(&transaction, &host, revision, now)?,
        FleetDelta::Reachability { host_id, online } => {
            apply_reachability(&transaction, host_id, online)?
        }
        FleetDelta::Snapshot(snapshot) => apply_snapshot(&transaction, snapshot, now)?,
        FleetDelta::AgentUp { agent, revision } | FleetDelta::AgentUpdated { agent, revision } => {
            apply_agent(&transaction, &agent, revision)?
        }
        FleetDelta::AgentDown {
            host_id,
            agent_id,
            revision,
            reason: _,
        } => apply_agent_down(&transaction, host_id, agent_id, revision, now)?,
        FleetDelta::Summary {
            host_id,
            agent_id,
            envelope,
        } => apply_summary(&transaction, host_id, agent_id, &envelope)?,
        FleetDelta::Progress {
            host_id,
            agent_id,
            progress,
        } => apply_progress(&transaction, host_id, agent_id, &progress)?,
    };
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(if changed {
        FleetChange::Changed
    } else {
        FleetChange::Unchanged
    })
}

pub(crate) fn load(
    connection: &mut Connection,
    generations: Generations,
) -> Result<Fleet, StoreError> {
    let transaction = connection.transaction().map_err(map_sqlite_error)?;
    check_generation(&transaction, generations)?;
    let hosts = load_hosts(&transaction)?;
    let summaries = load_summaries(&transaction)?;
    let progress = load_progress(&transaction)?;
    let agents = load_agents(&transaction, &summaries, &progress)?;
    transaction.commit().map_err(map_sqlite_error)?;
    Ok(Fleet { hosts, agents })
}

fn check_generation(
    transaction: &Transaction<'_>,
    generations: Generations,
) -> Result<(), StoreError> {
    let generation: i64 = transaction
        .query_row(
            "SELECT generation FROM family_shape WHERE family='fleet'",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    if from_i64(generation)? != generations.fleet {
        return Err(StoreError::GenerationMoved);
    }
    Ok(())
}

fn apply_host(
    transaction: &Transaction<'_>,
    host: &HostEntry,
    revision: u64,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let current = transaction
        .query_row(
            "SELECT revision FROM host WHERE id=?1",
            [host.id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(from_i64)
        .transpose()?;
    if current.is_some_and(|current| revision <= current) {
        return Ok(false);
    }

    let capabilities = host
        .capabilities
        .as_ref()
        .map(|value| serde_json::to_vec(value).map_err(|_| StoreError::Io))
        .transpose()?;
    let trust = encode(&host.trust_status)?;
    transaction
        .execute(
            "INSERT INTO host(id,name,online,version,platform,capabilities,trust,dial_error,revision,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                online=excluded.online,
                version=excluded.version,
                platform=excluded.platform,
                capabilities=excluded.capabilities,
                trust=excluded.trust,
                dial_error=excluded.dial_error,
                revision=excluded.revision,
                updated_at=excluded.updated_at",
            params![
                host.id.to_string(),
                host.name,
                i64::from(host.online),
                host.version,
                host.platform,
                capabilities,
                trust,
                host.last_dial_error,
                to_i64(revision)?,
                now.timestamp_millis(),
            ],
        )
        .map_err(map_sqlite_error)?;
    advance_host_revision(transaction, host.id, revision)?;
    Ok(true)
}

fn apply_reachability(
    transaction: &Transaction<'_>,
    host_id: HostId,
    online: bool,
) -> Result<bool, StoreError> {
    let changed = transaction
        .execute(
            "UPDATE host SET online=?2 WHERE id=?1 AND online<>?2",
            params![host_id.to_string(), i64::from(online)],
        )
        .map_err(map_sqlite_error)?;
    Ok(changed > 0)
}

fn apply_agent(
    transaction: &Transaction<'_>,
    agent: &Agent,
    revision: u64,
) -> Result<bool, StoreError> {
    if ordinary_event_is_stale(
        transaction,
        agent.host_id,
        agent.id,
        revision,
        RevisionRow::Agent,
    )? {
        return Ok(false);
    }
    write_agent_facts(transaction, agent, revision, Membership::Cached, None)?;
    if let Some(summary) = &agent.summary {
        apply_summary_value(transaction, agent.id, summary)?;
    }
    if let Some(progress) = &agent.progress {
        apply_progress_value(transaction, agent.id, progress)?;
    }
    advance_host_revision(transaction, agent.host_id, revision)?;
    Ok(true)
}

fn apply_agent_down(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
    revision: u64,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    if ordinary_event_is_stale(transaction, host_id, agent_id, revision, RevisionRow::Agent)? {
        return Ok(false);
    }
    transaction
        .execute(
            "UPDATE agent
             SET membership=?3, absent_since=COALESCE(absent_since,?4)
             WHERE id=?1 AND host_id=?2",
            params![
                agent_id.to_string(),
                host_id.to_string(),
                membership_code(Membership::Absent),
                now.timestamp_millis(),
            ],
        )
        .map_err(map_sqlite_error)?;
    raise_removal_fence(transaction, host_id, agent_id, revision)?;
    advance_host_revision(transaction, host_id, revision)?;
    Ok(true)
}

fn apply_snapshot(
    transaction: &Transaction<'_>,
    snapshot: FleetSnapshot,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let (_, deleted_through) = host_revision(transaction, snapshot.host_id)?;
    if snapshot.through_revision < deleted_through {
        return Ok(false);
    }

    let mut seen = BTreeSet::new();
    for (agent, fact_revision) in &snapshot.agents {
        if agent.host_id != snapshot.host_id {
            return Err(StoreError::Io);
        }
        seen.insert(agent.id.to_string());
        let fence = removal_fence(transaction, snapshot.host_id, agent.id)?;
        if fence.is_some_and(|fence| fence > snapshot.through_revision) {
            continue;
        }
        let existing_revision = agent_revision(transaction, agent.id)?;
        if existing_revision.is_none_or(|existing| *fact_revision > existing) {
            write_agent_facts(transaction, agent, *fact_revision, Membership::Cached, None)?;
        } else {
            transaction
                .execute(
                    "UPDATE agent SET membership=?2, absent_since=NULL WHERE id=?1",
                    params![agent.id.to_string(), membership_code(Membership::Cached)],
                )
                .map_err(map_sqlite_error)?;
        }
        if let Some(summary) = &agent.summary {
            apply_summary_value(transaction, agent.id, summary)?;
        }
        if let Some(progress) = &agent.progress {
            apply_progress_value(transaction, agent.id, progress)?;
        }
    }

    let stored = stored_agents_for_host(transaction, snapshot.host_id)?;
    for (agent_id, revision) in stored {
        if seen.contains(&agent_id.to_string()) || revision >= snapshot.through_revision {
            continue;
        }
        transaction
            .execute(
                "UPDATE agent
                 SET membership=?2, absent_since=COALESCE(absent_since,?3)
                 WHERE id=?1",
                params![
                    agent_id.to_string(),
                    membership_code(Membership::Absent),
                    now.timestamp_millis(),
                ],
            )
            .map_err(map_sqlite_error)?;
        raise_removal_fence(
            transaction,
            snapshot.host_id,
            agent_id,
            snapshot.through_revision,
        )?;
    }
    transaction
        .execute(
            "INSERT INTO host_revision(host_id,revision,deleted_through) VALUES (?1,?2,?2)
             ON CONFLICT(host_id) DO UPDATE SET
                revision=MAX(host_revision.revision,excluded.revision),
                deleted_through=MAX(host_revision.deleted_through,excluded.deleted_through)",
            params![
                snapshot.host_id.to_string(),
                to_i64(snapshot.through_revision)?,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(true)
}

fn apply_summary(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
    envelope: &SummaryEnvelope,
) -> Result<bool, StoreError> {
    if ordinary_event_is_stale(
        transaction,
        host_id,
        agent_id,
        envelope.revision,
        RevisionRow::Summary,
    )? {
        return Ok(false);
    }
    let changed = apply_summary_value(transaction, agent_id, envelope)?;
    if changed {
        advance_host_revision(transaction, host_id, envelope.revision)?;
    }
    Ok(changed)
}

fn apply_progress(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
    progress: &Progress,
) -> Result<bool, StoreError> {
    if ordinary_event_is_stale(
        transaction,
        host_id,
        agent_id,
        progress.revision,
        RevisionRow::Progress,
    )? {
        return Ok(false);
    }
    let changed = apply_progress_value(transaction, agent_id, progress)?;
    if changed {
        advance_host_revision(transaction, host_id, progress.revision)?;
    }
    Ok(changed)
}

#[derive(Clone, Copy)]
enum RevisionRow {
    Agent,
    Summary,
    Progress,
}

fn ordinary_event_is_stale(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
    revision: u64,
    row: RevisionRow,
) -> Result<bool, StoreError> {
    let (_, deleted_through) = host_revision(transaction, host_id)?;
    if revision <= deleted_through
        || removal_fence(transaction, host_id, agent_id)?.is_some_and(|fence| revision <= fence)
    {
        return Ok(true);
    }
    let stored = match row {
        RevisionRow::Agent => agent_revision(transaction, agent_id)?,
        RevisionRow::Summary => summary_revision(transaction, agent_id)?,
        RevisionRow::Progress => progress_revision(transaction, agent_id)?,
    };
    Ok(stored.is_some_and(|stored| revision <= stored))
}

fn write_agent_facts(
    transaction: &Transaction<'_>,
    agent: &Agent,
    revision: u64,
    membership: Membership,
    absent_since: Option<DateTime<Utc>>,
) -> Result<(), StoreError> {
    let kind = encode(&agent.kind)?;
    let args = encode(&agent.args)?;
    let (parent_host_id, parent_id) = agent.parent.as_ref().map_or((None, None), |parent| {
        (
            Some(parent.host_id.to_string()),
            Some(parent.agent_id.to_string()),
        )
    });
    let (working_on, working_on_at) = agent.working_on.as_ref().map_or((None, None), |value| {
        (
            Some(value.text.clone()),
            Some(value.updated_at.timestamp_millis()),
        )
    });
    transaction
        .execute(
            "INSERT INTO agent(
                id,host_id,kind,protocol,name,command,working_dir,args,readonly,
                parent_host_id,parent_id,created_at,working_on,working_on_at,
                membership,revision,absent_since,last_opened_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,NULL)
             ON CONFLICT(id) DO UPDATE SET
                host_id=excluded.host_id,
                kind=excluded.kind,
                protocol=excluded.protocol,
                name=excluded.name,
                command=excluded.command,
                working_dir=excluded.working_dir,
                args=excluded.args,
                readonly=excluded.readonly,
                parent_host_id=excluded.parent_host_id,
                parent_id=excluded.parent_id,
                created_at=excluded.created_at,
                working_on=excluded.working_on,
                working_on_at=excluded.working_on_at,
                membership=excluded.membership,
                revision=excluded.revision,
                absent_since=excluded.absent_since",
            params![
                agent.id.to_string(),
                agent.host_id.to_string(),
                kind,
                protocol_code(&agent.kind),
                agent.name,
                agent.command,
                agent.working_dir.to_string_lossy(),
                args,
                i64::from(agent.readonly),
                parent_host_id,
                parent_id,
                agent.created_at.timestamp_millis(),
                working_on,
                working_on_at,
                membership_code(membership),
                to_i64(revision)?,
                absent_since.map(|value| value.timestamp_millis()),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn apply_summary_value(
    transaction: &Transaction<'_>,
    agent_id: AgentId,
    envelope: &SummaryEnvelope,
) -> Result<bool, StoreError> {
    if summary_revision(transaction, agent_id)?.is_some_and(|stored| envelope.revision <= stored) {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO host_summary(agent_id,through,producer_version,observed_at,stale,revision,summary,unknown)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(agent_id) DO UPDATE SET
                through=excluded.through,
                producer_version=excluded.producer_version,
                observed_at=excluded.observed_at,
                stale=excluded.stale,
                revision=excluded.revision,
                summary=excluded.summary,
                unknown=excluded.unknown",
            params![
                agent_id.to_string(),
                to_i64(envelope.through)?,
                i64::from(envelope.producer_version),
                envelope.observed_at.timestamp_millis(),
                i64::from(envelope.stale),
                to_i64(envelope.revision)?,
                encode(&envelope.summary)?,
                encode(&envelope.summary.unknown)?,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(true)
}

fn apply_progress_value(
    transaction: &Transaction<'_>,
    agent_id: AgentId,
    progress: &Progress,
) -> Result<bool, StoreError> {
    let current = transaction
        .query_row(
            "SELECT through,revision FROM progress WHERE agent_id=?1",
            [agent_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(through, revision)| Ok((from_i64(through)?, from_i64(revision)?)))
        .transpose()?;
    if current.is_some_and(|(_, revision)| progress.revision <= revision) {
        return Ok(false);
    }
    match current {
        Some((through, _)) if progress.through < through => {
            transaction
                .execute(
                    "UPDATE progress SET revision=?2 WHERE agent_id=?1",
                    params![agent_id.to_string(), to_i64(progress.revision)?],
                )
                .map_err(map_sqlite_error)?;
        }
        _ => {
            transaction
                .execute(
                    "INSERT INTO progress(agent_id,through,at,revision) VALUES (?1,?2,?3,?4)
                     ON CONFLICT(agent_id) DO UPDATE SET
                        through=excluded.through,
                        at=excluded.at,
                        revision=excluded.revision",
                    params![
                        agent_id.to_string(),
                        to_i64(progress.through)?,
                        progress.at.timestamp_millis(),
                        to_i64(progress.revision)?,
                    ],
                )
                .map_err(map_sqlite_error)?;
        }
    }
    Ok(true)
}

fn advance_host_revision(
    transaction: &Transaction<'_>,
    host_id: HostId,
    revision: u64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO host_revision(host_id,revision,deleted_through) VALUES (?1,?2,0)
             ON CONFLICT(host_id) DO UPDATE SET revision=MAX(host_revision.revision,excluded.revision)",
            params![host_id.to_string(), to_i64(revision)?],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn raise_removal_fence(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
    revision: u64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO removal_fence(host_id,agent_id,revision) VALUES (?1,?2,?3)
             ON CONFLICT(host_id,agent_id) DO UPDATE SET revision=MAX(removal_fence.revision,excluded.revision)",
            params![
                host_id.to_string(),
                agent_id.to_string(),
                to_i64(revision)?,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn host_revision(transaction: &Transaction<'_>, host_id: HostId) -> Result<(u64, u64), StoreError> {
    transaction
        .query_row(
            "SELECT revision,deleted_through FROM host_revision WHERE host_id=?1",
            [host_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(revision, deleted)| Ok((from_i64(revision)?, from_i64(deleted)?)))
        .transpose()
        .map(|value| value.unwrap_or((0, 0)))
}

fn removal_fence(
    transaction: &Transaction<'_>,
    host_id: HostId,
    agent_id: AgentId,
) -> Result<Option<u64>, StoreError> {
    transaction
        .query_row(
            "SELECT revision FROM removal_fence WHERE host_id=?1 AND agent_id=?2",
            params![host_id.to_string(), agent_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(from_i64)
        .transpose()
}

fn revision_from_table(
    transaction: &Transaction<'_>,
    table: &str,
    id_column: &str,
    agent_id: AgentId,
) -> Result<Option<u64>, StoreError> {
    let sql = format!("SELECT revision FROM {table} WHERE {id_column}=?1");
    transaction
        .query_row(&sql, [agent_id.to_string()], |row| row.get::<_, i64>(0))
        .optional()
        .map_err(map_sqlite_error)?
        .map(from_i64)
        .transpose()
}

fn agent_revision(
    transaction: &Transaction<'_>,
    agent_id: AgentId,
) -> Result<Option<u64>, StoreError> {
    revision_from_table(transaction, "agent", "id", agent_id)
}

fn summary_revision(
    transaction: &Transaction<'_>,
    agent_id: AgentId,
) -> Result<Option<u64>, StoreError> {
    revision_from_table(transaction, "host_summary", "agent_id", agent_id)
}

fn progress_revision(
    transaction: &Transaction<'_>,
    agent_id: AgentId,
) -> Result<Option<u64>, StoreError> {
    revision_from_table(transaction, "progress", "agent_id", agent_id)
}

fn stored_agents_for_host(
    transaction: &Transaction<'_>,
    host_id: HostId,
) -> Result<Vec<(AgentId, u64)>, StoreError> {
    let mut statement = transaction
        .prepare("SELECT id,revision FROM agent WHERE host_id=?1")
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([host_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(|(id, revision)| Ok((parse_id(&id)?, from_i64(revision)?)))
        .collect()
}

fn load_hosts(transaction: &Transaction<'_>) -> Result<Vec<FleetHost>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT id,name,online,version,platform,capabilities,trust,dial_error,revision,updated_at
             FROM host ORDER BY id",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
                row.get::<_, Vec<u8>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(
            |(
                id,
                name,
                online,
                version,
                platform,
                capabilities,
                trust,
                last_dial_error,
                revision,
                updated_at,
            )| {
                Ok(FleetHost {
                    host: HostEntry {
                        id: parse_id(&id)?,
                        name,
                        online: online != 0,
                        version,
                        capabilities: capabilities
                            .as_deref()
                            .map(|bytes| {
                                serde_json::from_slice(bytes).map_err(|_| StoreError::Corrupt)
                            })
                            .transpose()?,
                        trust_status: decode::<HostTrustStatus>(&trust)?,
                        last_dial_error,
                        platform,
                    },
                    revision: from_i64(revision)?,
                    updated_at: timestamp(updated_at)?,
                })
            },
        )
        .collect()
}

fn load_summaries(
    transaction: &Transaction<'_>,
) -> Result<BTreeMap<String, SummaryEnvelope>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT agent_id,through,producer_version,observed_at,stale,revision,summary
             FROM host_summary ORDER BY agent_id",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Vec<u8>>(6)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(
            |(id, through, producer_version, observed_at, stale, revision, summary)| {
                Ok((
                    id,
                    SummaryEnvelope {
                        through: from_i64(through)?,
                        producer_version: u32::try_from(producer_version)
                            .map_err(|_| StoreError::Corrupt)?,
                        observed_at: timestamp(observed_at)?,
                        stale: stale != 0,
                        revision: from_i64(revision)?,
                        summary: decode(&summary)?,
                    },
                ))
            },
        )
        .collect()
}

fn load_progress(transaction: &Transaction<'_>) -> Result<BTreeMap<String, Progress>, StoreError> {
    let mut statement = transaction
        .prepare("SELECT agent_id,through,at,revision FROM progress ORDER BY agent_id")
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(|(id, through, at, revision)| {
            Ok((
                id,
                Progress {
                    through: from_i64(through)?,
                    at: timestamp(at)?,
                    revision: from_i64(revision)?,
                },
            ))
        })
        .collect()
}

fn load_agents(
    transaction: &Transaction<'_>,
    summaries: &BTreeMap<String, SummaryEnvelope>,
    progress: &BTreeMap<String, Progress>,
) -> Result<Vec<FleetAgent>, StoreError> {
    type AgentRow = (
        String,
        String,
        Vec<u8>,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<u8>,
        i64,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
        Option<i64>,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let mut statement = transaction
        .prepare(
            "SELECT id,host_id,kind,name,command,working_dir,args,readonly,
                    parent_host_id,parent_id,created_at,working_on,working_on_at,
                    membership,revision,absent_since,last_opened_at
             FROM agent ORDER BY id",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<AgentRow>, _>>()
        .map_err(map_sqlite_error)?;
    rows.into_iter()
        .map(
            |(
                id,
                host_id,
                kind,
                name,
                command,
                working_dir,
                args,
                readonly,
                parent_host_id,
                parent_id,
                created_at,
                working_on,
                working_on_at,
                membership,
                revision,
                absent_since,
                last_opened_at,
            )| {
                let agent_id = parse_id(&id)?;
                let host_id = parse_id(&host_id)?;
                let parent = match (parent_host_id, parent_id) {
                    (Some(host_id), Some(agent_id)) => Some(AgentParent {
                        host_id: parse_id(&host_id)?,
                        agent_id: parse_id(&agent_id)?,
                    }),
                    (None, None) => None,
                    _ => return Err(StoreError::Corrupt),
                };
                let working_on = match (working_on, working_on_at) {
                    (Some(text), Some(updated_at)) => Some(WorkingOn {
                        text,
                        updated_at: timestamp(updated_at)?,
                    }),
                    (None, None) => None,
                    _ => return Err(StoreError::Corrupt),
                };
                Ok(FleetAgent {
                    agent: Agent {
                        id: agent_id,
                        host_id,
                        name,
                        command: command.ok_or(StoreError::Corrupt)?,
                        working_dir: PathBuf::from(working_dir.ok_or(StoreError::Corrupt)?),
                        kind: decode::<AgentKind>(&kind)?,
                        readonly: readonly != 0,
                        args: decode(&args)?,
                        created_at: timestamp(created_at)?,
                        parent,
                        working_on,
                        summary: summaries.get(&id).cloned(),
                        progress: progress.get(&id).cloned(),
                        inventory_revision: from_i64(revision)?,
                    },
                    membership: decode_membership(membership)?,
                    absent_since: absent_since.map(timestamp).transpose()?,
                    last_opened_at: last_opened_at.map(timestamp).transpose()?,
                })
            },
        )
        .collect()
}

fn protocol_code(kind: &AgentKind) -> Option<i64> {
    let protocol = match kind {
        AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        } => StructuredProtocol::ClaudePtyTranscript,
        AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        } => StructuredProtocol::ClaudeSdk,
        AgentKind::Codex => StructuredProtocol::Codex,
        AgentKind::TestAgent => return None,
    };
    Some(match protocol {
        StructuredProtocol::ClaudePtyTranscript => 0,
        StructuredProtocol::ClaudeSdk => 1,
        StructuredProtocol::Codex => 2,
    })
}

fn membership_code(membership: Membership) -> i64 {
    match membership {
        Membership::Cached => 0,
        Membership::Absent => 1,
    }
}

fn decode_membership(value: i64) -> Result<Membership, StoreError> {
    match value {
        0 => Ok(Membership::Cached),
        1 => Ok(Membership::Absent),
        _ => Err(StoreError::Corrupt),
    }
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

fn parse_id(value: &str) -> Result<AgentId, StoreError> {
    value.parse().map_err(|_| StoreError::Corrupt)
}

fn timestamp(value: i64) -> Result<DateTime<Utc>, StoreError> {
    Utc.timestamp_millis_opt(value)
        .single()
        .ok_or(StoreError::Corrupt)
}
