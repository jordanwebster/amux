# Claude Transcript Redaction Spec

Status: proposed for immediate implementation

Scope: applies to Claude transcript JSONL rows at ingest time in amux, before a row is written to the structured buffer.

Primary goal: cut transcript size substantially without introducing transcript-state reconstruction, tool correlation maps, or size-threshold heuristics.

Non-goals:
- Backwards compatibility with the current raw transcript wire format
- Perfect preservation of all raw Claude transcript/debug details
- Tool-name-specific logic such as "if `mcp__...` then ..."

## Design Principles

1. Redaction is row-local and schema-based.
2. v1 does not inspect prior transcript rows and does not maintain `tool_use_id -> tool name` state.
3. Prefer dropping rows or fields over introducing new sentinel schemas.
4. In v1, a redacted field is removed entirely.
5. In v1, a dropped row is omitted entirely.
6. No size-based row dropping in v1.

## Processing Order

For each parsed JSONL row:

1. Parse the row into `serde_json::Value`.
2. Apply row-drop rules.
3. Apply recursive field-drop rules to the remaining row.
4. If the row still exists, write it to the structured buffer.

Important: recursive field-drop rules apply both to top-level rows and to nested transcript-like payloads inside `progress.data.message`.

## Rows To Drop Completely

These rows currently carry no user-visible semantics in the app and should not be emitted at all.

| Match | Action | Notes |
|---|---|---|
| `type == "file-history-snapshot"` | Drop row | Explicit no-op in app normalizer |
| `type == "queue-operation"` | Drop row | Explicit no-op in app normalizer |
| `type == "attachment"` | Drop row | Current app ignores attachment rows |
| `type == "last-prompt"` | Drop row | Current app ignores this row type |
| `type == "progress"` and `data.type == "hook_progress"` | Drop row | Current app treats as metadata-only/no-op |
| `type == "progress"` and `data.type == "bash_progress"` | Drop row | Current app treats as metadata-only/no-op |
| `type == "progress"` and `data.type == "mcp_progress"` | Drop row | Current app treats as metadata-only/no-op |
| `type == "progress"` and `data.type == "query_update"` | Drop row | Current app treats as metadata-only/no-op |
| `type == "progress"` and `data.type == "search_results_received"` | Drop row | Current app treats as metadata-only/no-op |

## Rows To Keep

These row types remain semantically important and must still be emitted after field redaction:

- `assistant`
- `user`
- `system`
- `progress` where `data.type == "agent_progress"`
- `progress` where `data.type == "waiting_for_task"`
- `agent-name`
- `custom-title`
- `permission-mode`

## Global Top-Level Metadata Fields To Drop

These fields are repetitive transport/debug metadata and are not currently required by the app's transcript semantics.

Apply to every row type when present:

| Field | Action |
|---|---|
| `sessionId` | Drop field |
| `userType` | Drop field |
| `version` | Drop field |
| `entrypoint` | Drop field |
| `isSidechain` | Drop field |

Do not drop these fields:

- `uuid`
- `parentUuid`
- `promptId`
- `requestId`
- `sourceToolAssistantUUID`
- `toolUseID`
- `parentToolUseID`
- `cwd`
- `gitBranch`
- `slug`
- `permissionMode`

## Recursive Field Redaction Rules

These rules apply anywhere in the row tree, including nested rows under `progress.data.message`.

### 1. Base64/blob payloads

If an object contains:

- `source.type == "base64"`
- `source.data` is present

then:

- drop `source.data`

This applies to:

- screenshot/image tool results
- user image blocks
- nested image blocks inside `progress.data.message`

Keep:

- `source.type`
- `source.media_type`

### 2. Thinking signatures

If an assistant content block has:

- `type == "thinking"` or `type == "redacted_thinking"`
- `signature` is present

then:

- drop `signature`

Keep the block itself. The presence of the thinking block still signals that thinking occurred.

### 3. Edit tool full-file snapshots

If `toolUseResult.originalFile` is present:

- drop `toolUseResult.originalFile`

Reason: this is usually a full pre-edit file snapshot and is one of the largest repeated payloads.

### 4. Text/image file payloads inside `toolUseResult`

If `toolUseResult.file` is an object:

- drop `toolUseResult.file.content` when present
- drop `toolUseResult.file.base64` when present

Keep:

- `toolUseResult.file.filePath`
- `toolUseResult.file.numLines`
- `toolUseResult.file.startLine`
- `toolUseResult.file.totalLines`
- any other non-payload metadata

### 5. Create/update full file contents inside `toolUseResult`

If `toolUseResult.type` is one of:

- `"create"`
- `"update"`

and `toolUseResult.content` is present, then:

- drop `toolUseResult.content`

Keep:

- `toolUseResult.type`
- `toolUseResult.filePath`
- `toolUseResult.structuredPatch`
- `toolUseResult.originalFile` is already removed by rule 3

### 6. Redundant agent-progress normalization payloads

If a row matches:

- `type == "progress"`
- `data.type == "agent_progress"`

then:

- drop `data.normalizedMessages` when present

Do not drop:

- `data.message`
- `data.agentId`
- `data.prompt`
- `data.parentToolUseID`

Reason: the app currently reconstructs subagent transcript semantics from `data.message`, but `data.normalizedMessages` is redundant.

## Explicit Preserve Rules

The following fields must be preserved in v1 even if they can be large:

- `message.id`
- `message.usage`
- `toolUseResult.structuredPatch`
- `toolUseResult.oldString`
- `toolUseResult.newString`
- `toolUseResult.stdout`
- `toolUseResult.stderr`
- `toolUseResult.result`
- `progress.data.message` for `agent_progress`
- all fields needed for `waiting_for_task`

Reason: these still participate in current app semantics or are likely to be useful in the near term, and removing them would move v1 from "redaction" into a larger protocol redesign.

## Explicit Non-Rules For V1

Do not do any of the following in v1:

- Do not drop rows just because they are large.
- Do not drop all `mcp__*` tool results.
- Do not require `tool_use_id` correlation state to determine redaction behavior.
- Do not replace removed payloads with sentinel strings such as `"[redacted]"`.
- Do not add hashes, byte counts, or replacement summary objects.

Those can be added later in a v2 compaction/redaction format if needed.

## Examples

### Example: screenshot tool result

Before:

```json
{
  "type": "image",
  "source": {
    "data": "... huge base64 ...",
    "media_type": "image/png",
    "type": "base64"
  }
}
```

After:

```json
{
  "type": "image",
  "source": {
    "media_type": "image/png",
    "type": "base64"
  }
}
```

### Example: edit tool result

Before:

```json
{
  "toolUseResult": {
    "filePath": "/repo/src/app.ts",
    "oldString": "old snippet",
    "newString": "new snippet",
    "originalFile": "... entire file ...",
    "structuredPatch": [...]
  }
}
```

After:

```json
{
  "toolUseResult": {
    "filePath": "/repo/src/app.ts",
    "oldString": "old snippet",
    "newString": "new snippet",
    "structuredPatch": [...]
  }
}
```

### Example: agent progress

Before:

```json
{
  "type": "progress",
  "data": {
    "type": "agent_progress",
    "agentId": "agent-1",
    "prompt": "Explore the codebase",
    "message": { "... nested transcript row ..." },
    "normalizedMessages": [ "... redundant ..." ]
  }
}
```

After:

```json
{
  "type": "progress",
  "data": {
    "type": "agent_progress",
    "agentId": "agent-1",
    "prompt": "Explore the codebase",
    "message": { "... nested transcript row, recursively redacted ..." }
  }
}
```

## Expected Outcome

This spec should deliver most of the easy savings while keeping the current app semantics intact:

- eliminate base64/blob bloat
- eliminate duplicate full-file snapshots
- eliminate known no-op transcript row families
- preserve edit/tool semantics
- preserve subagent progress
- avoid any need for transcript state tracking in the server
