//! Claude-native folding of consecutive exploration tools into display runs.
//!
//! This module deliberately works from Claude's own feed kinds. The shared
//! frame sees only the resulting run key and summary; it never learns how a
//! Claude tool is classified.

// The Claude adapter consumes this fold in its next migration step.
#![allow(dead_code)]

use amux_ui::claude::{FeedEntry, FeedEntryKind, ToolInvocation};
use amux_ui::{AgentId, Model};

use crate::chat::blocks::{RunKey, RunSummary};

pub(crate) type EntryId = u64;

/// One native Claude entry, or a consecutive run of read-only exploration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeItem {
    Entry(EntryId),
    Run {
        key: RunKey,
        summary: RunSummary,
        members: Vec<EntryId>,
    },
}

/// Fold one Claude agent's feed without projecting its entries into a shared
/// representation. A run starts only when a second read/search explicitly
/// groups with the first, so a lone exploration tool remains an ordinary
/// entry.
pub(crate) fn fold_runs(model: &Model, agent: AgentId) -> Vec<ClaudeItem> {
    let Some(layer) = model.claude(agent) else {
        return Vec::new();
    };
    let entries = layer.entries().collect::<Vec<_>>();
    let mut items = Vec::with_capacity(entries.len());
    let mut start = 0;

    while start < entries.len() {
        if exploration(entries[start]).is_none() {
            items.push(ClaudeItem::Entry(entries[start].id));
            start += 1;
            continue;
        }

        let mut end = start + 1;
        while end < entries.len() && groups_with_previous(entries[end]) {
            end += 1;
        }

        if end - start == 1 {
            items.push(ClaudeItem::Entry(entries[start].id));
        } else {
            let members = &entries[start..end];
            items.push(ClaudeItem::Run {
                key: RunKey(entries[start].id),
                summary: summarize(members.iter().copied()),
                members: members.iter().map(|entry| entry.id).collect(),
            });
        }
        start = end;
    }

    items
}

fn exploration(entry: &FeedEntry) -> Option<&ToolInvocation> {
    let FeedEntryKind::Tool(tool) = &entry.kind else {
        return None;
    };
    match &tool.invocation {
        invocation @ (ToolInvocation::Read { .. } | ToolInvocation::Query { .. }) => {
            Some(invocation)
        }
        _ => None,
    }
}

fn groups_with_previous(entry: &FeedEntry) -> bool {
    matches!(
        &entry.kind,
        FeedEntryKind::Tool(tool)
            if tool.group_with_previous
                && matches!(
                    tool.invocation,
                    ToolInvocation::Read { .. } | ToolInvocation::Query { .. }
                )
    )
}

fn summarize<'a>(entries: impl Iterator<Item = &'a FeedEntry>) -> RunSummary {
    const PATH_PREVIEW: usize = 2;

    let mut summary = RunSummary::default();
    for entry in entries {
        match exploration(entry).expect("run members are exploration tools") {
            ToolInvocation::Read { file_path } => {
                summary.reads += 1;
                if summary.first_paths.len() < PATH_PREVIEW
                    && let Some(path) = file_path
                {
                    summary.first_paths.push(path.clone());
                }
            }
            ToolInvocation::Query { .. } => summary.searches += 1,
            _ => unreachable!("exploration filters every other invocation"),
        }
    }
    summary.hidden = summary.reads.saturating_sub(summary.first_paths.len());
    summary
}

#[cfg(test)]
mod tests {
    use amux_ui::{
        Agent, HostEntry, HostTrustStatus, Msg, ServerMsg, StreamEntry, StreamMsg, update,
    };
    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::*;

    const SESSION: &str = "22222222-2222-4222-8222-222222222222";

    fn agent_id() -> AgentId {
        Uuid::from_u128(7)
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("fixed test time")
    }

    fn tool_row(index: u64, name: &str, input: Value) -> Value {
        json!({
            "type": "assistant",
            "uuid": Uuid::from_u128(0x6000 + u128::from(index)).to_string(),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:00:01.000Z",
            "message": {
                "id": "msg-explore",
                "role": "assistant",
                "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use",
                    "id": format!("toolu-{index}"),
                    "name": name,
                    "input": input
                }]
            }
        })
    }

    fn result_row(index: u64) -> Value {
        json!({
            "type": "user",
            "uuid": Uuid::from_u128(0x7000 + u128::from(index)).to_string(),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:00:02.000Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": format!("toolu-{index}"),
                    "content": "done"
                }]
            }
        })
    }

    fn model(rows: Vec<Value>) -> Model {
        let host_id = Uuid::from_u128(1);
        let agent = Agent {
            id: agent_id(),
            host_id,
            name: Some("fix-auth".to_string()),
            command: "claude".to_string(),
            working_dir: "/work".into(),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                "terminal_v1".to_string(),
                "claude_pty_transcript_v1".to_string(),
            ],
            readonly: false,
            args: Vec::new(),
            created_at: at(0),
            parent: None,
            working_on: None,
        };
        let host = HostEntry {
            id: host_id,
            name: "mbp".to_string(),
            online: true,
            version: None,
            capabilities: None,
            trust_status: HostTrustStatus::Trusted,
            last_dial_error: None,
        };
        let messages = [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(host_id),
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::ReplayComplete,
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Batch {
                    at: at(1),
                    entries: rows
                        .into_iter()
                        .enumerate()
                        .map(|(offset, payload)| StreamEntry {
                            seq: offset as u64 + 1,
                            payload,
                        })
                        .collect(),
                },
            },
        ];
        let mut model = Model::default();
        for message in messages {
            update(&mut model, message);
        }
        assert!(model.check_invariants().is_empty());
        model
    }

    fn read(index: u64, path: &str) -> Value {
        tool_row(index, "Read", json!({"file_path": path}))
    }

    #[test]
    fn three_reads_fold_under_the_first_entry_id() {
        let model = model(vec![read(1, "a.rs"), read(2, "b.rs"), read(3, "c.rs")]);

        assert_eq!(
            fold_runs(&model, agent_id()),
            vec![ClaudeItem::Run {
                key: RunKey(0),
                summary: RunSummary {
                    reads: 3,
                    searches: 0,
                    first_paths: vec!["a.rs".to_string(), "b.rs".to_string()],
                    hidden: 1,
                },
                members: vec![0, 1, 2],
            }]
        );
    }

    #[test]
    fn an_edit_splits_read_edit_read_into_entries() {
        let model = model(vec![
            read(1, "a.rs"),
            tool_row(2, "Edit", json!({"file_path": "a.rs"})),
            read(3, "b.rs"),
        ]);

        assert_eq!(
            fold_runs(&model, agent_id()),
            vec![
                ClaudeItem::Entry(0),
                ClaudeItem::Entry(1),
                ClaudeItem::Entry(2),
            ]
        );
    }

    #[test]
    fn a_lone_read_stays_an_entry() {
        let model = model(vec![read(1, "only.rs")]);
        assert_eq!(fold_runs(&model, agent_id()), vec![ClaudeItem::Entry(0)]);
    }

    #[test]
    fn an_in_place_tool_outcome_does_not_interrupt_the_run() {
        let model = model(vec![
            read(1, "a.rs"),
            result_row(1),
            tool_row(2, "Grep", json!({"pattern": "retry"})),
            read(3, "b.rs"),
        ]);
        assert_eq!(
            fold_runs(&model, agent_id()),
            vec![ClaudeItem::Run {
                key: RunKey(0),
                summary: RunSummary {
                    reads: 2,
                    searches: 1,
                    first_paths: vec!["a.rs".to_string(), "b.rs".to_string()],
                    hidden: 0,
                },
                members: vec![0, 1, 2],
            }]
        );
    }

    #[test]
    fn a_missing_agent_has_no_items() {
        assert!(fold_runs(&Model::default(), agent_id()).is_empty());
    }
}
