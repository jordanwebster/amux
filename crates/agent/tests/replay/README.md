# Replay fixtures

Each directory is one provider process in the playback format the fake
providers read (`io.jsonl`: `{"dir":"stdin"|"stdout"|"exit","line":…}`,
plus `transport_id` for a terminal session's `pty`, `hook` and
`transcript` channels). The fake writes every recorded output and checks
every byte the agent writes against the recorded input.

They are real recordings, cut or rewritten only where the agent differs
from the recording host:

| Fixture | From | Rewritten |
| --- | --- | --- |
| `sdk_question` | `claude-specs/fixtures/sdk/question_asked` | the initialize request, in the agent's key order, and its response's `request_id` (`agent-initialize`); after its answer, the interpreter's `get_settings` request and an answer in the shape `claude-specs/fixtures/sdk/effort` records (Haiku takes no effort, so none is applied); the prompt line in the agent's key order with `uuid` |
| `sdk_lost` | `sdk_question` | cut after the permission request, then an exit |
| `codex_approval` | `codex-specs/fixtures/runtime/approval_allow` | the agent's handshake ids (`agent-initialize`, `agent-thread`) and `thread/start` params; `turn/start` as the interpreter writes it (`amux-1`, `text_elements`); after the thread answer, the interpreter's `model/list` and `skills/list` requests and their answers, in the shape codex-cli 0.157.0 answers them (no recording holds them yet) |
| `codex_lost` | `codex_approval` | cut after the approval request, then an exit |
| `pty_deny` | `claude-specs/fixtures/pty/permission_deny_feedback` | nothing |
| `pty_lost` | `pty_deny` as recorded on 2.1.251, before the deny became Escape | cut after the permission request, then an exit |

Placeholders filled per run: `{{cwd}}`, `{{version}}` (the agent's), and
`{{uuid:<input id>}}` (the uuid headless Claude's message for that input
carries). When the agent's writes change on purpose, the provider log of
the failing test shows the new line beside the recorded one; update the
fixture line and say why in the commit.
