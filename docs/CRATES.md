# Crate boundaries

The workspace uses one lockfile and explicit members. `model` owns shared values
without I/O. `wire` owns protobuf messages, committed generated code and
encoding. `settings` owns persisted configuration values, and `artifacts` owns
content-addressed bytes.

`client` connects to an explicitly supplied channel or endpoint. `ui-state` is
the pure reducer. `ui-runtime` owns connections, effects, subscriptions and
report resources for one view instance. `tui` owns terminal rendering and input.

`host-api` is the owned asynchronous contract between `node` and
`agent-runtime`. `node` owns identity, trust, routing, admission and installation
lifecycle. `agent-runtime` owns providers, sessions, persistence, attachments,
diffs and artifact retention. The `amux` package composes them for desktop; an
embedded node supplies no host factory.

`testnet`, `claude-specs`, `codex-specs` and `tui-fixtures` own reusable test
infrastructure. Product graphs may not depend on them. Run `wt run
dependency-policy` to validate the boundary.
