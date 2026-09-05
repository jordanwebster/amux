# Native runtime bridge

`amux-mobile` embeds the repository's server and `amux-ui` reducer behind a
C ABI. `amux_mobile_start` returns an opaque handle immediately; a dedicated
Rust thread owns its executor, network tasks and ordered event callbacks.
`amux_mobile_stop` cancels network and token work and joins that thread. After
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

Answer with `amux_mobile_token_reply(handle, 1, json)`, passing
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

Run `timeout 900 wt test -- mobile_lifecycle` for the C-boundary relay,
reconnection, token and teardown tests. `timeout 900 wt run mobile-check`
checks device and simulator builds without local agent providers.

`timeout 1800 wt run ios-rust` builds the ARM64 device and simulator libraries
under the workspace mobile profile with an explicit iOS 26.0 deployment target
and packages their generated headers and
Clang module maps as `target/ios/AmuxMobile.xcframework`. It then compiles
`ios/Tools/LinkageSmoke.swift` against the packaged simulator slice and runs
that executable on `amux-golden` (iPhone 17 Pro, iOS 26.5), creating the
simulator if it is absent. A simulator booted by this invocation is shut down
afterward; an already running one is left running.

The smoke prints `amux_mobile_version` and checks that the shipping library
rejects `PlainLoopback`. Its output is saved in `target/ios/simulator-linkage.txt`.
`target/ios/size.txt` records archive sizes and the mobile profile settings;
these are library sizes, not installed application size. Cargo caches these builds;
archive assembly bypasses compiler wrappers so native object changes cannot
be lost behind cached Rust metadata. The Swift smoke treats linker warnings
as failures.

Projection callbacks contain `Fleet`, `Feed`, `Session`, `OpResult`, `Diff`,
`Connection`, `TokenRequest` and diagnostic `Invariant` events. Their JSON
contract is pinned in `src/projection/schema.json`. Fleet cards contain only
inventory and display facts, never retained transcripts. Session gates, phases,
asks and facts come from the shared native provider layer. Claude PTY and Codex
rows retain their own typed vocabularies under `layer: "claude_pty"` and
`layer: "codex"`. The current shared Claude SDK layer is explicitly unsupported;
its distinct row type cannot accept a PTY row.

Call `amux_mobile_dispatch` with a shared `amux-ui::Command` JSON object, or
`{"command":"subscribe","agent":"UUID"}` to receive that agent's session and
feed. It returns an owned UUID string matched by an `OpResult` event; release
the string with `amux_mobile_free`. Unknown or malformed commands return an
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
with `amux_mobile_set_frame_interval`. There is no fixed 60 Hz cap. Emission
follows the previous callback's completion, so delayed work does not cause
catch-up bursts. An idle runtime does not wake on a frame timer. Copy bytes
in the callback and return promptly so applying them can fit the next frame.

`timeout 900 wt test -- mobile_projection` pins the JSON, reconstructs a feed
through replacements, eviction and replay, checks command errors through the
C callback, and streams 1,000 rows at 50 per second under deterministic virtual
time. That bench proves cadence and delta payload size; it does not measure
Swift rendering or presented frames on a simulator or phone.
