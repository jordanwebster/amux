# Local test networks

Run the offline smoke suite with:

```sh
just test-crate testnet -- testnet_ -- --nocapture --test-threads=1
```

This runs the testnet crate's `testnet_` tests serially and prints their
control requests, replies and projected transcripts. It starts the runner as
a subprocess from a declared topology, checks network controls through
independent clients, sends a scripted Claude prompt and permission answer
through the relay using the production UI runtime, and verifies a strict
Codex recording. Shutdown and SIGTERM must both exit successfully, release
the relay and control listeners so they can be rebound, and remove temporary
state. The network and provider journeys also assert that their sockets
refuse connections after shutdown. No provider executable or cloud account
is needed. The normal workspace test recipe carries its own wall-clock
bound.

## Topology and readiness

Build the harness with `just ios tools`, then start the served network
with a topology file:

```sh
target/debug/testnet serve --topology e2e-tests/topologies/two-hosts.json
```

The daemons are ordinary runtimes and say nothing unless asked. Setting
`RUST_LOG` turns their tracing on and writes it to standard error, which is
how a driver outside this process watches them decide:

```sh
RUST_LOG=warn,node::services::reachability=debug target/debug/testnet serve \
  --topology e2e-tests/topologies/two-hosts.json
```

A topology is a JSON object with a `cloud_url` and four required lists, plus
an optional `tiers`.
Omitting `cloud_url` uses the installation default, `https://amux.sh`. For example,
`e2e-tests/topologies/two-hosts.json` contains:

```json
{
  "cloud_url": "https://amux.sh",
  "users": ["personal", "work", "unattached"],
  "daemons": [
    {"name": "laptop", "user": "personal", "repository_roots": ["../.."]},
    {"name": "desktop", "user": "personal", "repository_roots": ["../.."]}
  ],
  "paired": [["laptop", "desktop", "Cloud"]],
  "agents": [
    {"name": "helper", "daemon": "desktop", "working_dir": "../..", "provider": {"Claude": {"script": "../scripts/idle.json"}}}
  ]
}
```

A daemon may declare `"lan": true`, which puts it on the network when the
topology starts, so a device that browses before sending any control verb
finds it there. A simulator browses the Mac's own network instead, where a
machine appears only once `Announce` puts it there.
`e2e-tests/topologies/onramp.json` is the smallest such network: one machine
on this network, nobody signed in anywhere.

```json
{
  "cloud_url": "https://amux.sh",
  "users": [],
  "daemons": [
    {"name": "workstation", "repository_roots": ["../.."], "lan": true}
  ],
  "paired": [],
  "agents": []
}
```

`tiers` says what an account buys where it is not the paid default, and is
applied before anything can ask for a token — so a device signing in with that
account is admitted on it. `e2e-tests/topologies/free-tier.json` is one account
that has not paid for the relay, with one machine on it.

User labels are unique. A daemon names a declared user, or names none at all,
which is a device nobody has signed in on: it still pairs with and reaches the
machines on its own network, and has no relay. Pairings name
two distinct daemons and use `Cloud` or `Tcp`; cloud pairings must share a
user, so a daemon with no user pairs directly. Daemon and agent names are unique within their lists and may contain
ASCII letters, digits, hyphens, underscores and periods, except `.` or `..`.
Each daemon's `repository_roots` configures its host repository enumeration; an
empty list exposes no enumerated repositories. Successfully created agent
directories also appear as recent projects.

Each agent names its daemon and a Claude PTY script, a Claude SDK model, or a Codex recording.
All directory, script and recording paths resolve relative to the topology
file; directories must
exist. Invalid declarations fail before network startup. Empty lists are
allowed, including users without a daemon.

The runner starts real daemons, a loopback relay and a fake identity service
with isolated identities, trust stores and temporary data directories. The
first and only stdout line is JSON containing `cloud_url`, `identity`,
`relay`, `control`, per-user bearer credentials, daemon identities and agent
identities. `identity` is the URL of the fake identity service, reported
separately from `relay`: the service mints tokens and names the relay, the
relay only carries traffic. Phone journeys hand the app one of the static
user tokens rather than signing in through the service. Readiness follows
daemon attachment and the declared pairings. A cold workspace build happens
before the 30-second readiness deadline begins.

Each user entry has `label`, `user_id` (UUID) and `token`. Each daemon entry
has `name`, `host_id` (UUID) and `fingerprint` (64 hexadecimal SHA-256 digits
of its public key). Each agent entry has `name`, `daemon` and `agent_id`
(UUID). Both socket addresses use `127.0.0.1` with an ephemeral port. Tokens
are issued by this isolated test cloud and accepted by its assigned relay.
Diagnostic output goes to stderr, leaving stdout available for a driver to parse.

The cloud owns its identity URL, issues per-user credentials, and assigns a
relay. The relay authenticates those credentials and carries device traffic;
its socket address is never a cloud identity. The topology writes `cloud_url`
into each daemon's config file before startup. Client-only Rust harnesses
likewise write and load a config file using readiness's `cloud_url`, then
connect to the independently supplied `relay`. Restart reads the existing
config. Neither relay attachment nor the driver rewrites a running device's
cloud or the invitation produced by a host.

The iPhone loads the cloud from its installation's profile config, which
currently uses the `https://amux.sh` default. Phone journey topologies name
that cloud and receive an ephemeral loopback relay from it. The scripted
account boundary supplies relay credentials and routing; it does not change
cloud identity. Rust topologies can use `TestNet::builder().cloud_url(...)`
to exercise another cloud. Topologies with installation binding use the
existing identity HTTP fixture's own URL for every attached device; that
fixture names the cloud's independently addressed relay.

`just test-crate testnet -- testnet_control -- --nocapture` pairs by printed code
and QR over a relay whose address differs from the custom configured cloud.
`just test-crate testnet -- testnet_agents -- --nocapture` also exercises a
client config loaded from readiness with a nondefault cloud. Phone pairing
and account switching are exercised by `just ios journey hosts`
and `just ios journey accounts`.

## Control protocol

Send one JSON value per line to the TCP `control` address. Multiple clients
may connect; operations execute in arrival order. Each request returns one
`Ack` after its operation settles, or an `Error` with a message. An error does
not undo an operation that has already started.

Every request below is a verb the in-process harness also has, under the
same name: `CloudOffline` is `TestNet::cloud_offline`, `StartQrPairing` is
`Daemon::start_qr_pairing`, `AgentEmit` is `script::Provider::emit`, and so
on. The `testnet` crate documentation carries the full table. A phone journey
driving the door and a Rust spec calling the harness therefore say the same
sentence, and adding a verb means adding the method first.

| Request | Effect |
| --- | --- |
| `"CloudOffline"` | Stop the relay and sever its accepted sockets; wait for daemons to lose their relay links. |
| `"CloudOnline"` | Rebind the same relay address and wait for daemon attachment. Already online is a no-op. |
| `{"SeverDirect":{"a":"laptop","b":"desktop"}}` | Close both ends of the direct link and hold that pair's direct UDP path down; routes through the relay remain available. |
| `{"EstablishDirect":{"a":"laptop","b":"desktop"}}` | Release the held direct path and restore its QUIC link using stored reachability. Both hosts must still trust each other. |
| `{"RestartDaemon":{"name":"laptop"}}` | Stop and restart the daemon, preserving its identity, trust and listening address; wait for reachable peers to see it again. Provider processes end with the old runtime. |
| `{"Unpair":{"daemon":"laptop","peer":"desktop"}}` | Revoke the peer through the daemon's normal local administration API. |
| `{"StartPinPairing":{"daemon":"desktop","ttl_secs":30}}` | Start PIN pairing with a TTL of 1–3,600 seconds; return the six-digit `pin`. |
| `{"StartQrPairing":{"daemon":"desktop"}}` | Start QR pairing; return `qr` in the existing JSON pairing-payload format, naming the configured cloud identity. |
| `{"Latency":{"millis":100}}` | Delay relay traffic on its QUIC and TCP carriers by 0–1,000 ms. Applies to existing and future connections; direct links and the control socket are unaffected. |
| `{"Announce":{"daemon":"workstation"}}` | Put the machine on this network, as an advertisement a browsing device resolves, and return that advertisement in `found` as `{"host","name","version","addrs"}`. Nothing is trusted by it: what a browser gets is a name, an identity claim and addresses to try. The advertisement goes to the topology's daemons and is also published over real mDNS on the host machine, where a simulator's own browser resolves it from the record the daemon writes. |
| `{"Withdraw":{"daemon":"workstation"}}` | Take it off again, the way a machine going away says goodbye, on the topology and on the host machine's network. |
| `{"Tier":{"user":"personal","tier":"pro"}}` | Change what a declared account buys, from the next token it is issued. Links already up keep the tier they were admitted on until they re-authenticate, which is what makes the change observable rather than instantaneous. |
| `{"UdpBlocked":{"daemon":"phone","blocked":true}}` | Eat or restore every direct UDP datagram involving the machine — the network a phone on a hotel connection is on. |
| `{"Connections":{"daemon":"desktop"}}` | Return the number of live daemon links in `connections`, including its relay link. Routed RPCs are not additional links. |
| `{"Inventory":{"daemon":"desktop"}}` | Return the daemon's agents with their UUID, kind and driver, plus the devices it trusts. |
| `"Shutdown"` | Stop daemons and relay, remove temporary state, acknowledge and exit. SIGTERM also cleans up. |

An acknowledgement always has the same shape; unused fields are null or empty:

```json
{"Ack":{"pin":null,"qr":null,"observed":[],"sdk_inputs":[],"connections":2,"links":[],"agents":[],"devices":[],"found":null}}
```

Replay the control protocol and its independent daemon observations with
`just test-crate testnet -- testnet_control -- --nocapture`. Process teardown is
covered by `just test-crate testnet -- testnet_serve`.

## Scripted Claude sessions

Rust harnesses can create a process-free Claude PTY session with
`testnet::script::session(script).await`. Keep its returned `Provider`
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

`Provider::feed` accepts a decoded input and its validated attachment IDs,
records the input in arrival order and selects the first matching reaction at
or after the cursor, consuming through that reaction. Each observation's `pins`
contains only that input's IDs in their received order; a plain prompt has an
empty list even when earlier prompts carried attachments.
Triggers are `AnyPrompt`, `PromptContains`, `Command`, `Answer`, `Interrupt`
and `Any`. Command triggers match the first slash-command word of a prompt;
answer triggers distinguish permission, question and plan responses. The
capability lists are script metadata. Unknown ask IDs return `UnknownAsk`
without consuming a reaction; unmatched inputs return `Exhausted`.

A prompt received during a turn is observed immediately and played after the
current reaction reaches `EndTurn` and finishes its remaining steps. Deferred
prompts keep arrival order. EndTurn emits a Stop hook followed by one duration
row per prompt, even if repeated. Reactions without EndTurn stay open for answers or
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

Run `just test-crate testnet -- testnet_script -- --nocapture` to see the parsed
transcript and hook capture along with checks for asks, deferred prompts,
turn boundaries and cleanup.

Use `e2e-tests/topologies/scripted-agents.json` for a runnable scripted topology.
Claude script paths, working directories and repository roots resolve relative
to the topology file. Scripts are parsed before any network resources start.
Codex recording directories resolve relative to the topology file as well.

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

`testnet::connect_user(cloud_url, relay, token)` opens a client-only embedded runtime
with the normal routing and client services against the loopback relay. It
supplies the test token directly in place of production token exchange. The
client has an isolated device identity and must pair with the host, even when
both use the same account. Use `StartQrPairing` and the client's QR pairing API;
after the agent appears, `Runtime::note_attached` opens its structured stream.
Run `just test-crate testnet -- testnet_agents -- --nocapture` to see the control
requests, exact host observations and projected transcript from a production
`amux_ui::Runtime` using that connection. The test also checks account isolation,
child asks, invalid controls, exit and restart cleanup.

## Scripted Claude SDK sessions

`e2e-tests/topologies/claude-sessions.json` runs SDK and PTY agents on the same
host. A daemon's optional `sdk_script` names a JSON file with the SDK
`initialization` response and a `reply` string. An agent declared as
`{"ClaudeSdk":{"model":"sonnet"}}` uses that host's script. Missing scripts
are rejected before startup.

The script replaces the provider transport with `claude::sdk::from_io`.
Requests to create SDK agents, including requests from a paired client, still
pass through normal directory validation, backend construction and daemon
registration. Each session initializes independently, answers prompts with
native assistant and result messages, and acknowledges model changes only
for models listed in its initialization response. Other controls return a
provider error. The script does not implement permission dialogs or effort
changes. It is a testnet fixture, compiled out of production builds.

`AgentObserve` accepts a seeded SDK agent's name or any scripted SDK agent's
UUID, including one created after startup. Its `sdk_inputs` contains the raw
stdin envelopes the provider received, including initialization, prompts and
model controls. Seeded PTY agents also accept their original UUID or name;
their typed inputs remain in `observed`. An unknown UUID returns an error. Restart ends the scripted sessions and removes
the daemon's script configuration; it does not launch a replacement provider.

Run `just test-crate testnet -- testnet_sdk -- --nocapture` to exercise paired
creation over the relay, both SDK sessions, the PTY session, model control and
its PTY refusal, and rejected creation without an inventory change. This
tests the real host and shared client runtime; it does not prove the iPhone
view or qualify an authenticated Claude service.

## Convert a report transcript

`target/debug/testnet script-from-report PATH/msgs.jsonl` prints a
Script JSON value. The input uses the normal recorder header and retained Msg
lines. Conversion requires one uninterrupted Claude PTY stream beginning at
sequence 1 and a checkpoint without folded feed history. Lost checkpoint rows
return `EvictedHistory`; a partial, gapped, reopened or mixed-session stream
returns `PartialSession`. Other protocols return `UnsupportedLayer`.

The generated script has one `Any` reaction containing raw `Rows`. Its steps
can be passed directly to `Provider::play`, or its trigger can be replaced in
an authored script. A prompt trigger also produces the scripted provider's
normal prompt echo. Transport keymap and ready markers are regenerated;
semantic `amux.*` rows return `UnsupportedRow` because transcript playback
cannot reconstruct hook decisions. This converter preserves transcript
content; interactive asks and answers need an authored script.

`just test-crate testnet -- script_from_report -- --nocapture` checks the committed
synthetic recorder fixtures and compares the converted rows with output from
a real Claude provider session.

## Strict Codex recordings

Codex recordings require a Unix host, matching the daemon's Codex backend.
On Windows the runner rejects a Codex topology before starting the network;
Claude scripts and the network controls remain available.

`e2e-tests/topologies/codex-recording.json` declares a Codex agent backed by
`crates/codex-specs/fixtures/runtime/approval_allow`. Recording manifests and content hashes
are checked before startup. The runner uses the recorded client handshake and
thread-start parameters, then hands the real Codex session to the daemon's
normal backend. Subsequent prompts and approval answers come from the attached
client and must match the recording. No provider binary or live service runs.

Pair and attach through the relay as for Claude. This fixture expects the prompt
`Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE.`
and an Accept decision on its command approval. The recorded command is data;
playback does not execute it. Its response is `DONE`, followed by turn completion.

Send `{"AgentVerifyReplay":{"agent":"codex"}}` over the control socket after
the recorded turn. An Ack proves that every recorded read was delivered and
every expected write matched, with no extras. Before completion it returns
`replay incomplete`; an unrecorded write returns `ReplayWriteMismatch` with
the expected and actual write. A transport write failure also ends the Codex
connection so requests cannot hang waiting for an impossible response.

Claude-specific controls (emit, ask, turn end, exit, child spawn and decoded
input observation) refuse Codex recordings. Verification is the Codex host
observation boundary. Restart and shutdown close its replay transport and driver.
Only single-transport recordings beginning with initialize, initialized and
thread/start are supported; other recording shapes are refused.

`just test-crate testnet -- testnet_codex_recording -- --nocapture` drives the
production UI runtime over the relay, prints the approval and projected feed,
verifies all five recorded writes, and checks that an unrecorded prompt or
answer produces a named mismatch and settles its input without hanging.
