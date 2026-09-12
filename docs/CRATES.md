# Crate boundaries

| Layer | Crates |
| --- | --- |
| Values and protocol | `model`, `wire`, `settings`, `artifacts`, `redaction`, `client` |
| Daemon | `host-api`, `node`, `agent-runtime`, `claude`, `codex`, `pty-host` |
| Clients | `ui-state`, `ui-runtime`, `tui` |
| Products and tools | `amux`, `shot`, `xtask` |
| Test infrastructure | `testnet`, `replay-support`, `claude-specs`, `codex-specs`, `e2e-runner`, `test-agent` |

The values and protocol layer keeps shared meaning below effects. `model`
owns provider-neutral values without I/O; `wire` owns protobuf schemas,
committed generated code and conversions. `settings` owns persisted
configuration, `artifacts` owns content-addressed storage, `redaction` owns
sanitization, and `client` provides typed RPC clients over supplied channels
or endpoints.

The daemon layer separates network and installation ownership from provider
processes. `host-api` is the asynchronous boundary: `node` owns identity,
trust, routing, admission, services and installation lifecycle, while
`agent-runtime` owns provider sessions, persistence, attachments, diffs and
artifact retention. The provider and PTY crates implement those sessions.
`node` has no private `src/testnet` harness; whole-daemon behavior belongs to
`testnet`.

The client layer separates pure state from effects and presentation.
`ui-state` is the reducer, `ui-runtime` owns per-view connections,
subscriptions, effects and report resources, and `tui` owns terminal input
and rendering. TUI fixtures live inside `tui` and compile only for its tests
or explicit fixture consumers; they are not a separate package.

The product and tools layer composes rather than re-exports the lower layers.
`amux` owns desktop setup and the CLI, `shot` renders deterministic TUI
evidence, and `xtask` generates committed wire output. An embedded
`node::Installation` may omit the host factory and therefore starts no
provider runtime.

The scenario and executable support packages consume production APIs, never
the reverse. `testnet` is the public harness for whole-daemon prose specs,
cross-crate integration, embedded ownership, and live-provider entry points;
the provider spec crates own recordings, while `e2e-runner` and `test-agent`
drive real-process scenarios. `replay-support` is the deliberate exception: it
owns replay transports shared by `claude` and the spec packages, and its normal
edge from `claude` is accepted in the shipping dependency graph. Run `just
dependency-policy` to compare the protected edges with the declared allowlist.
