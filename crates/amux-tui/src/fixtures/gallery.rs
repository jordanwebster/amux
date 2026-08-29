//! Exemplar transcripts that put every painted block kind on one screen.
//!
//! The gallery is how the chat's visual vocabulary is reviewed: one Claude
//! session whose rows were chosen so each block kind appears exactly once,
//! in its shortest honest form, and still fits a 120x40 terminal. Content
//! is deliberately terse — a gallery is read block by block, not for its
//! story — and the rows enter through the same transcript shapes a real
//! session writes, so nothing here can show a presentation the reducer
//! cannot actually produce.

use serde_json::{Value, json};

const SESSION: &str = "22222222-2222-4222-8222-222222222222";
/// The host the fixture agents live on, as the message wire spells it.
const HOST: &str = "00000000-0000-0000-0000-000000000001";

fn uuid_row(n: u32) -> String {
    format!("dddddddd-0000-4000-8000-0000{n:08}")
}

fn ready() -> Value {
    json!({"type": "amux.transcript_ready"})
}

fn mode(mode: &str) -> Value {
    json!({"type": "permission-mode", "permissionMode": mode})
}

fn prompt(n: u32, ts: &str, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {"role": "user", "content": text},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn assistant(n: u32, ts: &str, id: &str, content: Vec<Value>, stop: Option<&str>) -> Value {
    json!({
        "type": "assistant",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {
            "id": id,
            "role": "assistant",
            "content": content,
            "stop_reason": stop,
        },
    })
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

fn tool_result(id: &str, content: &str) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": content})
}

fn results(n: u32, ts: &str, results: Vec<Value>, sidecar: Option<Value>) -> Value {
    let mut row = json!({
        "type": "user",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": ts,
        "message": {"role": "user", "content": results},
    });
    if let Some(sidecar) = sidecar {
        row["toolUseResult"] = sidecar;
    }
    row
}

/// A landed edit's `structuredPatch`: the magnitude on the file-change row
/// is a fact the sidecar states, never a count the renderer infers.
fn edit_sidecar(path: &str, added: usize, removed: usize) -> Value {
    let lines: Vec<String> = (0..added)
        .map(|i| format!("+added {i}"))
        .chain((0..removed).map(|i| format!("-removed {i}")))
        .collect();
    json!({"filePath": path, "structuredPatch": [{"lines": lines}]})
}

/// A pending Edit permission, whose ask-time diff is what the panel shows.
fn edit_hook(path: &str, old: &str, new: &str) -> Value {
    json!({
        "type": "hook.permission_request",
        "tool_name": "Edit",
        "tool_input": {"file_path": path, "old_string": old, "new_string": new},
        "permission_mode": "default",
        // Exactly one suggestion: the panel only offers a scoped "always
        // allow" when the hook stated one, and refuses to guess otherwise.
        "permission_suggestions": [{
            "type": "addDirectories",
            "destination": "session",
            "directories": ["/work/amux"],
        }],
    })
}

/// A message another agent sent, in the tag shape the carrier writes into
/// the recipient's own transcript.
fn agent_message(n: u32, kind: &str, from: &str, text: &str) -> Value {
    let tag = format!(
        "<amux id=\"00000000-0000-4000-8000-0000000000a1\" kind=\"{kind}\" \
from=\"{from}/{HOST}\" from-id=\"00000000-0000-0000-0000-0000000000b0\" \
from-kind=\"codex\">\n{text}\n</amux>"
    );
    json!({
        "type": "user",
        "uuid": uuid_row(n),
        "sessionId": SESSION,
        "timestamp": "2026-08-12T09:11:40Z",
        "isMeta": false,
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": tag},
    })
}

/// One session written to show the chat's whole visual vocabulary at once.
///
/// The order is the order a real session would write these rows, so the
/// screen reads as a session rather than a catalogue: the prompt and the
/// thinking marker at the top, the work in the middle, the turn rule at
/// the bottom, and the pending ask docked under all of it.
///
/// A 120x40 screen holds fourteen blocks once the ask panel takes its ten
/// rows, and the vocabulary has more kinds than that, so three are left
/// out — each one a repeat of a shape already on the screen. The
/// compaction rule is the same muted rule as the turn rule. The
/// unrecognized row is the same glyph-and-continuation shape as the tool
/// line, in the warn accent the collapsed question already shows. The MCP
/// startup block only exists in Codex threads, and `codex-mcp-startup` is
/// where it is reviewed. Every block that is here is here whole: a
/// gallery that scrolls hides exactly what it was built to show.
pub(super) fn gallery_rows() -> Vec<Value> {
    let plan = "Cap the backoff at six attempts, then thread the cap through SyncOptions.";
    vec![
        ready(),
        mode("default"),
        prompt(1, "2026-08-12T09:10:00Z", "Cap the retry backoff."),
        assistant(
            2,
            "2026-08-12T09:10:09Z",
            "msg-think",
            vec![
                json!({"type": "thinking", "thinking": "Where the cap belongs"}),
                json!({"type": "text", "text": "The cap belongs in `RetryConfig`."}),
            ],
            Some("tool_use"),
        ),
        // Three consecutive reads and searches: the fold turns them into
        // one collapsed run, which is the only way that block appears.
        assistant(
            3,
            "2026-08-12T09:10:12Z",
            "msg-explore",
            vec![
                tool_use(
                    "toolu-read-1",
                    "Read",
                    json!({"file_path": "sync/config.rs"}),
                ),
                tool_use(
                    "toolu-read-2",
                    "Read",
                    json!({"file_path": "sync/client.rs"}),
                ),
                tool_use("toolu-grep", "Grep", json!({"pattern": "max_attempts"})),
            ],
            Some("tool_use"),
        ),
        results(
            4,
            "2026-08-12T09:10:13Z",
            vec![
                tool_result("toolu-read-1", "40 lines"),
                tool_result("toolu-read-2", "120 lines"),
                tool_result("toolu-grep", "3 matches"),
            ],
            None,
        ),
        assistant(
            5,
            "2026-08-12T09:10:20Z",
            "msg-bash",
            vec![tool_use(
                "toolu-bash",
                "Bash",
                json!({"command": "cargo check -p amux-sync"}),
            )],
            Some("tool_use"),
        ),
        results(
            6,
            "2026-08-12T09:10:24Z",
            vec![tool_result("toolu-bash", "Finished in 4.10s")],
            None,
        ),
        assistant(
            7,
            "2026-08-12T09:10:30Z",
            "msg-edit",
            vec![tool_use(
                "toolu-edit",
                "Edit",
                json!({"file_path": "sync/config.rs"}),
            )],
            Some("tool_use"),
        ),
        results(
            8,
            "2026-08-12T09:10:31Z",
            vec![tool_result("toolu-edit", "ok")],
            Some(edit_sidecar("sync/config.rs", 2, 1)),
        ),
        assistant(
            9,
            "2026-08-12T09:10:35Z",
            "msg-ask",
            vec![tool_use(
                "toolu-ask",
                "AskUserQuestion",
                json!({"questions": [{
                    "header": "Ceiling",
                    "question": "Cap",
                    "multiSelect": false,
                    "options": [{"label": "6 attempts"}, {"label": "10 attempts"}]
                }]}),
            )],
            Some("tool_use"),
        ),
        results(
            10,
            "2026-08-12T09:10:36Z",
            vec![tool_result("toolu-ask", "ok")],
            Some(json!({"questions": ["Cap"], "answers": {"Cap": "6 attempts"}})),
        ),
        assistant(
            11,
            "2026-08-12T09:10:40Z",
            "msg-plan",
            vec![tool_use(
                "toolu-plan",
                "ExitPlanMode",
                json!({"plan": plan, "planFilePath": "~/.claude/plans/retry.md"}),
            )],
            Some("tool_use"),
        ),
        results(
            12,
            "2026-08-12T09:10:41Z",
            vec![tool_result("toolu-plan", "User has approved your plan.")],
            Some(json!({"plan": plan, "filePath": "~/.claude/plans/retry.md"})),
        ),
        json!({
            "type": "user",
            "uuid": uuid_row(13),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:11:30Z",
            "message": {"role": "user", "content": "Subagent finished: trust store audited"},
            "origin": {"kind": "task-notification"},
        }),
        agent_message(
            14,
            "message",
            "codex-retry",
            "Backoff cap looks right to me.",
        ),
        json!({
            "type": "assistant",
            "uuid": uuid_row(15),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:11:50Z",
            "isApiErrorMessage": true,
            "error": "server_error",
            "message": {
                "id": "msg-api-error",
                "role": "assistant",
                "content": [],
                "stop_reason": null,
            },
        }),
        json!({
            "type": "system",
            "subtype": "turn_duration",
            "uuid": uuid_row(18),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:12:05Z",
            "durationMs": 62_000,
        }),
        // Both snippets end in a newline so the diff shows only the
        // change: an unterminated last line would add two `\ No newline`
        // rows that say nothing about this edit.
        edit_hook(
            "sync/config.rs",
            "    pub max_attempts: u8,\n",
            "    pub max_attempts: u16,\n",
        ),
    ]
}

/// A run of reads and searches on either side of an edit.
///
/// The edit is the point: exploration folds away, but anything that
/// changed the working tree stays on its own line between the two runs,
/// so a reader can see what was touched without opening anything.
pub(super) fn exploration_rows() -> Vec<Value> {
    vec![
        ready(),
        mode("default"),
        prompt(
            30,
            "2026-08-12T09:10:00Z",
            "Find every retry ceiling and raise the default.",
        ),
        assistant(
            31,
            "2026-08-12T09:10:05Z",
            "msg-run-1",
            vec![
                tool_use(
                    "toolu-run-1",
                    "Grep",
                    json!({"pattern": "max_attempts", "path": "sync"}),
                ),
                tool_use(
                    "toolu-run-2",
                    "Read",
                    json!({"file_path": "sync/config.rs"}),
                ),
                tool_use(
                    "toolu-run-3",
                    "Read",
                    json!({"file_path": "sync/client.rs"}),
                ),
                tool_use(
                    "toolu-run-4",
                    "Grep",
                    json!({"pattern": "RetryConfig", "path": "crates"}),
                ),
            ],
            Some("tool_use"),
        ),
        results(
            32,
            "2026-08-12T09:10:07Z",
            vec![
                tool_result("toolu-run-1", "3 matches"),
                tool_result("toolu-run-2", "40 lines"),
                tool_result("toolu-run-3", "120 lines"),
                tool_result("toolu-run-4", "7 matches"),
            ],
            None,
        ),
        assistant(
            33,
            "2026-08-12T09:10:12Z",
            "msg-run-edit",
            vec![tool_use(
                "toolu-run-edit",
                "Edit",
                json!({"file_path": "sync/config.rs"}),
            )],
            Some("tool_use"),
        ),
        results(
            34,
            "2026-08-12T09:10:13Z",
            vec![tool_result("toolu-run-edit", "ok")],
            Some(edit_sidecar("sync/config.rs", 3, 1)),
        ),
        assistant(
            35,
            "2026-08-12T09:10:18Z",
            "msg-run-2",
            vec![
                tool_use(
                    "toolu-run-5",
                    "Grep",
                    json!({"pattern": "max_attempts", "path": "crates"}),
                ),
                tool_use("toolu-run-6", "Read", json!({"file_path": "sync/tests.rs"})),
                tool_use(
                    "toolu-run-7",
                    "Read",
                    json!({"file_path": "sync/backoff.rs"}),
                ),
            ],
            Some("tool_use"),
        ),
        results(
            36,
            "2026-08-12T09:10:20Z",
            vec![
                tool_result("toolu-run-5", "5 matches"),
                tool_result("toolu-run-6", "80 lines"),
                tool_result("toolu-run-7", "60 lines"),
            ],
            None,
        ),
        assistant(
            37,
            "2026-08-12T09:10:26Z",
            "msg-run-done",
            vec![json!({
                "type": "text",
                "text": "The ceiling now lives in one place, and the tests read it from there.",
            })],
            Some("end_turn"),
        ),
        json!({
            "type": "system",
            "subtype": "turn_duration",
            "uuid": uuid_row(38),
            "sessionId": SESSION,
            "timestamp": "2026-08-12T09:10:27Z",
            "durationMs": 27_000,
        }),
    ]
}
