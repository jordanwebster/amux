# Native runtime bridge

`app-ffi` is the C ABI over two crates any rich client reuses: `app-runtime`
(account sessions, the presentation projection, the per-account store and the
frame-coalesced event queue; it never links the node) and `app-embedded` (the
owner of a provider-free embedded node installation and its relay link).
Nothing outside `app-ffi` knows a C type exists. `amux_app_start` returns an opaque handle immediately; a dedicated
Rust thread owns its executor, network tasks and ordered event callbacks.
`amux_app_stop` cancels network and token work and joins that thread. After
stop returns the application can release its callback context.

Start accepts a NUL-terminated UTF-8 JSON object:

```json
{
  "data_dir": "/app/data",
  "cache_dir": "/app/cache",
  "device_name": "My iPhone",
  "relay": {
    "url": "https://relay.example:443",
    "tls": "System",
    "token": "Callback"
  },
  "log_path": "/app/logs/amux.log"
}
```

The paths must be absolute. The data directory holds this installation's
identity and trust. The relay URL is the resolved routing endpoint. The
application obtains routing tokens through its account API; the embedded
server owns authentication, token refresh and reconnection to that endpoint.
`System` uses the shared TLS transport and requires an HTTPS origin.

For isolated journeys, build with `debug-tools`, use `PlainLoopback` and an
`http://127.0.0.1:PORT` (or literal IPv6 loopback) URL. Hostnames and non-loopback
addresses are refused. Builds without `debug-tools` reject `PlainLoopback`,
including when compiled in a debug profile. `{"Static":"routing-token"}` can
replace `"Callback"` for a test credential.

Callbacks receive one JSON array, with externally tagged events such as:

```json
[{"TokenRequest":{"request_id":1}}]
```

Answer with `amux_app_token_reply(handle, 1, json)`, passing
`{"token":"routing-token","expires_at":1788652800}` or `{"error":"reason"}`.
Expiry is optional and expressed as Unix seconds. Unknown or duplicate request
IDs are ignored; malformed replies fail their request. An unanswered request
times out after 30 seconds. Connection events describe relay connectivity,
separately from the always-local client service. Fleet events carry the
shared reducer's current agents and hosts.

The callback's bytes are borrowed only until the callback returns. Copy them
and schedule UI work on the application's own thread. Callbacks may arrive
before start returns. Never call stop from a callback, and finish all other
calls using the handle before stopping it. Start returns null for invalid
configuration or failure to create a worker; asynchronous failures arrive as
connection events. The generated header documents pointer lifetimes.

Run `just test-crate app-ffi` for the C-boundary relay, reconnection, token
and teardown tests; `just test-crate app-runtime` covers the projection and
queue with no node linked. `just ios graph-check` proves the shipping library
carries no provider, agent-runtime or test crate.

`just ios rust` builds the one slice a development build links — the
simulator, with the driving tools, under the `dev` profile — and packages it
as `target/ios/AmuxAppDebugTools.xcframework` only when the archive or its
generated header changed; when no Rust input changed it runs no cargo at all.
`just ios package` builds the ARM64 device and simulator libraries under the
workspace `mobile` profile with an explicit iOS 26.0 deployment target,
packages their generated headers and Clang module maps as
`target/ios/AmuxApp.xcframework`, then compiles
`apps/apple/Tools/LinkageSmoke.swift` against the packaged simulator slice and runs
it on the pinned simulator.

The smoke prints `amux_app_version` and checks that the shipping library
rejects `PlainLoopback`. Its output is saved in `target/ios/simulator-linkage.txt`.
`target/ios/size.txt` records archive sizes and the mobile profile settings;
these are library sizes, not installed application size. Cargo caches these builds;
archive assembly bypasses compiler wrappers so native object changes cannot
be lost behind cached Rust metadata. The Swift smoke treats linker warnings
as failures.

Projection callbacks contain `Fleet`, `Feed`, `Session`, `OpResult`, `Diff`,
`Connection`, `TokenRequest`, diagnostic `Invariant`, and terminal
`StoreFailure` events. A store failure is the runtime's final event and names
both the cause and the remedy after network and chat work has stopped. Their
JSON contract is pinned in `src/projection/schema.json`. Fleet cards contain only
inventory and display facts, never retained transcripts. Session gates, phases,
asks and facts come from the shared native provider layer. Claude PTY, Claude
SDK and Codex rows retain their own typed vocabularies under `layer:
"claude_pty"`, `layer: "claude_sdk"` and `layer: "codex"`. The SDK layer carries
its native session facts and send gates; it cannot accept a PTY row.

Call `amux_app_dispatch` with a shared `ui_state::Command` JSON object, or
`{"command":"subscribe","agent":"UUID"}` to receive that agent's session and
feed. It returns an owned UUID string matched by an `OpResult` event; release
the string with `amux_app_free`. Unknown or malformed commands return an
operation error. `unsubscribe` ends projection delivery; the shared reducer may
keep observing the agent for fleet attention. Discard the phone's feed when
unsubscribing and start empty on the next subscription.

Apply each Feed to a map of absolute positions in this order:

1. Remove every position below `evicted`.
2. Apply each `[position, row]` in `replace` to an existing position.
3. Insert `append` starting at `base`.

These positions are distinct from the native row's `id`. They keep increasing
when a replay replaces its observation window and reuses native IDs. Initial
subscription appends the retained window once. Later batches serialize only
new or changed rows; no full-feed replacement shape exists. Apply batches in
callback order and use the positions as row identities.

Set `frame_interval_ns` in the start configuration to the display's requested
interval (the default is 16,666,667 ns). Update it as the display rate changes
with `amux_app_set_frame_interval`. There is no fixed 60 Hz cap. Emission
follows the previous callback's completion, so delayed work does not cause
catch-up bursts. An idle runtime does not wake on a frame timer. Copy bytes
in the callback and return promptly so applying them can fit the next frame.

`just test -- mobile_projection` pins the JSON, reconstructs a feed
through replacements, eviction and replay, checks command errors through the
C callback, and streams 1,000 rows at 50 per second under deterministic virtual
time. That bench proves cadence and delta payload size; it does not measure
Swift rendering or presented frames on a simulator or phone.

Each account has a SQLite store at
`<cache_dir>/store/<escaped account>.sqlite`. Before starting a runtime,
`amux_app_cached_fleet` reads that store and returns an owned JSON array
containing one unreconciled Fleet event. A missing store returns that event
with no rows because the cache is disposable. An existing store that cannot
be opened or read returns `{"error":STRING}` with the same cause and remedy as
the running runtime's `StoreFailure`; `NULL` is reserved for arguments that
cannot be read as strings. Release the returned string with `amux_app_free`.

The running library installs those remembered cards in the shared reducer,
marked as awaiting their machine and with send gates closed. The event queue
withholds its first fleet callback until the store rows have been installed, so
the runtime cannot blank the fleet the application already drew while it
connects. A remote machine's completed inventory confirms or removes its
remembered cards, including removing all of them. Unpairing removes that
machine's rows after the local machine list completes. Local agent-list
completion and relay connectivity never prove remote inventory membership;
unreachable paired machines keep their remembered rows. Untrusted pairing
candidates are excluded from the fleet.

`amux_app_snapshot` returns the shared reducer Model as owned JSON.
Its hosts map includes online unpaired hosts advertised through this account's
relay. The embedded runtime subscribes through its owner administration handle;
profile sockets and peer tunnels expose only trusted hosts. Pairing candidates
stay outside Fleet callbacks and the stored fleet until trust is confirmed.
In `debug-tools` builds, `amux_app_seed_store` replaces one account's store
with the hosts, agents, removals and conversation rows described by its JSON
argument, using the same store and runtime paths as the phone. It returns
owned JSON `{"ok":true}` or `{"error":"…"}`. `amux_app_cached_chat` opens one
stored conversation before any connection and returns the projected
`{"events":[…]}` or `{"error":"…"}`. Both results must be released with
`amux_app_free`.

Also in `debug-tools` builds, `amux_app_report_snapshot` returns
`{"msgs":{"format_version":3,"checkpoint":MODEL,"invariant_violation":BOOL,
"msgs":[JSON_LINE,...]},
"daemon":JSON_STRING_OR_NULL,"daemon_absent_reason":STRING_OR_NULL}`.
The recorder freezes before the embedded daemon dump request. To form
`msgs.jsonl`, write a header with `format_version`, `checkpoint` and
`invariant_violation`, then each message string on its own line. The daemon
string is the contents of `daemon.json`; it describes the embedded phone
service and its remote routes.
If the dump fails or takes more than three seconds, the recorder still returns
with an explicit absence reason. These calls wait up to five seconds for the
worker; call them outside the event callback, finish before stop, check for
null and release returned strings with `amux_app_free`.

`amux_app_replay_report` takes the path of a
`msgs.jsonl` written that way and returns `{"events":[EVENT,...]}` — the same
projected events a live connection delivers — or `{"error":STRING}` when the
file cannot be read or folded. It needs no handle and starts nothing: the
recorded messages are folded by the shared reducer and the result is projected,
so none of the effects the recording once asked for are carried out. Every
agent in the recorded model is subscribed, so the batch carries its
conversations as well as its fleet, and the projection is told the relay is
connected because a recording holds no connection of its own — reconciliation
then follows what the recorded model itself synchronized to. Release the result
with `amux_app_free`.

`just test -- mobile_cache` restarts a paired bridge offline, verifies
its first callback and stable ordering on reconnect, checks rename persistence,
prunes offline deletions and unpaired hosts without reordering survivors,
and replays an exported recorder snapshot through the shared reducer.

`just ios loopback-smoke` builds the driving simulator library under the
`dev` profile, stages it separately in `target/ios/loopback`,
and compiles `apps/apple/Tools/LoopbackSmoke.swift`. The recipe starts the testnet
runner with two real Mac daemons and passes its relay address and temporary
bearer token to Swift on the pinned simulator. Swift prints the online daemon
names and identities from the shared reducer snapshot, verifies that these
unpaired relay hosts stay outside Fleet callbacks, then stops its Rust worker.
Snapshot reads run outside callbacks so they cannot deadlock the Rust worker.
The recipe compares those identities with runner readiness, requires a
nonempty inventory, shuts the runner down, and verifies successful process
exit, released relay/control listeners and removal of temporary state. It
restores the simulator's previous boot state. The passing capture lives in
`target/ios/loopback-smoke.txt`. This proves relay inventory from iOS; pairing
and agent interactions have their own journeys.
