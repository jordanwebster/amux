# Claude Transcript Redaction Feasibility

Status: assessment of the proposed spec in [transcript-redaction-spec.md](./transcript-redaction-spec.md)

## Executive Summary

The proposal in [transcript-redaction-spec.md](./transcript-redaction-spec.md) is feasible with low-to-moderate implementation effort.

The main reason is architectural: Claude transcript structured output already enters amux through a narrow ingest path, is represented as `serde_json::Value`, and is transported as opaque JSON. That makes the proposed design a good fit for the current codebase:

1. parse each JSONL row
2. drop rows that match simple schema rules
3. recursively remove fields from kept rows
4. write the resulting row into the structured buffer

This can be implemented without adding transcript history reconstruction, tool correlation state, size heuristics, or a new typed output model.

## Why This Fits The Current Architecture

### 1. Transcript ingest is already centralized

Claude transcript rows are parsed and written in one place:

- `crates/amux/src/claude/transcript.rs`

`tail_transcript()` reads each JSONL line, parses it into `serde_json::Value`, and writes it directly to the structured buffer. There is no semantic parser in the middle anymore, so a redaction layer can be inserted directly in this path before `buffer.write(...)`.

### 2. Structured output is already opaque JSON

The structured buffer and wire messages already treat Claude payloads as raw JSON values:

- `crates/amux/src/buffer.rs`
- `crates/amux/src/message.rs`

That means the proposed recursive field-removal logic does not fight the current transport model. It matches it.

### 3. The spec avoids the hard classes of change

The spec in [transcript-redaction-spec.md](./transcript-redaction-spec.md) is intentionally constrained:

- row-local only
- schema-based only
- no cross-row state
- no `toolUseID -> tool name` correlation
- no size-threshold heuristics
- no sentinel replacement schema

Those constraints keep the implementation simple. The work is mostly a JSON tree walker plus a small set of top-level row filters.

## Expected Implementation Shape

The current code suggests a straightforward implementation:

1. Add a small transcript-redaction module under `crates/amux/src/claude/`.
2. Give it one entry point that accepts a parsed `serde_json::Value`.
3. Return either:
   - `None` for dropped rows, or
   - `Some(redacted_value)` for kept rows.
4. Call that logic from `tail_transcript()` before `buffer.write(...)`.

The redactor would need two kinds of logic:

### Row-drop rules

These are simple top-level matches on:

- `type`
- `type == "progress"` plus `data.type`

The spec's row-drop section is operationally simple and does not require recursion or state.

### Recursive field-drop rules

These require a walk over nested `Value::Object` and `Value::Array` nodes. The walker needs limited local context so it can apply rules such as:

- remove `source.data` when `source.type == "base64"`
- remove `signature` from `thinking` and `redacted_thinking` blocks
- remove `toolUseResult.originalFile`
- remove `toolUseResult.file.content`
- remove `toolUseResult.file.base64`
- remove `toolUseResult.content` for `create` and `update`
- remove `data.normalizedMessages` from `progress.data.type == "agent_progress"`

That is more than a flat path matcher, but it is still ordinary JSON-tree traversal rather than protocol redesign.

## Scope Boundaries That Make It Safe

The proposal is most feasible if it stays within the scope stated by the spec:

- Claude transcript JSONL rows only
- ingest-time only
- before write into the structured buffer

This is important because amux also writes hook-originated structured events through a separate path:

- `crates/amux/src/agents/claude.rs`
- `crates/amux/src/claude/structured_log_source.rs`

Those hook payloads are not transcript rows. Applying the transcript redactor globally at `StructuredLogSource::write()` would broaden the scope beyond the spec and create avoidable risk.

## Main Risks

The implementation itself is not the risky part. The main risks are specification correctness and contract cleanup.

### 1. The current transport is documented as raw/lossless passthrough

Recent code and notes explicitly describe the Claude structured output path as opaque/lossless passthrough:

- `crates/amux/src/claude/transcript.rs`
- `crates/amux/src/buffer.rs`
- `DEVLOG.md`

That does not block the change, but it means docs and tests should be updated to match the new behavior.

### 2. Some row-drop claims are app-dependent

The spec says several row families are current no-ops, for example:

- `attachment`
- `last-prompt`
- progress rows such as `hook_progress`, `bash_progress`, `mcp_progress`, `query_update`, `search_results_received`

That may be correct, but the server repo alone does not prove all of those assumptions. The highest-risk part of the spec is not field redaction; it is incorrectly dropping a row family that a client still needs.

There is some supporting historical evidence for at least part of the list:

- older notes already treated `file-history-snapshot` and `queue-operation` as parsed-but-not-emitted bookkeeping rows in `DEVLOG.md`

But the broader row-drop set should still be validated against real transcript samples and the current app normalizer.

### 3. Recursive nested handling needs care

The spec explicitly says redaction rules also apply to nested transcript-like payloads inside `progress.data.message`.

That is still feasible, but it means the walker cannot be purely top-level. It must recurse carefully and avoid deleting the container fields that the app still needs, especially:

- `progress.data.message`
- fields needed by `waiting_for_task`

### 4. Existing passthrough-oriented tests will need to change

Current transcript tests in `crates/amux/src/claude/transcript.rs` assert passthrough behavior and preservation of all fields. Those tests will need to be rewritten around redaction semantics.

## Difficulty Assessment By Area

### Low difficulty

- row dropping by `type`
- row dropping by `type == "progress"` plus `data.type`
- top-level metadata field removal
- recursive removal of `toolUseResult.originalFile`
- recursive removal of `toolUseResult.file.content`
- recursive removal of `toolUseResult.file.base64`
- recursive removal of `data.normalizedMessages` for `agent_progress`

### Moderate difficulty

- implementing the recursive walker cleanly without over-redacting
- handling nested transcript-like payloads inside `progress.data.message`
- keeping rule ordering predictable and testable
- updating tests from passthrough expectations to redaction expectations

### Not required by this spec, and therefore intentionally avoided

- transcript compaction
- size heuristics
- per-tool correlation state
- replacement sentinel objects
- protocol versioning

## Recommendation

The spec in [transcript-redaction-spec.md](./transcript-redaction-spec.md) is a practical fit for the current architecture and should be straightforward to implement if kept strictly scoped to transcript-row ingest.

The recommended rollout shape is:

1. implement the redactor only in the transcript tailer path
2. add focused unit tests for each row-drop and field-drop rule
3. update existing passthrough-oriented transcript tests
4. validate the proposed dropped row families against real Claude transcript samples and current client behavior

## Overall Verdict

Feasible: yes.

Implementation complexity: low to moderate.

Primary uncertainty: whether every proposed dropped row family is truly unused by the current client.

Primary engineering risk: accidental over-redaction of nested `progress.data.message` payloads if recursion rules are implemented too broadly.
