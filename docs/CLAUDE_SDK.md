# The Claude SDK chat

Status: design record, normative for the SDK-backed Claude chat; the surfaces
below are the contract the client layer and the chat screen are built against.
Companions: `docs/CHAT.md` owns the shared chat shell — frame, feed blocks,
reader, review page, ask lifecycle, keybinding tiers and themes — and is not
restated here; `docs/UI.md` owns the client-layer doctrine (one layer per agent
kind, one classification projected, facts before inference); `docs/CODEX.md`
owns the Codex chat, which this one deliberately resembles; and
`docs/CLAUDE_TRANSCRIPT.md` owns the transcript facts the other Claude chat
folds. Where prose and a passing specification disagree, the specification wins.

Every wire shape quoted below is taken from recordings of the installed Claude
Code 2.1.260 under `crates/claude/fixtures/sdk/` — `question_asked`,
`plan_reviewed`, `elicitation_accepted` — or from the older corpus recorded at
2.1.247/2.1.251. The one surface with no recording behind it is the user
dialog, and every claim about it is marked as unrecorded where it appears.

## What an SDK-backed Claude agent is

The same Claude Code the PTY driver runs, launched as a stream-JSON process the
daemon speaks to directly instead of a terminal it scrapes. Two consequences
shape everything here.

**The session states facts the transcript only implies.** Turn boundaries,
permission mode, model, token usage, task lifecycle and MCP server health all
arrive as rows rather than as inferences from a burst-written file. The SDK
chat therefore renders far fewer `INFERRED` values than the PTY chat, and it
never guesses at liveness.

**Requests come to us instead of to a terminal UI.** Permission, question, plan,
MCP elicitation and user-dialog requests all arrive as `control_request` frames
that block the session until answered. There is no terminal underneath where a
person could answer them in band, so an unanswerable request is a dead session,
not an inconvenience. That is why all five reach the screen.

There is no raw attach for an SDK agent — the process has no terminal UI to
attach to — so this chat is the only way in, on every host.

## Vocabulary

`docs/CHAT.md` owns chat, feed, entry, composer, ask, phase and reader. The SDK
chat adds five terms and widens one.

- **Ask** widens from two kinds to five: **permission**, **question**, **plan
  review**, **elicitation** and **dialog**. As in the PTY chat, question and
  plan review are carried by the provider's permission channel; unlike the PTY
  chat, elicitation and dialog are separate channels with their own request
  ids and their own answer shapes.
- **Session facts** — the model, permission mode, context meter and MCP server
  list the session states about itself. Facts, never inferred; a fact the
  session has not stated is shown as unknown rather than filled in.
- **Streaming message** — an assistant message that is open: it begins on
  `message_start`, grows by content deltas, and is replaced by the final
  `assistant` row under the same message id. A real state, not an animation.
- **Task** — a subagent run the session reports through its own lifecycle rows,
  from `task_started` to `task_notification`, keyed by task id.
- **Context meter** — used tokens against the model's context window, read
  passively from rows that arrive anyway.
- **Context breakdown** — the per-category accounting behind the meter, fetched
  only when a person asks for it, because fetching costs a round trip.

## The header

The chat header is the shared one: `name · kind @ host` on the left, phase on
the right. The SDK chat fills the second header line with session facts, in the
place the Codex chat already uses for its model, approval and sandbox line:

```
  fix-auth · claude @ mbp                                                                                chat · working
  sonnet-4-5 · default · ctx 34k/200k · 2 tasks
```

Every field is a stated fact. The model and permission mode come from
`system.init` and from `system.status` rows when they change mid-session; the
meter is derived as below; the task count is the number of tasks not yet
finished. A field the session has not stated is omitted rather than guessed —
an empty region is honest, a `?` is noise, and an invented default is a lie.

Nothing on this line names the backend. `claude @ mbp` is what a PTY agent
shows too; the second line reads as ordinary session detail, and a PTY chat
that learns to show its own model prints the same shape from its own facts.

## The streaming block

An open assistant message paints as an ordinary assistant block with a dim
caret at the growth point:

```
▎   now make the retry count configurable

    The cap belongs in RetryConfig; I'll thread it through SyncOptions and
    default it to six attempts▌

  ◐ working · 6s · ctrl+x interrupt
```

Three rules keep it from lying.

1. **The final row wins.** When the `assistant` row lands it replaces the open
   message by message id — same block, same position, final text. Deltas are
   never appended after that, so a reconnect mid-message cannot double-write.
2. **Deltas are coalesced before recording.** Per `docs/UI.md`, high-rate
   streams batch into one Msg before they are recorded, so replay reproduces the
   same screens without reproducing arrival timing.
3. **An interrupted message stays interrupted.** If the turn ends without a
   final row, the block keeps the text it has and gains the interrupted marker.
   It is never left mid-stream with a live caret.

Thinking blocks stream the same way and collapse to the existing thinking
marker when the message completes.

## Tasks

Subagent runs get one block per task, in the feed where the task started, and a
count in the header line. The block updates in place:

```
  ⣾ Explore · scan the sync client for retry paths · running · Read sync/client.rs
  ✔ general-purpose · draft the migration plan · done · 3 tools · 42s
```

State comes from the task rows, not from the tool stream: `task_started` opens
the block with its description and subagent type, `task_updated` and
`task_progress` move it, `task_notification` closes it with the summary the
subagent reported. A task whose lifecycle stops arriving stays in its last
stated state and is not aged into a guess.

Nested timelines — what a subagent did, step by step — are out of scope. The
list reports what the session sends about its children, and nothing is read out
of child transcript files.

## Context

**The meter is passive.** Used tokens are the newest assistant message's
`input_tokens + cache_read_input_tokens + cache_creation_input_tokens` — the
size of the context that call actually saw. The window is `contextWindow` from
the result row's `modelUsage`, retained across the turn once seen. Neither
costs a control call; both arrive on every ordinary turn. The meter records
which row produced it, so a client can say `ctx 34k` without a window rather
than inventing a denominator. Compaction replaces the count with the reported
post-compaction tokens. An absent count or a conversation reset makes the
meter unknown until new usage arrives. Without any usage, the meter stays
unknown. The daemon never requests context usage automatically.

**The breakdown is deliberate.** `<leader> c` issues one `get_context_usage`
call and opens an overlay on its answer:

```
    context · 34,102 of 200,000 tokens                                                                        esc close

      system prompt        2,410
      tools               11,884
      mcp tools            4,205
      messages            14,903
      memory files           700

    fetched just now · leader-c refresh
```

The overlay states when it was fetched, because it is a snapshot and the meter
behind it keeps moving. Nothing refetches it on a timer: a per-turn round trip
buys a person a number they look at occasionally.

## MCP status

`system.init` names the configured MCP servers and their connection state, and
later rows update them. The chat shows one compact line when anything is not
ready — `mcp · 3 ready · 1 failed (github)` — and nothing at all when every
server is ready, which is the ordinary case. This is the Codex chat's startup
aggregation rule (`docs/CODEX.md`, D10) applied to Claude's own facts: one line
with a count, never one row per server.

## Asks

All five kinds share the lifecycle, the docked position, the numbered-list
idiom and the Esc rule that `docs/CHAT.md` §C already specifies: an ask takes
over the composer area, Esc steps back a stage but never answers, and answers
are optimistic until the session confirms them. What follows is what each kind
carries and how its answer is encoded.

The id is the provider's `request_id` in every case — the SDK gives us one, so
none of the PTY chat's content-identity correlation is needed here. Multiple
pending asks queue and the head shows `(1 of N)`.

### Permission

`control_request` with `subtype: "can_use_tool"`, carrying `tool_name`,
`display_name`, the tool `input`, and `permission_suggestions` when the CLI has
any. Answered `{"behavior": "allow"}` — optionally with `updatedInput` or
`updatedPermissions` — or `{"behavior": "deny", "message": …}`.

```
    permission — Edit sync/config.rs (+4 -1) · 1 of 2

      pub struct RetryConfig {
     -    pub max_attempts: u8,
     +    pub max_attempts: u8,        // capped at 6
     +    pub jitter_ms: u16,
      ⋮ +1 more lines · f full document

    › 1. Allow once
      2. Always allow Edit in this project
      3. Deny — tell the agent why (optional)
    1-3/↑↓ select · enter confirm · f open document · esc back (never answers)
```

Option 2 is generated from the request's own `permission_suggestions`, one
option per suggestion, exactly as in the PTY chat; a request with no
suggestions shows only allow-once and deny. Deny carries the typed feedback as
`message` — unlike the PTY path, which must compose a deny keystroke and a
follow-up prompt, the SDK takes the sentence directly.

MCP tools arrive here too, named `mcp__<server>__<tool>` with a human
`display_name`, and their suggestion is an `addRules` entry for the tool
(recorded in `elicitation_accepted`).

### Question

`AskUserQuestion` arrives as a permission request — not as a dialog — with
`requires_user_interaction: true` and `input.questions[]`, each carrying
`header`, `question`, `multiSelect` and `options[{label, description}]`. It is
answered `allow` with `updatedInput` echoing the questions plus an `answers`
object keyed by question text (recorded in `question_asked`).

```
    questions

      [storage*] [rollout] [submit]
      Which stores should the migration cover? (select all that apply)

      1. [x] trust store       pairing + relay trust records
      2. [ ] session index     bounded tail metadata
    › 3. [x] recorder dumps    panic-hook recordings
      4. [ ] Other…            type your own answer
    1-4/↑↓ select · space toggle · tab next question · enter advance · esc back (never answers)
```

The panel is the PTY chat's question panel unchanged — the tab row, the
`Other…` free-text option and the mandatory submit tab for multi-question or
multi-select forms. It is the same tool from the same provider; only the
transport differs.

### Plan review

`ExitPlanMode` also arrives as a permission request, with `input.plan` and the
CLI's `planFilePath`. The plan opens the reader fullscreen, as in the PTY chat:

```
    plan · change the retry cap                                                                            1 of 3 pages

    # Plan: make the retry count configurable
    ...
    ## Verification
    cargo test -p amux-sync

    › 1. Approve — auto       agent proceeds, edits apply without asking
      2. Approve — manual     agent asks before each edit
      3. Request changes      feedback required
    1-3/↑↓ select · enter confirm · ↑↓ scroll · esc dock panel (never answers)
```

The three actions encode as:

| Action | Response |
| --- | --- |
| Approve — auto | `allow` with `updatedInput{plan, planFilePath}` and `updatedPermissions: [{"type":"setMode","destination":"session","mode":"acceptEdits"}]` |
| Approve — manual | `allow` with `updatedInput`, plus an explicit session `setMode` back to `default` |
| Request changes | `deny` with the typed feedback as `message` and `interrupt: false` |

The explicit mode in approve-manual is load-bearing and was learned from the
recording: at 2.1.260 a **bare** allow on `ExitPlanMode` is followed by a
`system.status` row with `permissionMode: "acceptEdits"`, so the CLI leaves plan
mode into accept-edits on its own. Approving manually therefore has to say so;
sending nothing would silently give the agent the auto behaviour under the
manual label. The auto path's explicit `setMode` is recorded and confirmed
(`plan_reviewed`); the manual path's `setMode default` is this design's answer
to that finding and is not yet recorded — the first recording that exercises it
settles whether one call suffices or the mode must also be re-sent after the
turn.

Request-changes does not end the turn: the agent may revise and re-ask, which
is a new ask with a new request id.

### Elicitation

An MCP server asking its own question of the user. `control_request` with
`subtype: "elicitation"`, carrying `mcp_server_name`, a `message`, a `mode`
(`"form"` in the recording) and a `requested_schema` — an ordinary JSON Schema
object with `properties` and `required`. Answered
`{"action": "accept", "content": {…}}`, `{"action": "decline"}` or
`{"action": "cancel"}`.

```
    external asks · 1 of 1

    Confirm the word PELICAN.

    › confirmed   PELICAN▌
      required · text

    › 1. Send
      2. Decline — the server is told you declined
      3. Cancel
    tab next field · enter confirm · esc back (never answers)
```

The form is derived from the schema, not authored: one field per property, in
schema order, typed as text, number, boolean or a choice list from `enum`,
with `required` marking the fields that must be filled before Send is offered.
`description` renders as the dim helper line and `default` prefills.

A schema this cannot express — nested objects, arrays, `oneOf` — renders the
panel **blocked**, naming the reason and offering only Decline and Cancel. That
is Codex's rule for unanswerable obligations (`docs/CODEX.md`), and it is the
right one: a half-answer that looks like an answer is worse than a stated
limit. Blocked schemas join the gap list below.

Declining is a person's answer and travels as `decline`; the daemon never
answers an elicitation on its own, which is what today's placeholder behaviour
does and why it is being removed.

### Dialog

`control_request` with `subtype: "request_user_dialog"`, carrying an open-string
`dialog_kind`, an opaque `payload` and an optional `tool_use_id`. Answered
`{"behavior": "completed", "result": …}` or `{"behavior": "cancelled"}`.

**No dialog kind is recorded.** The complete corpus contains zero
`request_user_dialog` frames, so the set of kinds is unknown, not empty, and
the shape of a `result` is unknown per kind. The design is built for that
honestly, in two layers.

A payload carrying a `message` string and an `options` array of labelled
choices is rendered as a choice panel and answered `completed` with the chosen
option:

```
    dialog — trust_prompt · 1 of 1

    The workspace ~/work/amux is not in your trusted folders.

    › 1. Trust this folder
      2. Don't trust it
      3. Cancel — the agent is told the dialog was dismissed
    1-3/↑↓ select · enter confirm · esc back (never answers)
```

Any other payload renders blocked, with the kind, a bounded and
control-sanitized summary of the payload, and Cancel as the only answer:

```
    dialog — settings_editor · 1 of 1

    This request cannot be answered from the chat.
    kind settings_editor · payload: object with 4 fields (scope, path, edits, revision)

    › 1. Cancel — the agent is told the dialog was dismissed
    enter confirm · esc back (never answers)
```

Raw JSON is never shown, and Cancel is labelled as what it is so nobody reads
it as agreement. Blocked kinds join the gap list, and each kind we record
afterwards can graduate to a typed panel without a protocol change, because the
row carries the kind and payload verbatim.

## Keys

Everything in `docs/CHAT.md` §Keybindings applies unchanged. The SDK chat adds
one binding and gives two existing ones new facts to act on.

| Key | Context | Action | Tier |
| --- | --- | --- | --- |
| `<leader> c` | SDK chat | fetch and open the context breakdown; again to refresh, esc to close | plain |
| Shift+Tab | composer | cycle permission mode — a `set_permission_mode` control publishes the acknowledged mode in session facts | plain (CSI Z) |
| Ctrl+X | chat | interrupt — here the SDK interrupt control, which the session acknowledges | plain |

The model is displayed, and settable by a typed client command, but has no key:
a model picker is composer grammar, and this design does not add composer
grammar. Bypass mode is refused with a typed error unless the session was
launched with it granted.

## What the three chats share

Every surface above, and whether the other two chats take it. "Adopts" means
the surface lands there in this work from that backend's own facts; "lacks"
names the capability that is missing, which is the only acceptable reason for a
visible difference between the three chats.

| Surface | Claude PTY chat | Codex chat |
| --- | --- | --- |
| Header session-fact line | Adopts. `message.model` is a per-message fact in the transcript and permission mode is a hook fact; both print in the same place. | Adopts. It already prints model, approval and sandbox there; only the placement and separators change. |
| Streaming assistant message | Lacks: main-session transcript files burst-write whole messages, so there is no partial text to stream. Block-level streaming exists only in subagent files. | Adopts. The app-server sends `item/agentMessage/delta`, which the layer does not fold today; the same open-block rule applies. |
| Task list | Lacks a lifecycle: the transcript has `Task` tool calls and sidechain files, not task state rows, so the existing subagent line stays and no live list is synthesized. | Lacks: subagent-sourced items exist, but there is no task lifecycle vocabulary to fill a state column. |
| Context meter | Adopts, partially: the same used-token sum is available per assistant message, but no row states the context window, so the PTY meter shows used tokens with no denominator. | Adopts fully. `thread/tokenUsage/updated` carries the totals and `modelContextWindow`. |
| Context breakdown overlay | Lacks: no control returns a per-category accounting; Claude's own `/context` is a terminal screen, not a fact. | Adopts, coarsely: its token-usage row breaks down into input, cached, output and reasoning — four categories, not the per-tool grid, and the overlay says so. |
| MCP status line | Lacks: the transcript carries no server inventory or health. | Already has it — the SDK chat adopts the Codex chat's rule rather than the other way round. |
| Permission panel | Shared. Same tool vocabulary, same suggestion-derived options; only the encoding beneath differs. | Adopts the panel shell; its options stay its own wire-verbatim decisions. |
| Question panel | Shared, unchanged — the same `AskUserQuestion` tool through a different transport. | Lacks: no question obligation exists in the app-server vocabulary. |
| Plan reader and its three actions | Shared, with different encodings: the PTY path composes keystrokes, the SDK path answers the request. | Lacks an obligation: Codex streams plan items, but never asks for approval of one. |
| Elicitation form | Lacks: Claude's own terminal answers elicitations in band and writes nothing to the transcript. | Lacks an answerable shape: `item/tool/requestUserInput` is documented as unanswerable in the frozen input vocabulary and stays visibly blocked. |
| Dialog panel | Lacks, for the same reason: the terminal answers dialogs in band. | Lacks: no equivalent request. |
| Reader, review page, attachments, exploration runs, family banners | Shared and already built; the SDK chat adopts them as they are. | Shared and already built. |

The two Claude chats share Claude's own tool vocabulary — how an `Edit`,
`Write`, `Bash`, `Task`, `AskUserQuestion` or `ExitPlanMode` input reads, and
the documents an ask puts in the reader — through one facts module, because it
is literally the same provider producing the same JSON. They share nothing
else: two folds, two conditions, two feeds. `docs/UI.md` states that boundary
normatively.

## Named gaps

Carried forward to the capability inventory, each with the capability that is
missing rather than an apology:

- **Dialog kinds are unknown.** No `request_user_dialog` frame has ever been
  recorded, so the choice-panel recognizer is a design bet until a live kind
  arrives. Unrecognized kinds render blocked with Cancel.
- **Elicitation schemas beyond flat fields** — nested objects, arrays,
  `oneOf` — render blocked. A person answers them by interrupting.
- **The PTY context meter has no denominator**, because no transcript row
  states the context window.
- **Neither the PTY chat nor the Codex chat gets a task list**, for want of a
  task lifecycle in either backend.
- **No nested subagent timeline** anywhere: the list reports what a session
  says about its children.

## Rejected alternatives

- **One shared Claude layer for both drivers.** The PTY layer infers working,
  streaming and turn ends from a burst-written file; the SDK states them. A
  shared fold would either force inference onto authoritative facts or promise
  streaming the transcript cannot deliver. Sharing the tool vocabulary is
  translation of one provider's JSON; sharing the fold would be normalization.
- **Polling `get_context_usage` every turn.** The assistant row's usage and the
  result's `contextWindow` already arrive for free. A round trip per turn buys
  only the per-category breakdown, which is an overlay a person opens.
- **Auto-answering elicitations and dialogs in the daemon**, which is today's
  behaviour. Declining on someone's behalf is exactly the auto-answering this
  work exists to remove.
- **Rendering an unrecognized dialog payload as JSON with a free-text answer.**
  It looks answerable and is not; a stated limit is more useful than a guess
  that reaches a live agent.
- **A spare PTY so SDK agents could offer raw attach.** The SDK process has no
  terminal UI; raw would show an empty screen and a second process to supervise.
