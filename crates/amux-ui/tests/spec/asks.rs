//! Chapter 11 — Asks: extraction, correlation, lifecycle.
//!
//! `docs/CHAT.md` §Asks against the Phase 0 fixtures. An ask is the
//! chat-layer surface of a live obligation: the pending signal is the amux
//! `hook.permission_request` row (FACT, arrival-ordered — it precedes the
//! transcript tail), routed on `tool_name`, never notification wording;
//! the unpaired AskUserQuestion/ExitPlanMode in a final message is the
//! transcript-only signal and fallback. Resolution is the B5 facts: a
//! non-error tool_result (allow / answers / plan approval), the typed
//! denial, the interrupt rows, or the turn-end signals when resolution
//! rows lag. Hook payloads carry NO tool_use id — correlation rides the
//! content identity (the hook's `tool_input` equals the transcript
//! `tool_use.input` byte-for-byte, fixture-verified).

use amux_ui::Msg;
use amux_ui::claude::{Ask, AskKind, AskState, AskWhy, ClaudeLayer, ToolInvocation};
use serde_json::json;

use crate::harness::*;

fn feed_prefix(fixture: &str, end: usize) -> Vec<Msg> {
    chat_feed_prefix("fix-auth-bug", fixture, end)
}

fn the_layer(model: &amux_ui::Model) -> &ClaudeLayer {
    claude_layer(model, "fix-auth-bug")
}

fn head<'m>(model: &'m amux_ui::Model) -> &'m Ask {
    the_layer(model).ask_head().expect("an ask pending")
}

// --- permission (plain tool) -----------------------------------------------

/// The permission fixture's first turn, cut right after the duplicate hook
/// pair: ONE pending permission ask with the Bash payload — hook delivery
/// is at-least-once (§18b: every hook row arrives twice) and ask identity
/// is per request, not per row. The hook has not been correlated to a
/// transcript row yet (the tail lags the hook by design).
#[test]
fn a_permission_hook_extracts_one_pending_ask_despite_duplicate_delivery() {
    let model = fold(feed_prefix("permission", 8));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 1, "duplicate delivery is one ask");
    let ask = head(&model);
    assert_eq!(ask.state, AskState::Pending);
    assert_eq!(ask.why(), AskWhy::Permission);
    assert_eq!(ask.tool_use_id, None, "hook payloads carry no tool_use id");
    let AskKind::Permission {
        tool_name,
        invocation: ToolInvocation::Bash { command, .. },
    } = &ask.kind
    else {
        panic!("a Bash permission: {:?}", ask.kind);
    };
    assert_eq!(tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        command.as_deref(),
        Some("echo allowed-probe > allowed.txt"),
        "the typed per-tool payload is the same extraction tool entries use"
    );
}

/// When the transcript tail catches up, the unpaired `tool_use` in the
/// final message correlates to the hook-born ask by content identity: the
/// ask gains its `toolu_*` resolution key without a second ask appearing.
#[test]
fn the_transcript_tool_use_correlates_the_hook_ask() {
    let model = fold(feed_prefix("permission", 10));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 1);
    assert_eq!(
        head(&model).tool_use_id.as_deref(),
        Some("toolu_01D9384zKG2ozqqE54c59MKP"),
        "the ask now carries the transcript identity"
    );
}

/// Allow ⇔ a non-error tool_result for the correlated id (B5): the ask
/// leaves the queue; the collapsed fact renders from the tool entry.
#[test]
fn a_non_error_result_resolves_the_permission_as_allowed() {
    let model = fold(feed_prefix("permission", 11));
    assert_eq!(the_layer(&model).ask_count(), 0, "resolved, not pending");
}

/// Deny ⇔ `is_error:true` plus `toolDenialKind:"user-rejected"` (B5): the
/// second turn's ask resolves on the typed denial; the interrupt row and
/// turn authority that follow find nothing left to close.
#[test]
fn a_typed_denial_resolves_the_permission_as_denied() {
    let pending = fold(feed_prefix("permission", 21));
    assert_eq!(the_layer(&pending).ask_count(), 1, "second ask pending");
    let denied = fold(feed_prefix("permission", 24));
    assert_eq!(the_layer(&denied).ask_count(), 0, "denial resolved it");
    let closed = fold(chat_feed("fix-auth-bug", "permission"));
    assert_eq!(the_layer(&closed).ask_count(), 0);
}

// --- question ---------------------------------------------------------------

/// `hook.permission_request` fires for AskUserQuestion too (Phase 0):
/// extraction routes on `tool_name` and yields a QUESTION ask carrying the
/// C4 facts — question text, `{label, description}` options.
#[test]
fn an_ask_user_question_hook_extracts_a_question_ask() {
    let model = fold(feed_prefix("question_single", 8));
    let ask = head(&model);
    assert_eq!(ask.why(), AskWhy::Question);
    let AskKind::Question { questions } = &ask.kind else {
        panic!("a question ask: {:?}", ask.kind);
    };
    assert_eq!(questions.len(), 1);
    assert_eq!(
        questions[0].question.as_deref(),
        Some("Which color do you prefer?")
    );
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Red", "Blue"]
    );
}

/// The answers sidecar (keyed by question TEXT, §18a) is the question's
/// resolution fact: the ask leaves the queue when the result row lands.
#[test]
fn the_answers_result_resolves_the_question() {
    let correlated = fold(feed_prefix("question_single", 10));
    assert_eq!(
        head(&correlated).tool_use_id.as_deref(),
        Some("toolu_0193YwXqWKEhBWar328MuBdE")
    );
    let answered = fold(feed_prefix("question_single", 11));
    assert_eq!(the_layer(&answered).ask_count(), 0);
}

/// Multi-select questions extract and resolve identically — the joined
/// answer string is a tool-entry fact, not ask state.
#[test]
fn a_multi_select_question_extracts_and_resolves() {
    let pending = fold(feed_prefix("question_multi", 8));
    let AskKind::Question { questions } = &head(&pending).kind else {
        panic!("a question ask");
    };
    assert!(questions[0].multi_select);
    let answered = fold(chat_feed("fix-auth-bug", "question_multi"));
    assert_eq!(the_layer(&answered).ask_count(), 0);
}

// --- plan review ------------------------------------------------------------

/// ExitPlanMode is a PERMISSION whose invocation is the plan variant —
/// not a third kind (CHAT.md §Vocabulary). The payload carries the full
/// plan for the reader, straight from the hook.
#[test]
fn an_exit_plan_mode_hook_extracts_the_plan_review_variant() {
    let model = fold(feed_prefix("plan_approve", 19));
    let ask = head(&model);
    assert_eq!(ask.why(), AskWhy::Permission);
    let AskKind::Permission {
        tool_name,
        invocation: ToolInvocation::Plan {
            plan: Some(plan), ..
        },
    } = &ask.kind
    else {
        panic!("a plan-review permission: {:?}", ask.kind);
    };
    assert_eq!(tool_name.as_deref(), Some("ExitPlanMode"));
    assert!(plan.starts_with("# Add README.md"));
}

/// Approval ⇔ the non-error ExitPlanMode result (§18a — NO permission-mode
/// row change): the ask resolves and the accepted plan is retained as
/// session state.
#[test]
fn plan_approval_resolves_the_ask_and_retains_the_plan() {
    let model = fold(chat_feed("fix-auth-bug", "plan_approve"));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 0);
    assert_eq!(layer.accepted_plans().len(), 1);
}

/// Rejection (request-changes) resolves the FIRST plan ask without ending
/// the turn — the agent revises and asks AGAIN with a new plan: a second,
/// distinct ask under the same `prompt_id` (fixture-verified). Ask
/// identity is per request content, not per prompt.
#[test]
fn a_rejected_plan_can_be_re_asked_as_a_new_ask() {
    let first_pending = fold(feed_prefix("plan_reject", 25));
    let first = head(&first_pending);
    assert_eq!(first.why(), AskWhy::Permission);

    let first_rejected = fold(feed_prefix("plan_reject", 29));
    assert_eq!(the_layer(&first_rejected).ask_count(), 0);

    let second_pending = fold(feed_prefix("plan_reject", 37));
    let layer = the_layer(&second_pending);
    assert_eq!(layer.ask_count(), 1, "the revised plan is a NEW ask");
    let AskKind::Permission {
        invocation: ToolInvocation::Plan {
            plan: Some(plan), ..
        },
        ..
    } = &head(&second_pending).kind
    else {
        panic!("a plan-review permission");
    };
    assert!(
        plan.contains("Note: I checked the full repo"),
        "the second ask carries the REVISED plan"
    );

    let closed = fold(chat_feed("fix-auth-bug", "plan_reject"));
    assert_eq!(the_layer(&closed).ask_count(), 0);
}

/// The plan-approval notification says "needs your approval" — no
/// "permission" substring (§18b). The notification row folds to NOTHING:
/// the pending ask's kind comes from `hook.permission_request.tool_name`
/// alone, so the wording cannot misclassify it (E2's forbidden ground).
#[test]
fn notification_wording_never_classifies_the_ask() {
    // Prefix through both hook.notification rows (37, 38).
    let model = fold(feed_prefix("plan_reject", 39));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 1);
    assert_eq!(
        head(&model).why(),
        AskWhy::Permission,
        "plan review stays a permission whatever the notification says"
    );
}

// --- authored edges ---------------------------------------------------------

fn permission_hook(command: &str) -> serde_json::Value {
    json!({
        "type": "hook.permission_request",
        "hook_event_name": "PermissionRequest",
        "session_id": "22222222-2222-4222-8222-222222222222",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "permission_mode": "default",
    })
}

fn a_prompt_row() -> serde_json::Value {
    json!({
        "type": "user",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000001",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": "do the thing"},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    })
}

fn ready() -> serde_json::Value {
    json!({"type": "amux.transcript_ready"})
}

/// Multiple pending asks queue in arrival order with an honest head +
/// count (C): two distinct requests are two asks; resolving neither is
/// implied by the other.
#[test]
fn multiple_pending_asks_queue_with_an_honest_count() {
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                ready(),
                a_prompt_row(),
                permission_hook("echo first"),
                permission_hook("echo second"),
            ],
        )],
    ]));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 2, "the honest (1 of N) count");
    let AskKind::Permission {
        invocation: ToolInvocation::Bash { command, .. },
        ..
    } = &layer.ask_head().expect("head").kind
    else {
        panic!("a Bash permission");
    };
    assert_eq!(command.as_deref(), Some("echo first"), "arrival order");
    let ids: Vec<u64> = layer.asks().map(|ask| ask.id).collect();
    assert_eq!(ids, vec![0, 1], "stable monotonic handles");
}

/// A request identical to a PENDING ask is the duplicate delivery and
/// folds to nothing; the same request arriving after resolution is a
/// genuine re-ask and becomes a new ask.
#[test]
fn duplicate_delivery_is_tolerated_and_re_asks_survive_it() {
    let tool_use = json!({
        "type": "assistant",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000002",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:01.000Z",
        "message": {
            "id": "msg_dup", "role": "assistant", "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "id": "toolu_dup", "name": "Bash",
                         "input": {"command": "echo first"}}]
        },
    });
    let denial = json!({
        "type": "user",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000003",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:02.000Z",
        "toolDenialKind": "user-rejected",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_dup", "is_error": true,
             "content": "The user doesn't want to proceed with this tool use."}
        ]},
    });
    // Three identical deliveries while pending → one ask. The dedupe is
    // bounded content identity, so replays and historical double-delivery
    // streams fold identically.
    let pending = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                ready(),
                a_prompt_row(),
                permission_hook("echo first"),
                permission_hook("echo first"),
                permission_hook("echo first"),
            ],
        )],
    ]));
    assert_eq!(the_layer(&pending).ask_count(), 1);

    // Resolve it (denied), then the agent asks the very same thing again
    // in a NEW request row: a new ask. (Authored with distinct row bytes —
    // a re-request row differs in prompt/session context in practice; the
    // bounded dedupe trade-off for byte-identical hook rows is recorded in
    // the fold.)
    let re_ask = json!({
        "type": "hook.permission_request",
        "hook_event_name": "PermissionRequest",
        "session_id": "22222222-2222-4222-8222-222222222222",
        "tool_name": "Bash",
        "tool_input": {"command": "echo first"},
        "permission_mode": "default",
        "prompt_id": "second-request",
    });
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![
            batch(
                "fix-auth-bug",
                10,
                vec![
                    ready(),
                    a_prompt_row(),
                    permission_hook("echo first"),
                    permission_hook("echo first"),
                    tool_use,
                    denial,
                ],
            ),
            batch("fix-auth-bug", 20, vec![re_ask]),
        ],
    ]));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 1, "the re-ask is a new pending ask");
    assert_eq!(layer.ask_head().expect("head").id, 1, "a NEW ask identity");
}

/// Interrupt closes every ask (C5): the user cut the turn instead of
/// answering — the panel dismisses, the interruption fact renders.
#[test]
fn an_interrupt_closes_the_pending_ask() {
    let interrupt = json!({
        "type": "user",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000004",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:03.000Z",
        "message": {"role": "user", "content": [
            {"type": "text", "text": "[Request interrupted by user for tool use]"}
        ]},
    });
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                ready(),
                a_prompt_row(),
                permission_hook("echo first"),
                interrupt,
            ],
        )],
    ]));
    assert_eq!(the_layer(&model).ask_count(), 0, "interrupt closes asks");
}

/// The transcript-only fallback (no hook rows at all — the read-only
/// degradation): an unpaired AskUserQuestion or ExitPlanMode in a FINAL
/// message IS the ask (FACT-grade pairing rule). A plain tool's unpaired
/// use stays "running" — never inferred into a permission ask.
#[test]
fn unpaired_question_and_plan_tool_uses_are_asks_without_hooks() {
    let row = |uuid: &str, name: &str, input: serde_json::Value| {
        json!({
            "type": "assistant",
            "uuid": uuid,
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-11T22:00:01.000Z",
            "message": {
                "id": format!("msg_{name}"), "role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": format!("toolu_{name}"), "name": name,
                             "input": input}]
            },
        })
    };
    let question = row(
        "aaaaaaaa-0000-4000-8000-000000000005",
        "AskUserQuestion",
        json!({"questions": [{"header": "Color", "question": "Which?", "options": [
            {"label": "Red", "description": "warm"}], "multiSelect": false}]}),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![ready(), a_prompt_row(), question],
        )],
    ]));
    let layer = the_layer(&model);
    assert_eq!(layer.ask_count(), 1);
    let ask = layer.ask_head().expect("head");
    assert_eq!(ask.why(), AskWhy::Question);
    assert_eq!(ask.tool_use_id.as_deref(), Some("toolu_AskUserQuestion"));

    let plan = row(
        "aaaaaaaa-0000-4000-8000-000000000006",
        "ExitPlanMode",
        json!({"plan": "# The plan"}),
    );
    let bash = row(
        "aaaaaaaa-0000-4000-8000-000000000007",
        "Bash",
        json!({"command": "sleep 60"}),
    );
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![ready(), a_prompt_row(), plan, bash],
        )],
    ]));
    let layer = the_layer(&model);
    assert_eq!(
        layer.ask_count(),
        1,
        "the plan is an ask; the unpaired Bash is running, not an ask"
    );
    assert_eq!(layer.ask_head().expect("head").why(), AskWhy::Permission);
}

/// Evicting content never evicts asks (B9): a pending ask survives the
/// feed window being flooded past its bound, and the late resolution
/// still lands — the obligation outlives the bytes. (The flood is
/// unstated-source rows: a HUMAN prompt would legitimately close the ask
/// as stale — a new human turn implies nothing was blocking.)
#[test]
fn eviction_never_evicts_a_pending_ask() {
    let flood: Vec<serde_json::Value> = (0..1200)
        .map(|n| {
            json!({
                "type": "user",
                "uuid": format!("bbbbbbbb-0000-4000-8000-{n:012}"),
                "sessionId": "22222222-2222-4222-8222-222222222222",
                "timestamp": "2026-08-11T22:10:00.000Z",
                "message": {"role": "user", "content": format!("filler {n}")},
            })
        })
        .collect();
    // The ask pends, the feed floods far past retention, then the
    // correlating tool_use and its resolution arrive late.
    let tool_use = json!({
        "type": "assistant",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000008",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:20:00.000Z",
        "message": {
            "id": "msg_late", "role": "assistant", "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "id": "toolu_late", "name": "Bash",
                         "input": {"command": "echo first"}}]
        },
    });
    let result = json!({
        "type": "user",
        "uuid": "aaaaaaaa-0000-4000-8000-000000000009",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:20:01.000Z",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_late", "content": "ok"}
        ]},
    });

    let pending = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            [
                vec![ready(), a_prompt_row(), permission_hook("echo first")],
                flood.clone(),
            ]
            .concat(),
        )],
    ]));
    let layer = the_layer(&pending);
    assert!(layer.evicted_entries() > 0, "the flood evicted content");
    assert_eq!(layer.ask_count(), 1, "the obligation survived the window");

    let resolved = fold(seq([
        chat_base("fix-auth-bug"),
        vec![
            batch(
                "fix-auth-bug",
                10,
                [
                    vec![ready(), a_prompt_row(), permission_hook("echo first")],
                    flood,
                ]
                .concat(),
            ),
            batch("fix-auth-bug", 20, vec![tool_use, result]),
        ],
    ]));
    assert_eq!(
        the_layer(&resolved).ask_count(),
        0,
        "the late resolution still resolves the evicted-era ask"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("asks::permission_pending", feed_prefix("permission", 8)),
        ("asks::permission_correlated", feed_prefix("permission", 10)),
        ("asks::question_pending", feed_prefix("question_single", 8)),
        ("asks::plan_pending", feed_prefix("plan_approve", 19)),
        ("asks::plan_re_ask", feed_prefix("plan_reject", 39)),
        (
            "asks::queued",
            seq([
                chat_base("fix-auth-bug"),
                vec![batch(
                    "fix-auth-bug",
                    10,
                    vec![
                        ready(),
                        a_prompt_row(),
                        permission_hook("echo first"),
                        permission_hook("echo second"),
                    ],
                )],
            ]),
        ),
    ]
}
