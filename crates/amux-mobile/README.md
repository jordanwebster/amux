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
