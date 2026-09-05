# Local test networks

Start the debug runner with a topology file:

```sh
timeout 3600 wt run testnet -- serve --topology e2e-tests/topologies/two-hosts.json
```

The runner starts real daemons and a loopback relay with isolated identities,
trust stores and temporary data directories. The first and only stdout line
is JSON containing `relay`, `control`, per-user bearer credentials, daemon
identities and agent identities. Readiness follows daemon attachment and the
declared pairings. A cold workspace build happens before the 30-second
readiness deadline begins.

Send one JSON value per line to the TCP `control` address. Multiple clients
may connect; operations execute in arrival order. Each request returns one
`Ack` after its operation settles, or an `Error` with a message. An error does
not undo an operation that has already started.

| Request | Effect |
| --- | --- |
| `"CloudOffline"` | Stop the relay and sever its accepted sockets; wait for daemons to lose their relay links. |
| `"CloudOnline"` | Rebind the same relay address and wait for daemon attachment. Already online is a no-op. |
| `{"SeverDirect":{"a":"laptop","b":"desktop"}}` | Close both ends of the direct link; routes through the relay remain available. |
| `{"EstablishDirect":{"a":"laptop","b":"desktop"}}` | Restore the direct link using stored TCP reachability. Both hosts must still trust each other. |
| `{"RestartDaemon":{"name":"laptop"}}` | Stop and restart the daemon, preserving its identity, trust and listening address; wait for reachable peers to see it again. Provider processes end with the old runtime. |
| `{"Unpair":{"daemon":"laptop","peer":"desktop"}}` | Revoke the peer through the daemon's normal local administration API. |
| `{"StartPinPairing":{"daemon":"desktop","ttl_secs":30}}` | Start PIN pairing with a TTL of 1–3,600 seconds; return the six-digit `pin`. |
| `{"StartQrPairing":{"daemon":"desktop"}}` | Start QR pairing; return `qr` in the existing JSON pairing-payload format, pointing at the test relay. |
| `{"Latency":{"millis":100}}` | Delay each newly received TCP chunk entering the relay by 0–1,000 ms. Applies to existing and future connections; direct links and the control socket are unaffected. |
| `{"Connections":{"daemon":"desktop"}}` | Return the number of live daemon links in `connections`, including its relay link. RPC tunnels are not additional links. |
| `"Shutdown"` | Stop daemons and relay, remove temporary state, acknowledge and exit. SIGTERM also cleans up. |

An acknowledgement always has the same shape; unused fields are null or empty:

```json
{"Ack":{"pin":null,"qr":null,"observed":[],"connections":2}}
```

Replay the control protocol and its independent daemon observations with
`timeout 900 wt test -- testnet_control -- --nocapture`. Process teardown is
covered by `timeout 900 wt test -- testnet_serve`.

## Scripted Claude sessions

Rust harnesses can create a process-free Claude PTY session with
`amux::testnet::script::session(script).await`. Keep its returned `Provider`
handle alive while consuming the returned `claude::pty::Session`. The provider
writes a temporary JSONL transcript and sends real Claude hooks; the session's
normal tailer, parser and semantic ask handling produce the events.

Scripts use externally tagged JSON variants. For example:

```json
{
  "reactions": [
    {
      "on": "AnyPrompt",
      "play": [
        {"Markdown": {"text": "Checking the workspace."}},
        {"Ask": {"Permission": {
          "tool": "Bash",
          "invocation": {"command": "pwd"},
          "scoped_directories": ["/workspace"]
        }}}
      ]
    },
    {
      "on": {"Answer": "Permission"},
      "play": [
        {"Tool": {"name": "Bash", "input": {"command": "pwd"}, "output": "/workspace", "denied": false}},
        "EndTurn",
        {"Exit": {"code": 0}}
      ]
    }
  ],
  "commands": [],
  "models": [],
  "efforts": []
}
```

`Provider::feed` records decoded inputs in arrival order and selects the first
matching reaction at or after the cursor, consuming through that reaction.
Triggers are `AnyPrompt`, `PromptContains`, `Command`, `Answer`, `Interrupt`
and `Any`. Command triggers match the first slash-command word of a prompt;
answer triggers distinguish permission, question and plan responses. The
capability lists are script metadata. Unknown ask IDs return `UnknownAsk`
without consuming a reaction; unmatched inputs return `Exhausted`.

A prompt received during a turn is observed immediately and played after the
current reaction reaches `EndTurn` and finishes its remaining steps. Deferred
prompts keep arrival order. EndTurn writes one duration row and Stop hook per
prompt, even if repeated. Reactions without EndTurn stay open for answers or
control operations. `Provider::play` accepts additional steps and waits for
their transcript and hook ingestion; consume the session concurrently to keep
its bounded event stream moving.

Steps support raw JSONL rows, Markdown, tool calls and results, permission,
question and plan asks, todos, provider child notifications, agent messages,
working time, turn end, compaction, API errors, exit and unknown raw values.
Todo states are `pending`, `in_progress` and `completed`. Child notifications
describe provider-internal work; they do not create a separate daemon agent.
Exit reports its code and closes the event stream, including when the control
handle remains held. Dropping the provider removes its temporary transcript
and ends playback. Asynchronous playback errors are available from
`Provider::error`.

Run `timeout 900 wt test -- testnet_script -- --nocapture` to see the parsed
transcript and hook capture along with checks for asks, deferred prompts,
turn boundaries and cleanup.

Use `e2e-tests/topologies/scripted-agents.json` for a runnable scripted topology.
Claude script paths, working directories and repository roots resolve relative
to the topology file. Scripts are parsed before any network resources start.
Codex recording playback is not available yet.

| Request | Effect |
| --- | --- |
| `{"AgentEmit":{"agent":"helper","rows":[{"type":"custom","value":1}]}}` | Append provider JSONL rows and wait for parser ingestion. |
| `{"AgentRaiseAsk":{"agent":"helper","ask":{"Plan":{"markdown":"Review this plan."}}}}` | Raise a semantic ask through provider rows and hooks. |
| `{"AgentEndTurn":{"agent":"helper"}}` | Close the current scripted turn once. |
| `{"AgentExit":{"agent":"helper","code":0}}` | End the provider session with a nonnegative exit code. |
| `{"AgentSpawnChild":{"agent":"helper","child":"reviewer"}}` | Create a separate Claude session on the same daemon, inheriting the directory and recording the parent relationship. Its empty script is driven by controls. |
| `{"AgentObserve":{"agent":"helper"}}` | Return all decoded inputs accepted by the daemon and delivered to this provider, in arrival order. Controls do not count as inputs. |

Observations contain `seq`, `intent`, `text`, `ask_id`, `answer` and `pins`.
The current PTY intent seam has no attachment pins; that field is empty.
The daemon checks the stream sequence and the real provider control validates
input before script delivery. Observations remain readable after provider
exit. Restart removes handles for the stopped daemon's scripted agents.

`amux::testnet::connect_user(relay, token)` opens a client-only embedded runtime
with the normal routing and client services against the loopback relay. It
supplies the test token directly in place of production token exchange. The
client has an isolated device identity and must pair with the host, even when
both use the same account. Use `StartQrPairing` and the client's QR pairing API;
after the agent appears, `Runtime::note_attached` opens its structured stream.
Run `timeout 900 wt test -- testnet_agents -- --nocapture` to see the control
requests, exact host observations and projected transcript from a production
`amux_ui::Runtime` using that connection. The test also checks account isolation,
child asks, invalid controls, exit and restart cleanup.
