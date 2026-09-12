# Crate boundaries

The workspace uses one lockfile and explicit members. `model` owns shared values
without I/O. `wire` owns protobuf messages, committed generated code and
encoding. `settings` owns persisted configuration values, `artifacts` owns
content-addressed bytes, and `redaction` owns the sanitizer shared by production
diagnostic reports and support tooling.

`client` connects to an explicitly supplied channel or endpoint. `ui-state` is
the pure reducer. `ui-runtime` owns connections, effects, subscriptions and
report resources for one view instance. `tui` owns terminal rendering and input.

`host-api` is the owned asynchronous contract between `node` and
`agent-runtime`. `node` owns identity, trust, routing, admission and installation
lifecycle. `agent-runtime` owns providers, sessions, persistence, attachments,
diffs and artifact retention. The `amux` package composes them for desktop; an
embedded node supplies no host factory.

`replay-support`, `testnet`, `claude-specs`, `codex-specs` and `tui-fixtures`
own reusable test scenarios, fixtures and their runners. They consume
production APIs; product and build dependencies may not reach them, including
through default features. TUI unit tests compile the support-owned fixture
source in the TUI test crate because a Cargo dev edge would create a package
cycle. Provider packages expose no profile-selected spec API. Run
`just dependency-policy` to validate the boundary.

Node still has a private `src/testnet` white-box harness for tests that exercise
its internal service state. It is compiled only for node's own unit tests and is
not a reusable scenario API. Cross-package scenarios, recordings and embedded
ownership tests live in the support packages above.
Agent-runtime's opt-in test surface consists of opaque adapters over its private
provider constructors. The support package owns waits, row accumulation,
assertions and lifecycle orchestration.
