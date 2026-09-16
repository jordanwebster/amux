use std::fmt::Write;

use fold::StoreError;
use model::{AgentId, Summary};
use rusqlite::{Connection, OptionalExtension};

use crate::db::map_sqlite_error;

const PROVIDERS: [(&str, &str); 3] = [
    ("claude_pty", "claude_pty_entry"),
    ("claude_sdk", "claude_sdk_entry"),
    ("codex", "codex_entry"),
];

pub(crate) fn render(connection: &Connection, agent: AgentId) -> Result<String, StoreError> {
    let agent = agent.to_string();
    let state = connection
        .query_row(
            "SELECT revision,content_revision,segment_high_water,previous_through,needs_baseline
             FROM chat_state WHERE agent_id=?1",
            [&agent],
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
        .map_err(map_sqlite_error)?;
    let mut output = String::new();
    writeln!(&mut output, "agent {agent}").expect("write to string");
    if let Some((fence, content, high_water, through, disposition)) = state {
        writeln!(
            &mut output,
            "state fence={fence} content_revision={content} segment_high_water={high_water} previous_through={} disposition={disposition}",
            through.map_or_else(|| "none".to_owned(), |value| value.to_string())
        )
        .expect("write to string");
    } else {
        output.push_str("state none\n");
    }

    let head = connection
        .query_row(
            "SELECT version,protocol,segment,baseline_kind,baseline_seq,through,tip_version,entry_version,observed_at,summary
             FROM chat_head WHERE agent_id=?1",
            [&agent],
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
        .map_err(map_sqlite_error)?;
    if let Some((
        version,
        protocol,
        segment,
        baseline,
        baseline_seq,
        through,
        tip,
        entry,
        observed,
        summary,
    )) = head
    {
        let summary: Summary = postcard::from_bytes(&summary).map_err(|_| StoreError::Corrupt)?;
        writeln!(
            &mut output,
            "head version={version} protocol={} segment={segment} boundary={} baseline_seq={} through={through} tip_version={tip} entry_version={entry} observed_at={observed}",
            protocol_name(protocol), boundary_name(baseline), show(baseline_seq)
        )
        .expect("write to string");
        let todo = summary.todo.as_ref().map_or_else(
            || "none".to_owned(),
            |todo| {
                format!(
                    "{}/{} current={:?}",
                    todo.done,
                    todo.total,
                    todo.current.as_deref().unwrap_or("")
                )
            },
        );
        writeln!(
            &mut output,
            "summary attention={:?} phase={:?} todo={todo} context={:?} model={:?} unknown={:?}",
            summary.attention, summary.phase, summary.context, summary.model, summary.unknown
        )
        .expect("write to string");
    }

    let mut segments = connection
        .prepare(
            "SELECT id,predecessor,baseline_kind,baseline_seq,first_seq,last_seq,closed_by,opened_at
             FROM segment WHERE agent_id=?1 ORDER BY id",
        )
        .map_err(map_sqlite_error)?;
    let rows = segments
        .query_map([&agent], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    for (id, predecessor, baseline, baseline_seq, first, last, closed, opened) in rows {
        writeln!(
            &mut output,
            "segment {id} predecessor={} boundary={} baseline_seq={} first={} last={} closed_by={} opened_at={opened}",
            show(predecessor), boundary_name(baseline), show(baseline_seq), show(first),
            show(last), closed.map_or("none", boundary_name)
        )
        .expect("write to string");
    }
    let frontier = connection
        .query_row(
            "SELECT segment,order_seq,order_slot,key FROM eviction_frontier WHERE agent_id=?1",
            [&agent],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if let Some((segment, seq, slot, key)) = frontier {
        writeln!(
            &mut output,
            "boundary Evicted frontier={segment}:{seq}:{slot}:{key}"
        )
        .expect("write to string");
    }

    for (provider, table) in PROVIDERS {
        let mut entries = connection
            .prepare(&format!(
                "SELECT key,segment,order_seq,order_slot,revision_seq,revision_fence,revision_ordinal,kind,text,bytes
                 FROM {table} WHERE agent_id=?1 ORDER BY segment,order_seq,order_slot,key"
            ))
            .map_err(map_sqlite_error)?;
        let rows = entries
            .query_map([&agent], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        for (key, segment, seq, slot, rev_seq, rev_fence, rev_ordinal, kind, text, bytes) in rows {
            writeln!(
                &mut output,
                "entry provider={provider} segment={segment} order={seq}:{slot} key={key} revision={rev_seq}:{rev_fence}:{rev_ordinal} kind={kind} bytes={bytes} text={:?}",
                text.unwrap_or_default()
            )
            .expect("write to string");
        }
    }
    Ok(output)
}

fn show(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn boundary_name(value: i64) -> &'static str {
    match value {
        0 => "Start",
        1 => "Truncated",
        2 => "Gap",
        3 => "VersionGap",
        4 => "Evicted",
        _ => "Invalid",
    }
}

fn protocol_name(value: i64) -> &'static str {
    match value {
        0 => "claude_pty",
        1 => "claude_sdk",
        2 => "codex",
        _ => "invalid",
    }
}
