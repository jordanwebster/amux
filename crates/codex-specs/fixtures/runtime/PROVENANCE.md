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
