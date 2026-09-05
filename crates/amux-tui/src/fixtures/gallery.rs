//! Exemplar transcripts that put every painted block kind on one screen.
//!
//! The gallery is how the chat's visual vocabulary is reviewed: sessions
//! whose rows were chosen so each block kind appears exactly once, in its
//! shortest honest form, and still fits a 120x40 terminal. Content is
//! deliberately terse — a gallery is read block by block, not for its
//! story — and the rows enter through the same transcript shapes a real
//! session writes, so nothing here can show a presentation the reducer
//! cannot actually produce.
//!
//! It takes two pages, because no single session can produce the whole
//! vocabulary. Three block kinds only exist downstream of a Codex thread:
//! the compaction rule, the unrecognized row and the MCP startup block —
//! Claude's transcript has no content type that folds to any of them. The
//! numbered diff gutter is the same kind of gap in the other direction: a
//! Claude ask-time diff is numberless by design, because the edit has not
//! landed and there are no line numbers to be honest about, so the Claude
//! page can show a diff but never a gutter. Codex patches carry hunk
//! headers, so the Codex page is where the gutter is reviewed.
//!
//! The pages are therefore split by what each provider can say, not by
//! how much fits: the Claude page holds the shared vocabulary, and the
//! Codex page holds what Claude cannot produce plus the approval shapes
//! that go with it.

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
            // The model and the usage every real assistant message
            // carries: what the header's session-fact line and the
            // context meter read.
            "model": "claude-opus-5",
            "content": content,
            "stop_reason": stop,
            "usage": {
                "input_tokens": 1_240,
                "cache_read_input_tokens": 30_000,
                "cache_creation_input_tokens": 400,
                "output_tokens": 512,
            },
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
/// rows, and this page shows fourteen. The three kinds a Claude thread
/// cannot produce at all — the compaction rule, the unrecognized row and
/// the MCP startup block — are on the Codex page, together with the
/// numbered diff gutter a Claude ask-time diff never has. Every block
/// that is here is here whole: a gallery that scrolls hides exactly what
/// it was built to show.
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

// --- the Codex page ---------------------------------------------------------

/// The unified patch the file-change block paints, with hunk headers so
/// the gutter has real line numbers to show.
const PATCH: &str = "@@ -12,3 +12,4 @@\n     if attempt >= config.max_attempts {\n-        return Err(RetryError::Exhausted);\n+        return Err(RetryError::Exhausted { attempt });\n+        metrics.record(attempt);\n     }\n";

/// One Codex thread written to show the blocks a Claude session cannot
/// produce.
///
/// A Codex thread is where the compaction rule, the unrecognized row and
/// the MCP startup block live, and where a file change carries a real
/// unified patch, so the numbered gutter has numbers to print. The order
/// is a session's order: the compaction rule closes the history that came
/// before, the turn does its work, and the next turn stops on an approval
/// that is docked under everything — an approval whose decisions include
/// a network-policy amendment, which is the only place that shape appears.
///
/// The arithmetic is as tight as the Claude page's: the feed's rows are
/// full, so a block gained here is a block lost off the top. Nothing that
/// is a repeat of a shape the Claude page already shows earns a row —
/// which is why the completed command is the one awaiting approval, and
/// why the reasoning marker carries the only continuation on the page.
pub(super) fn codex_gallery_rows() -> Vec<Value> {
    vec![
        json!({"type": "amux.codex_ready"}),
        json!({"type": "thread/compacted", "turnId": "turn-08"}),
        json!({"type": "turn/started", "turn": {"id": "turn-09", "status": "inProgress"}}),
        json!({"type": "item/completed", "turnId": "turn-09", "item": {
            "id": "user-cap",
            "type": "userMessage",
            "content": [{"type": "text", "text": "Cap the retry backoff."}],
        }}),
        // Two servers, one of them failed: the block states the tally, not
        // the roster, so it stays one row however many servers there are.
        json!({"type": "mcpServer/startupStatus/updated", "threadId": "thread-1",
               "name": "node_repl", "status": "ready"}),
        json!({"type": "mcpServer/startupStatus/updated", "threadId": "thread-1",
               "name": "issues", "status": "failed", "error": "launch failed",
               "failureReason": "process exited"}),
        json!({"type": "item/completed", "item": {
            "id": "reason-cap",
            "type": "reasoning",
            "content": [],
            "summary": ["Where the cap belongs"],
        }}),
        json!({"type": "item/completed", "item": {
            "id": "msg-cap",
            "type": "agentMessage",
            "text": "The cap belongs in `RetryConfig`.",
            "phase": "final_answer",
        }}),
        // The patch arrives as a delta and the completed item names the
        // file: that is the order the wire writes them, and the block only
        // has a diff to paint because the delta came first.
        json!({"type": "item/fileChange/outputDelta", "itemId": "edit-cap", "delta": PATCH}),
        json!({"type": "item/completed", "item": {
            "id": "edit-cap",
            "type": "fileChange",
            "status": "completed",
            "changes": [{"path": "sync/backoff.rs", "kind": {"type": "update"}}],
        }}),
        // A method this build has never seen. Nothing is dropped: the row
        // says what arrived and stays legible.
        json!({"type": "thread/experimental/telemetryPing", "threadId": "thread-1"}),
        json!({"type": "turn/completed", "turn": {"id": "turn-09", "status": "completed"}}),
        json!({"type": "turn/started", "turn": {"id": "turn-10", "status": "inProgress"}}),
        json!({"type": "item/started", "item": {
            "id": "exec-ask",
            "type": "commandExecution",
            "command": "cargo test --workspace",
            "status": "inProgress",
        }}),
        json!({"type": "item/commandExecution/requestApproval", "itemId": "exec-ask",
               "command": "cargo test --workspace",
               "reason": "Run the repository test suite?",
               "proposedNetworkPolicyAmendments": [{"host": "crates.io", "action": "allow"}]}),
        json!({"type": "amux.codex_approval_required", "request_id": "approval-1",
        "availableDecisions": [
            "accept",
            {"applyNetworkPolicyAmendment": {
                "network_policy_amendment": {"host": "crates.io", "action": "allow"}
            }},
            "decline"
        ]}),
    ]
}
