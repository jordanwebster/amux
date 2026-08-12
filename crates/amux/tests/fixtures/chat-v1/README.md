# chat-v1 baseline transcript fixtures

Redacted, provenance-stamped captures of the `claude_pty_transcript_v1`
structured stream, produced by the Phase 0 capture harness
(`crates/amux/tests/capture/`) driving a **real** `claude` through a real,
isolated amux daemon. These are the seed of the CHAT.md §H suite and the
Tier-1 input for Phase 1's Claude-layer fold.

## What each scenario is

| file | scenario | key rows it locks in |
|---|---|---|
| `pong.rows.jsonl` | H.1 prompt round trip | user prompt, `hook.stop` arriving before the transcript tail caught up (the capture window closed at the hook — no assistant rows, no `turn_duration`; the arrival-ordering consequence of transcript-semantics §1, demonstrated) |
| `tools.rows.jsonl` | Edit + Bash tool use | `toolUseResult` Edit `structuredPatch`, Bash `{stdout,…}` |
| `permission.rows.jsonl` | permission allow + deny | `hook.permission_request`, deny `toolDenialKind:"user-rejected"` |
| `question_single.rows.jsonl` | AskUserQuestion single-select | `toolUseResult.{questions,answers}` (answers keyed by question text — see drift note) |
| `question_multi.rows.jsonl` | AskUserQuestion multi-select + Other | `multiSelect:true` question; `answers` join `"Hammer, Saw, a torque wrench "` proves both predefined selections *and* the Other free-text landed |
| `interrupt.rows.jsonl` | Esc interrupt mid-turn | null-`stop_reason` flush + `[Request interrupted by user]` + `interruptedMessageId` |
| `plan_approve.rows.jsonl` | ExitPlanMode approve (manual) | ExitPlanMode `tool_use` `{plan,planFilePath}`, approval `tool_result` |
| `plan_reject.rows.jsonl` | ExitPlanMode request-changes | rejection `tool_result` `is_error:true` |
| `compact.rows.jsonl` | `/compact` | `system/compact_boundary` + `isCompactSummary` summary row |

Phase 3 added the encoding-verification set (claude 2.1.228, haiku; the
C6 keystroke tables in `amux-ui/src/claude/encoding.rs` cite these runs):

| file | scenario | key rows it locks in |
|---|---|---|
| `permission_session.rows.jsonl` | permission menu digit 2 | the menu's middle option comes from the hook's `permission_suggestions` (`addDirectories`, destination `session`); digit 2 allows AND applies it — **no** `command_permissions` attachment for a directory-scope grant, and a different Bash command re-asks |
| `permission_deny_feedback.rows.jsonl` | permission menu digit 3 | digit 3 (`No`) denies IMMEDIATELY — typed denial + `[Request interrupted by user for tool use]` + `turn_duration`, **no feedback field** (unlike the plan menu); the feedback composition is a follow-up prompt |
| `question_tabs.rows.jsonl` | two single-select questions | a digit SELECTS the option and auto-advances to the next question tab; the last answer lands on the review step (`1. Submit answers` preselected) and one Enter submits; answers keyed by both question texts |
| `question_other_single.rows.jsonl` | Other on single-select | digit (options+1) opens the inline editor; type + Enter commits — a single-question form submits on selection, no review step (C4's mandatory-submit rule matches claude's own behavior) |
| `question_mixed.rows.jsonl` | multi-select + single-select in one form | Space toggles, Tab advances from the multi-select question to the NEXT question tab, digit auto-advances the single-select, Enter confirms the review |
| `plan_auto.rows.jsonl` | ExitPlanMode approve — auto (H.5) | digit 1 approves; edits then land with NO further asks; the `permission-mode` row does **not** flip (stays `plan`) while `hook.stop`'s `permission_mode` says `acceptEdits` — the hook payload is the effective-mode fact |
| `mode_cycle.rows.jsonl` | Shift+Tab mode cycling (D4) | CSI Z cycles default → acceptEdits → plan → default; **zero** `permission-mode` rows across three presses and two turns — the hook payloads' `permission_mode` is the only prompt fact source |
| `prompt_multiline.rows.jsonl` | bracketed-paste multiline prompt | `ESC[200~ … ESC[201~` + Enter lands ONE user row whose string content is byte-identical to the sent text, newline included (B1's echo-correlation evidence) |

Each `<scenario>.meta.json` sidecar carries the provenance stamp: capture
date, `claude --version`, model, the harness invocation, whether the daemon
env was poisoned (it is, by default — see below), and the exact keystroke
program that drove the run.

## Provenance

- Captured Claude Code **2.1.228**, model **haiku** (haiku 4.5) for every
  scenario; see each `.meta.json` for the exact date/model.
- Every capture ran against a daemon whose environment was **deliberately
  poisoned** with the Claude Code child-session marker set
  (`CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDECODE=1`, …). That these fixtures
  contain transcript rows at all is the live proof of the Phase 0 spawn-seam
  fix: without the scrub + force-persistence, the poisoned daemon would make
  claude suppress transcript persistence and every capture would be empty.

## Redaction

Claude Code runs under the *real* user profile (profile isolation breaks
auth on macOS — the keychain read is config-dir-gated, verified empirically),
so `redact.rs` does two jobs:

1. **Drops config-bearing attachment rows whole** — `skill_listing`,
   `agent_listing_delta`, `deferred_tools_delta`. These carry the owner's
   *own* installed skills (names + full descriptions), configured subagents,
   and MCP/connector tool inventory (Gmail/Calendar/etc.), none of which is
   scenario structure. (This is why row counts are lower than the live
   stream.) Tool-inventory *counts* (`total_deferred_tools`) are zeroed.
2. **Scrubs identifying substrings** from what remains: scratch root
   (`[SCRATCH]`/`[SCRATCH-SLUG]`), home dir (`[HOME]`/`[HOME-SLUG]`),
   username token (`[USER]`), hostname (`[HOST]`).

Then it *fails the run* if any config-bearing row, tool/skill-inventory
marker, non-zero inventory count, home dir, username token, or
credential-shaped substring survives. No real prompts beyond the scripted
ones, no absolute paths, no credentials, no user config.

## Regenerating

```
cargo build -p amux-cli
AMUX_CAPTURE_OUT=target/capture timeout 600 \
    cargo test -p amux --test capture -- <scenario>
```

See `crates/amux/tests/capture/main.rs` for the scenario list and env knobs.
The harness is opt-in (no scenarios named ⇒ it skips), never part of default
CI.
