# Codex recording provenance

Each directory here is one scenario recorded against the real Codex app-server
by `codex-probe record <name>`. The sanitizer replaces machine paths and
credentials while preserving provider IDs, timestamps, notification payloads
and response ordering, so replay is deterministic offline and keeps the full
provider-visible event shapes.

## Inject drain

`crates/codex-specs/fixtures/runtime/inject_drain` was captured with
codex-cli 0.157.0 and explicit model `gpt-5.6-luna` by
`codex-probe record inject_drain`, through the same sanitizer. It pins that
an item sent with `thread/inject_items` while a command runs is acknowledged
at once with an empty result, produces no item of its own, and is answered by
the running turn under the same turn id before that turn completes.

## Chat rows

`reasoning_summary`, `exploring`, `failing_command`, `file_changes`, `image`,
`compaction`, `plan_mode`, `turn_error` and `signed_out` under
`crates/codex-specs/fixtures/runtime/` were captured with codex-cli 0.157.0
and `gpt-5.6-luna` by `codex-probe record <name>`, through the same sanitizer.
Capture seeds the project with `config.txt`, `old.txt` and `square.png` (a
32×32 red PNG, `crates/codex-specs/assets/square.png`). `turn_error` asks for
a model the account cannot use; `signed_out` runs with a Codex home that has
no credentials. The plan tool could not be recorded: asked by name, the
capture model answers without calling it.

## Two clients

`two_clients_prompt`, `two_clients_approval` and `two_clients_steer` were
captured with codex-cli 0.160.0 and `gpt-5.6-luna` by
`codex-probe record <name>`, through the same sanitizer. Capture starts
`codex app-server --listen unix://…` and connects two clients, amux and
another standing in for Codex's own app, over the socket's WebSocket
framing; each line carries the client it went to or came from as
`transport_id` (`amux` or `other`), and replay feeds each client its own
lines. amux starts the thread and names it, which lets a client resuming
with the turns join a thread with no turn. They pin that the server sends every
notification and approval request to both clients, echoes each prompt and
steer with the sender's client message id (`clientId`), tells both clients
an approval was resolved when either answers, and answers the joining
client's `thread/resume` with a `deprecationNotice` and both clients with
`thread/goal/cleared`.

`two_clients_join_fresh` was captured the same way and runs no turn. The
other client resumes as Codex's own app does, with `excludeTurns`, and is
refused (`-32600`, "invalid paginated history lineage for <thread>: missing
source rollout"): the app pages a thread's turns from its history on disk,
which Codex writes at the first turn, and naming the thread does not write
it. amux then reads the thread with `thread/read` and `includeTurns`, which
writes it (answered with a `deprecationNotice` beside the thread), and the
same resume joins.
