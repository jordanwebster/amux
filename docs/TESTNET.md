# TestNet

*For developers writing tests that span several hosts, and for anyone driving a real client against a served test network.*

TestNet (`crates/testnet`) starts a declared network of amux hosts, breaks it in controlled ways and observes the
consequences. The same network runs inside a Rust test, where the test holds a `Net` and calls its verbs, or in
its own process as `testnet serve`, where a driver sends the same verbs over a control socket. Terminal and phone
journeys, the phone's goldens and the flood performance workload all run on it. How it fits among the other
suites is on [Testing](TESTING.md).

## What is real

Each host is a production daemon runtime (`node`) started inside the harness process, on its own temporary
installation with its own identity, trust store, profile store and front door socket. Agents are real `amux
agent` processes, running the scripted fake providers from `crates/provider-fakes` (`fake-claude-pty`,
`fake-claude-sdk`, `fake-codex`) in place of Claude and Codex. Links between hosts are real authenticated QUIC
links over loopback. A declared relay is the production relay server on loopback, beside a stand-in for the
account service that signs accounts in and mints relay credentials.

What is replaced: the providers (by the fakes), the account service (by the stand-in), multicast discovery (by a
scripted bus, unless a host asks for real mDNS), and, in a Rust test, policy time (by a driven clock).

Because the daemons are runtimes inside one process, "kill a daemon" drops its runtime rather than killing a
process; the agent processes do keep running, as they would. A claim that the `amux` binary itself survives or
restarts belongs to the process tests in `crates/amux/tests`.

On first use in a process, the harness builds `amux` and the fake providers with the same Cargo and profile that
built the test, so agents always run the binary under test.

## Topologies

A `Topology` declares the hosts, the links between them, an optional relay with its accounts, and the agents to
start. The Rust builder and the JSON loader produce the same value, and `Topology::validate` rejects unknown and
duplicate names before anything starts.

```rust
let topology = Topology::new()
    .host("desk")
    .host("laptop")
    .link("desk", "laptop")
    .agent(AgentDecl::new("worker", "desk").prompt("go").steps(vec![
        Step::Text { chunks: vec!["before".into()] },
        Step::WaitFor { path: "release".into() },
        Step::TurnEnd,
    ]));
```

The same network as JSON, as `testnet serve` reads it:

```json
{
  "hosts": [{"name": "desk"}, {"name": "laptop"}],
  "links": [{"a": "desk", "b": "laptop"}],
  "agents": [
    {"name": "worker", "host": "desk", "prompt": "go",
     "script": {"steps": [{"text": {"chunks": ["before"]}}, {"wait_for": {"path": "release"}}, "turn_end"]}}
  ]
}
```

| Field | Meaning |
| --- | --- |
| `scope` | The discovery scope every host advertises and filters by, unless a host names its own. |
| `hosts[].name` | Letters, digits, `-` or `_`; unique. |
| `hosts[].lan` | Listen for direct links on a loopback QUIC listener. |
| `hosts[].discovery` | Advertise and browse on the net's scripted discovery bus, and dial trusted hosts found there. |
| `hosts[].bonjour` | Advertise on the machine's real local network through the system's mDNS responder, for a client outside the net (a simulator) to find. Needs `lan` and not `discovery`. |
| `hosts[].scope` | This host's discovery scope, where it differs from the net's. |
| `hosts[].account` | The relay account this host's profile signs in to at start; needs a `relay`. |
| `hosts[].script` | What agents a client creates on this host play; none plays nothing. |
| `hosts[].repositories` | Git repositories made under the host's repository root, by path below it. |
| `hosts[].files` | Files committed in those repositories when they are made, by path below the root (`amux/README.md`), so an agent's edits show as a working-tree diff. |
| `links[]` | `{"a", "b"}`: two hosts that trust each other and are linked at start. |
| `relay.accounts[]` | `{"name", "tier"}`, tier `pro` (the default: relayed tunnels) or `free` (hosts are listed; the relay opens no tunnels). |
| `settle` | Start each declared agent only once the one before it has settled (see `settle(name)` below), so agents that come to rest in the same standing are listed in declaration order, the last declared as most recently active. Every declared agent must come to rest. Without it the agents start together and which first turn ends last is a race. |
| `agents[].name`, `host` | Unique name; the host it runs on. |
| `agents[].kind` | `claude_pty`, `claude_sdk` (the default) or `codex`: which fake runs and which interpreter reads it. |
| `agents[].script` or `script_file` | What the fake plays, inline or from a file relative to the topology file; not both. |
| `agents[].prompt` | The first prompt, sent at creation. |
| `agents[].parent` | An agent declared earlier, on any host. |
| `agents[].cwd` | Its working directory; none is the host's work directory, and a relative path a folder below it. |
| `agents[].repository` | One of its host's repositories to work in, instead of `cwd`. |

JSON topologies live in `journeys/topologies`; scripts shared between them in `journeys/scripts`.

### Scripts

A script is the model's side of a session; the fake supplies the provider's own protocol (handshakes, echoes,
queueing, control replies), so every script gets the same provider behaviour. Steps run in order: when idle, the
fake waits for a prompt or an injected message, then plays steps until `turn_end`.

| Step | Effect |
| --- | --- |
| `text` | Streamed assistant text, chunk by chunk where the provider streams. |
| `thinking` | Reasoning before the next step. |
| `tool` | A tool call that runs without asking. |
| `ask` | Something the host must answer first: `permission`, `question`, `plan`, `form`, `link`, `grant` (Codex) or `tool_server_dialog` (terminal Claude). |
| `wait_for` | Hold the turn until a file exists; a relative path names a file in the net's gates directory. |
| `pause` | Stay busy for some milliseconds. |
| `turn_end` | Finish the turn. |
| `exit` | Exit the provider process with a code. |
| `repeat` | Play some steps a number of times, without writing them out. |

A script can also set the `model`, the `models` and `commands` a session offers, and, for terminal Claude,
`offers_auto_mode` and `untrusted_folder`. A script that asks a provider for an ask kind it cannot raise fails to
load.

## The net in a Rust test

`Net::start(topology)` starts the relay and every host, signs hosts in to their accounts, trusts and links the
declared pairs, waits until each link carries traffic both ways, and spawns the declared agents, each returning
once its process has said hello (or, when the topology sets `settle`, once it has settled). When it returns the net is ready. `Net::start_with(topology, NetOptions)` takes a clock
mode, a shared `DrivenClock`, hooks that adjust each host's edge or launch parameters (the tail size, a
retention budget), and a fixed root directory.

The harness owns resources, verbs and observations, never scenarios. Scenarios live in the tests.

**Verbs** change the world and return an `Ack { installed, at_ms }` once the change is in place.

| Verb | Effect |
| --- | --- |
| `sever_link(a, b)`, `restore_link(a, b)` | Cut a link the way a dead connection goes (no close, the carrier stops), and bring it back. |
| `trust(a, b)`, `untrust(a, b)` | Trust as pairing would, without linking; forget, as unpairing does. |
| `kill_daemon(host)` | Crash the host's daemon; its agent processes keep running in their grace. |
| `stop_daemon(host)` | Shut it down cleanly; its agents keep running. |
| `restart_daemon(host)` | Start it again from what its installation holds, under the same boot, and relink. |
| `checkpoint_host(host)` | Record what has reached the drive: the store flushed and its file as it stands. |
| `rewind_host(host, cuts)` | Power loss: daemon and agents die at once, the store goes back to the checkpoint without its write-ahead log, each named journal is cut at a byte, and the host returns under a new boot id. |
| `advance(by)` | Move policy time on every host (driven nets only). |
| `spawn(decl)`, `resume(name, text)` | Start an agent; start an exited agent's next incarnation. |
| `settle(name)` | Wait until the agent rests on its host: idle or needing the person, nothing queued, so a creation prompt's turn has run. The fleet lists agents of one standing most recently active first; settling each before starting the next fixes that order. |
| `send(name, text)`, `input(...)` | Send a prompt, or any input (an answer, a withdrawal, an interrupt), through the agent's own host. |
| `stop(name)` | Stop the agent on its own host, as a person does; it can be resumed. |
| `delete(name)`, `delete_family(name)` | Delete an agent, or delete it with its children and report what the cascade reached. |
| `spawn_child(parent, decl)` | Spawn through the parent's tool socket, naming the child's host. |
| `next_start(name, script)` | Give the next process the host starts for `name` this script, for a resume the host makes itself. |
| `freeze(name)`, `thaw(name)` | Stop an agent process where it stands, holding its lock and connection, and let it run again. |
| `sign_in(host, account)`, `set_tier(account, tier)` | Sign a host in through its front door; change what an account has bought. |
| `block_udp(host, blocked)` | Drop every datagram on the host's way to the relay, or stop dropping them. |
| `open_gate(name)` | Create a gate file, releasing every `wait_for` step waiting on it. |

**Observations** wait for consequences and return what they saw. `observe(host, agent, tail)` opens a Subscribe
stream as a client would and records it; `observe_inventory(host)` does the same for the inventory. Their
`observe_until(predicate, deadline)` passes only when the predicate holds: it fails with `Stuck::Deadline` when
the deadline passes and with `Stuck::Closed` at once when the stream ends. `observe::eventually` and
`observe::holds_for` poll with the same rule, and a check that never answers fails too. `PATIENCE`, 30 seconds, is
the usual deadline; real work on loopback settles well inside it. `assert_block_invariant(host, agent)` checks a
replica's rows against its origin: empty, or one contiguous block ending at the origin's newest row at the
origin's revisions.

**Resources** answer questions about the net: `host(name)` (its data directory, front door and installation
config), `runtime(name)`, `agent_dir(name)`, `journal_end(name)`, `provider_input(name)` (every line the stdio
fakes read), `client(host)` and `tools(name)` (the client service as a person or an agent reaches it), and
`relay()`.

A test ends with `net.shutdown().await`, which stops every agent and shuts every daemon down cleanly. Dropping a
net kills what it can and is for panics.

```rust
let mut net = Net::start(topology).await?;
let mut chat = net.observe("desk", "worker", 50).await?;
chat.observe_until(|events| says(events, "before"), PATIENCE).await?;
net.kill_daemon("desk").await?;
net.open_gate("release")?;          // the agent writes on with no daemon
net.restart_daemon("desk").await?;
let mut chat = net.observe("desk", "worker", 50).await?;
chat.observe_until(|events| says(events, "after"), PATIENCE).await?;
net.shutdown().await?;
```

### The driven clock

A net started with `Net::start` runs every daemon's policy timers (retention, outbox retries and notification
delays, credential refresh, reply and start deadlines) on one `DrivenClock`. It starts at the wall time the net was
built, so certificates and credentials minted against it look current, and moves only on `advance`.
`DrivenClock::armed(at_ms)` resolves once something sleeps until exactly that moment, so a test can step past a
deadline knowing it was set. Relay credentials expire on the same clock (`CREDENTIAL_TTL`, ten minutes).

Transports stay on real time: a QUIC idle timer or a socket read is not policy. Never pause the async runtime
around real IO. `testnet serve` and the flood run on wall time, where `advance` is refused.

## `testnet serve`

```sh
cargo build -p testnet --bins          # or `just ios tools`, which builds it with the phone's tools
target/debug/testnet serve journeys/topologies/terminal-stories.json
target/debug/testnet serve TOPOLOGY --control 127.0.0.1:7000 --root-in /tmp/fixed
```

`serve` loads and validates the topology, starts it on wall time and, only once the net is ready, prints one
readiness line of JSON on standard output. `--control` fixes the control socket's address (an ephemeral loopback
port by default). `--root-in DIR` puts the net's root at `DIR/net` instead of a fresh temporary directory, so every
path a client draws is the same each run; the directory must not already hold a `net`. With `RUST_LOG` set, the
daemons' tracing goes to standard error.

### Readiness

```json
{
  "control": "127.0.0.1:53211",
  "root": "/tmp/testnetAbc123",
  "gates": "/tmp/testnetAbc123/gates",
  "hosts": [
    {"name": "desk", "host_id": "…", "profile": "…",
     "front_door": "/tmp/testnetAbc123/desk/door.sock",
     "config": "/tmp/testnetAbc123/desk/installation.yaml"}
  ],
  "agents": [{"name": "worker", "host": "desk", "id": "…", "kind": "claude_sdk"}],
  "cloud_url": "http://127.0.0.1:53212",
  "relay_tcp": "127.0.0.1:53213"
}
```

Each host lives under `<root>/<name>/`: its installation in `data/`, its agents' working directory in `work/`,
and `door.sock` and `installation.yaml` beside them. A host's `config` is that installation config, so the real terminal client runs against it
with `amux --config <config>`. `cloud_url` and `relay_tcp` appear only when the topology declares a relay:
`cloud_url` is the account-service stand-in, where `refresh-<account>` is the login for a declared account, and
`relay_tcp` is the relay's plain TCP carrier for a client outside the net.

### The control door

Connect to `control` over TCP and send one JSON request per line; each gets one reply line, in order. Several
drivers may connect at once. Requests use the verb as the key (`{"Sever": {"a": "desk", "b": "laptop"}}`), and a
verb without fields is a bare string (`"Shutdown"`).

| Request | Effect and reply |
| --- | --- |
| `{"Sever": {"a", "b"}}`, `{"Restore": {"a", "b"}}` | Cut or restore a declared link. |
| `{"Link": {"a", "b"}}` | Whether both ends route to each other directly: `{"up": true}`. |
| `{"Trust": {"a", "b"}}`, `{"Untrust": {"a", "b"}}` | Trust as pairing does; forget as unpairing does. |
| `{"SetTier": {"account", "tier"}}` | Change what an account has bought (`pro` or `free`). |
| `{"SignIn": {"host", "account"}}` | Sign a host's profile in to a declared account. |
| `{"KillDaemon": {"host"}}`, `{"StopDaemon": {"host"}}` | Crash or cleanly stop a host's daemon. |
| `{"RestartDaemon": {"host"}}` | Kill it if it is running, then start it again. |
| `{"Checkpoint": {"host"}}`, `{"Rewind": {"host", "cuts": [{"agent", "byte"}]}}` | Record what reached the drive; lose power back to it. |
| `{"Advance": {"ms"}}` | Refused: a served net runs on wall time. |
| `{"Spawn": {"agent": {…}}}` | Start an agent declared as in a topology: `{"id": "…"}`. |
| `{"Resume": {"agent", "text"}}` | Start an exited agent again, optionally with a prompt: `{"incarnation": n}`. |
| `{"Send": {"agent", "text"}}` | Send a prompt through the agent's host: `{"verdict": "…"}`. |
| `{"Stop": {"agent"}}` | Stop the agent on its host, as a person does; it can be resumed. |
| `{"Freeze": {"agent"}}`, `{"Thaw": {"agent"}}` | Stop the agent's process where it stands, answering nothing, and let it run again. |
| `{"OpenGate": {"name"}}` | Release `wait_for` steps waiting on that file. |
| `{"Inventory": {"host"}}` | The host's fleet once its inventory has caught up: `{"agents": [{"name", "id", "host_id", "lifecycle", "phase"}]}`. |
| `{"Chat": {"host", "agent"}}` | Everything the host holds of a chat, read to its first CaughtUp: `{"items": [{"key", "order", "text", "input_id", "attachments"}], "phase"}`. `agent` is a declared name or, for an agent a client created, its id. |
| `{"Block": {"host", "agent"}}` | Check the replica block invariant: `"holds"`. |
| `{"ProviderInput": {"agent"}}` | Every line the agent's provider read: `{"lines": […]}`. |
| `"Shutdown"` | Stop every agent and daemon, remove the net's root, reply, and exit. |

A reply is `{"ok": <value>}`, where a verb's value is usually its acknowledgement
(`{"installed": "link desk - laptop severed", "at_ms": …}`), or
`{"error": {"kind", "message"}}`, where `kind` tells a driver's own mistake from the net's refusal:

| Kind | Meaning |
| --- | --- |
| `invalid` | The request did not parse, or named a host, agent or link the net does not have. |
| `refused` | The net refused: a host down or running, no checkpoint, wall time. |
| `stuck` | An observation did not see its consequence in time. |
| `violation` | The replica block invariant does not hold. |
| `closed` | The net has shut down. |

An error does not undo a verb that had already started.

`Shutdown` is the clean end. An interrupt (Ctrl-C) also shuts the net down; in both cases the root directory is
removed, so copy anything you need (a dump, a log) out of it first.

Every door verb is a net capability or a declared composition of two. The map is `door::CAPABILITIES`, the crate
documentation in `crates/testnet/src/lib.rs` lists the same table, and a test holds the two to each other. Adding a
verb means adding the capability to `Net` first.

## The harness's own tests

```sh
just test -- --test harness                  # topology validation, clock, waits, faults, the door
just test -- --test spec_edge --test spec_families --test spec_inventory --test spec_network --test spec_replication
```

The `spec_*` targets are the many-daemon specifications: pairing, trust, discovery, routing, relay carriers,
credential refresh, revocation, account isolation, the inventory, replication and agent families across hosts.
Each reads top to bottom as a description of one behaviour, with its assertions in the test body.
