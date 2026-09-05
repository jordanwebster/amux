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
