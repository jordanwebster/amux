# The Claude SDK chat

Status: implemented, normative for the SDK-backed Claude chat. The capability
inventory and named gaps below distinguish shipped surfaces from provider
limitations and missing live evidence.
Companions: `docs/CHAT.md` owns the shared chat shell — frame, feed blocks,
reader, review page, ask lifecycle, keybinding tiers and themes — and is not
restated here; `docs/UI.md` owns the client-layer doctrine (one layer per agent
kind, one classification projected, facts before inference); `docs/CODEX.md`
owns the Codex chat, which this one deliberately resembles; and
`docs/CLAUDE_TRANSCRIPT.md` owns the transcript facts the other Claude chat
folds. Where prose and a passing specification disagree, the specification wins.

Recorded wire shapes come from Claude Code 2.1.260 under
`crates/claude/fixtures/sdk/` — `question_asked`, `plan_reviewed`,
`elicitation_accepted` — and the older corpus at 2.1.247/2.1.251. The live
conversation fixture under `crates/amux-ui/tests/spec/fixtures/claude_sdk/`
was captured at 2.1.261. Dialog routing was inspected in that installed binary;
those source findings are distinguished below from recorded traffic.

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
MCP elicitation and user-dialog requests use `control_request` frames. There
is no terminal underneath where a person could answer them in band. The daemon
therefore exposes received requests to the chat and keeps them pending rather
than answering for the user. Permission, question, plan and elicitation have
live captures; dialog transport and its panel remain unvalidated against a
real frame, as described below.

There is no raw attach for an SDK agent — the process has no terminal UI to
attach to — so this chat is the only way in, on every host.

## Choosing the driver

To use the SDK for newly created Claude agents, set this in the installation
config (`$XDG_CONFIG_HOME/amux/config.yaml`), which every profile shares:

```yaml
claude:
  driver: sdk
```

The shipped default is `pty`. An explicit `amux new claude --driver pty` overrides
an `sdk` configuration for that agent. CLI creation, the TUI create flow and
MCP spawn use the same driver resolver; changing the config never converts an
existing session. The fleet and chat identify both drivers simply as Claude.

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

The chat header is the shared one: `name · kind @ host` on the left, model,
permission mode and phase on the right. The activity row above the composer
carries the passive context meter and running-task count:

```
  fix-auth · claude @ mbp                                                       sonnet-4-5 · default · working
  ... conversation feed ...
  ◐ working · ctx 34.1k/200.0k · 2 tasks running · ctrl+x interrupt
```

Every field is a stated fact. The model and permission mode come from
`system.init`, `system.status` and acknowledged session-control rows; the meter
is derived as below; the task count is the number of tasks still running. An
unreported model or mode is omitted. The meter explicitly says `ctx unknown`
until usage arrives.

Nothing on this line names the backend. `claude @ mbp` is what a PTY agent
shows too. The PTY and Codex chats print their own session facts in the same
header region.

## The streaming block

An open assistant message paints as an ordinary assistant block with a dim
caret at the growth point:

```
▎   now make the retry count configurable

    The cap belongs in RetryConfig; I'll thread it through SyncOptions and
    default it to six attempts▌

  ◐ working · ctx 34.1k/200.0k · ctrl+x interrupt
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

Subagent runs get one block per task, in the feed where the task was launched,
and a count in the activity row. The block updates in place:

```
  ⣾ Explore · scan the sync client for retry paths · running · Read sync/client.rs
  ✔ general-purpose · draft the migration plan · done · 3 tools · 42s
```

The `Task`/`Agent` tool use that launched the subagent and the task rows that
follow describe one subagent, so they are one entry: the first task row takes
the launch row over where it sits, carrying the description and subagent type
the launch already stated, and the launch tool's own result ("Agent launched.")
is not the task's outcome. State comes from the task rows, not from the tool
stream: `task_started` opens the block, `task_updated` and `task_progress` move
it, `task_notification` closes it with the summary the subagent reported. A
task whose lifecycle stops arriving stays in its last stated state and is not
aged into a guess. The lifecycle rows (`task_started`, `task_progress`,
`task_updated`, `task_notification`) carry the launching `tool_use_id`, which
is how the two are joined; the `background_tasks_changed` list names tasks by
id alone, so a task it announces first moves into the launch row when the
first row carrying the id arrives.

A subagent's own rows arrive on the parent's stream marked with that same
`tool_use_id` as `parent_tool_use_id`. They are the subagent's timeline, not
the session's, so each message and tool use paints as one muted line naming
its task — `└ scan the sync client · Read sync/client.rs`, clipped to the row
— and never joins the session's exploration runs on either side of it. A
subagent's thinking paints nothing: the task block already says it is running.
Nothing is read out of child transcript files.

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
    wt test -- sync

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
manual label. Both paths are recorded and confirmed (`plan_reviewed`, at
2.1.261): the auto approval's `setMode acceptEdits` is followed by a
`system.status` row stating `acceptEdits`, and the manual approval's `setMode
default` by one stating `default`. One call suffices; nothing has to be re-sent
after the turn.

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

      1. Send
      2. Decline     the server is told you declined
      3. Cancel
    tab/↑↓ move · enter next field · esc back (never answers)
```

One cursor moves over the fields and then the action list: a field is
edited where it sits, Enter steps to the next one, and on the action list
Enter answers. A required field left empty keeps Send from being offered,
with what is missing stated where Send is.

The form is derived from the schema, not authored: one field per property,
typed as text, number, boolean or a choice list from `enum`, with `required`
marking the fields that must be filled before Send is offered. `description`
renders as the dim helper line and `default` prefills.

Fields are ordered by name, not by the order the server declared them. A JSON
object's keys do not keep their written order once the request has been read,
so the declaration order never reaches the chat; name order is the one order
that draws the same form for the same schema every time.

A schema this cannot express — nested objects, arrays, `oneOf` — renders the
panel **blocked**, naming the reason and offering only Decline and Cancel. That
is Codex's rule for unanswerable obligations (`docs/CODEX.md`), and it is the
right one: a half-answer that looks like an answer is worse than a stated
limit. Blocked schemas join the gap list below.

Declining is a person's answer and travels as `decline`; the daemon never
answers an elicitation on its own.

### Dialog

`control_request` with `subtype: "request_user_dialog"`, carrying an open-string
`dialog_kind`, an opaque `payload` and an optional `tool_use_id`. Answered
`{"behavior": "completed", "result": …}` or `{"behavior": "cancelled"}`.

**No dialog kind is recorded.** The corpus contains no `request_user_dialog`
frame. Inspection of Claude Code **2.1.261** found **37 registered dialog
kinds**, but the headless adapter forwards exactly two as user dialogs:
`refusal_fallback_prompt` and `fable_overage_consent_prompt`. It returns defaults
for the other registered dialogs; MCP elicitation uses the separate
`elicitation` control channel. Registering a kind in the provider is not proof
that it can reach a headless client.

Forwarding also requires declaring the kind at initialization and satisfying
its runtime gates. amux currently declares no supported dialog kinds. Neither
forwardable kind was reached in the live probes: overage consent needs a
billing state we will not manufacture, and a benign copyright refusal ended
the turn normally without triggering refusal fallback. The live dialog
demonstration is therefore absent.

The panel ships as the kind-and-payload recognizer with a blocked fallback,
unvalidated against a real frame. The two source-known kinds have specialized
payloads and string-valued choices, not the generic shape illustrated below;
they have no typed panel here. The daemon must never auto-cancel a dialog it
receives. It retains the request until a person answers or the session exits.

The following examples are synthetic illustrations of the recognizer and
fallback, not recorded provider dialogs.

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
it as agreement. A recorded kind can receive a typed panel without changing
the row protocol, which carries the kind and payload verbatim.

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

## The capability inventory

Every capability the Claude terminal chat has, and what the SDK chat does
with it. "Adopted" means the same surface, from the SDK's own facts.
"Absent" is followed by the fact that cannot express it, or — where the
SDK can express it and the surface simply has not been built — by that
admission, because a parity list that hides unbuilt work is worth nothing.

| Terminal-chat capability | SDK chat |
| --- | --- |
| User prompt with optimistic echo, and a failed send that resurfaces the draft | Adopted, from the same composer and the same send lifecycle. |
| Assistant message as terminal markdown, upserted as its parts arrive | Adopted. Identity is the message's own id rather than an inference over rows. |
| Partly-arrived reply | Better: the terminal transcript burst-writes whole messages, so it can only show liveness; the SDK streams the text and the block carries a caret. |
| Thinking marker | Adopted, and stated rather than inferred: an open thinking block reads `~ thinking`, a closed one `~ thought`, and a redacted one says so. |
| Tool one-liner with its target and its outcome | Adopted, including a failed tool and the head of what it printed. |
| File change with its magnitude and a patch preview | Adopted. A landed edit paints as `✎ Edit src/lib.rs · +2 −1` with the patch under it, from the `structuredPatch` the tool result carries. Both chats read that sidecar through the shared facts module. |
| Collapsed exploration run | Adopted. Two or more consecutive reads and searches fold to one row — `2 reads · 2 searches · sync/config.rs, sync/client.rs` — that `<leader> o` opens and shuts. The SDK layer states the grouping from its own tool names, and both chats walk the same projection. |
| Subagent line | Better: a subagent is a task with a lifecycle, so the SDK chat states what each one was asked to do, whether it is running, and what it last used. |
| Agent-to-agent message and family banners | Adopted unchanged — the kernel gives both chats the message. |
| Interruption marker | Adopted in a different shape: the interrupted message itself is marked, rather than a separate marker row. |
| API error row | Adopted in a different shape: the rule that closes an errored turn is followed by what the session said went wrong — the error strings it collected, or the result text when it collected none. |
| Turn rule with its duration | Adopted, plus what the turn cost, which the SDK prices and the transcript does not. |
| Compaction rule | Adopted, with the tokens before and after. The transcript's post-compaction summary row has no SDK equivalent: it is an artifact of the file, not a fact of the session. |
| Unrecognized row, retained and painted | Adopted, and additionally a stated ready, resumed, history-gap or conversation-reset boundary. |
| Attachment blocks, and reading a pasted one | Adopted unchanged. |
| Review page and the reader over a sent review | Adopted unchanged. |
| Retention, windowing, scrollback and replay | Adopted unchanged — they belong to the shared frame, not to a backend. |
| Permission ask | Adopted. Same tool vocabulary and the same suggestion-derived options; the answer is a control response rather than composed keystrokes. |
| Question ask | Adopted — the same tool, over a different transport. |
| Plan review and its three actions | Adopted. The terminal path composes keystrokes; this one answers the request. |
| Collapsed fact for a resolved ask | Adopted unchanged. |
| Header identity and phase word | Adopted. The phase is a stated session state rather than an inference over file quiet. |
| Header session facts — model and permission mode | Adopted, and the terminal chat now takes the same line back from this one. |
| Context meter | Adopted with a denominator: the session states its context window, which no transcript row does. |
| MCP status line | Absent from the terminal chat, present here: the session reports its server inventory and health. |
| Read-only chat | Adopted unchanged. |
| Composer send, steer, newline, history, paste | Adopted unchanged. |
| Interrupt | Adopted as the SDK interrupt control, which the session acknowledges. |
| Permission-mode cycling on Shift+Tab | Adopted, and better: the acknowledged mode comes back as a fact instead of being read off a screen. |
| Help overlay and the shared keybinding tiers | Adopted unchanged. |
| Raw attach | Absent, permanently: the process has no terminal UI to attach to. This chat is the only way in, which is why every request reaches the screen. |

## What the three chats share

Every surface above, and whether the other two chats have it. "Adopted" means
the surface is implemented there from that backend's own facts; "lacks"
names the capability that is missing, which is the only acceptable reason for a
visible difference between the three chats.

| Surface | Claude PTY chat | Codex chat |
| --- | --- | --- |
| Header session-fact line | Adopted. `message.model` is a per-message fact in the transcript and permission mode is a hook fact; both print in the same place. The footer states the key that cycles permission mode. | Adopted. Model, approval and sandbox appear on the header's right, bare-valued and joined like the other two. They are creation choices the launcher hands over, so a chat opened another way states none. |
| Streaming assistant message | Lacks: main-session transcript files burst-write whole messages, so there is no partial text to stream. Block-level streaming exists only in subagent files. | Adopted. The layer already folds `item/agentMessage/delta`; the open block now carries the same caret instead of a marker row of its own. |
| Task list | Lacks a lifecycle: the transcript has `Task` tool calls and sidechain files, not task state rows, so the existing subagent line stays and no live list is synthesized. | Lacks: subagent-sourced items exist, but there is no task lifecycle vocabulary to fill a state column. In the SDK chat the launch tool row and the task are one entry, joined by the launching `tool_use_id`, and the subagent's own rows paint as attributed one-liners. |
| Context meter | Adopted, partially: the same used-token sum is available per assistant message, but no row states the context window, so the PTY meter shows used tokens with no denominator. | Adopted fully. `thread/tokenUsage/updated` carries the latest turn's usage and `modelContextWindow`; the meter uses that context snapshot, not cumulative thread spend. |
| Context breakdown overlay | Lacks: no control returns a per-category accounting; Claude's own `/context` is a terminal screen, not a fact. | Adopted, coarsely: its token-usage row breaks down the latest turn into input and output, with cached input and reasoning nested under their respective totals. It has no per-tool grid, and the overlay says so. Nothing is fetched: the numbers arrived with the last turn, so `<leader> c` never refreshes. |
| MCP status line | Lacks: the transcript carries no server inventory or health. | Already has it — the SDK chat adopts the Codex chat's rule rather than the other way round. |
| Permission panel | Shared. Same tool vocabulary, same suggestion-derived options; only the encoding beneath differs. | Adopted the panel shell; its options stay its own wire-verbatim decisions. |
| Question panel | Shared, unchanged — the same `AskUserQuestion` tool through a different transport. | Lacks: no question obligation exists in the app-server vocabulary. |
| Plan reader and its three actions | Shared, with different encodings: the PTY path composes keystrokes, the SDK path answers the request. | Lacks an obligation: Codex streams plan items, but never asks for approval of one. |
| Elicitation form | Lacks: Claude's own terminal answers elicitations in band and writes nothing to the transcript. | Lacks an answerable shape: `item/tool/requestUserInput` is documented as unanswerable in the frozen input vocabulary and stays visibly blocked. |
| Dialog panel | Lacks, for the same reason: the terminal answers dialogs in band. | Lacks: no equivalent request. |
| Reader, review page, attachments, exploration runs, family banners | Shared and already built; the SDK chat adopts them as they are. | Adopted the reader: `<leader> o` opens a pasted attachment or a sent review in the same fullscreen reader. It carries no action row — a Codex reader only ever shows something already sent. The rest was already built. |

The two Claude chats share Claude's own tool vocabulary — how an `Edit`,
`Write`, `Bash`, `Task`, `AskUserQuestion` or `ExitPlanMode` input reads, and
the documents an ask puts in the reader — through one facts module, because it
is literally the same provider producing the same JSON. Built directly on that
vocabulary, they also share the walk that folds consecutive reads and searches
into one run, so a run reads the same whichever feed carried it. The TUI
adapters reuse the Claude ask panels, reader and review presentation. State
remains separate: two folds, two conditions, two feeds. `docs/UI.md` states
that boundary normatively.

## Named gaps

Carried forward to the capability inventory, each with the capability that is
missing rather than an apology:

- **Live dialogs are unvalidated.** No kind is recorded. Claude Code 2.1.261
  registers 37 kinds, but its headless adapter forwards only
  `refusal_fallback_prompt` and `fable_overage_consent_prompt` as dialogs and
  returns defaults for the others (MCP elicitation uses its own channel). amux
  declares neither kind. The overage path requires billing conditions we will
  not manufacture; a benign copyright refusal did not trigger refusal fallback
  and ended normally. The corpus still has no `request_user_dialog` frame. The
  kind-and-payload recognizer and blocked Cancel fallback ship without live
  validation; the daemon never auto-cancels a received dialog.
- **Elicitation schemas beyond flat fields** — nested objects, arrays,
  `oneOf` — render blocked with a reason and explicit Decline or Cancel.
  The chat cannot submit content for those schemas.
- **The PTY context meter has no denominator**, because no transcript row
  states the context window.
- **Neither the PTY chat nor the Codex chat gets a task list**, for want of a
  task lifecycle in either backend.
- **No nested subagent timeline** anywhere: the task block reports what a
  session says about its children, and the SDK chat's attributed one-liners
  are that session's own stream, not a child transcript.

## Rejected alternatives

- **One shared Claude layer for both drivers.** The PTY layer infers working,
  streaming and turn ends from a burst-written file; the SDK states them. A
  shared fold would either force inference onto authoritative facts or promise
  streaming the transcript cannot deliver. Sharing the tool vocabulary is
  translation of one provider's JSON; sharing the fold would be normalization.
- **Polling `get_context_usage` every turn.** The assistant row's usage and the
  result's `contextWindow` already arrive for free. A round trip per turn buys
  only the per-category breakdown, which is an overlay a person opens.
- **Auto-answering elicitations and dialogs in the daemon.** Declining or
  cancelling on someone's behalf hides a decision the user must make.
- **Rendering an unrecognized dialog payload as JSON with a free-text answer.**
  It looks answerable and is not; a stated limit is more useful than a guess
  that reaches a live agent.
- **A spare PTY so SDK agents could offer raw attach.** The SDK process has no
  terminal UI; raw would show an empty screen and a second process to supervise.
