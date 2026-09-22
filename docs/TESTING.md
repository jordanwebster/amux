# Testing

`just --list` is the command catalogue. Recipes share the workspace lockfile
and carry outer timeouts; a timeout is a hang to diagnose, not a reason to
lengthen a deadline.

This document says what each kind of test proves, where a new test belongs,
and how a result is to be read. The performance harness has its own document,
[PERFORMANCE.md](PERFORMANCE.md); the served test network has
[TESTNET.md](TESTNET.md); phone journeys and goldens have [IOS.md](IOS.md).

## The rule

Test at the smallest boundary that contains the implementation whose promise
can fail. Most behaviour is proved below the process boundary with explicit
inputs and a driven policy clock. A smaller set of tests uses real sockets,
files and processes. A few complete stories drive each client through its
real UI. Every suite states what is real, what is replaced, and which
observation would reject the failure it exists to catch.

Three words carry the structure. A **suite** is a named group of tests. Its
**boundary** is what it proves. Its **lane** is when and where it runs.
"Spec" describes how a test is written, "golden" describes an oracle, "live"
describes an external dependency, and "performance" is a different question
about the same product. None of those is a layer by itself.

## The data chain

The client architecture describes one chain: a provider fact is interpreted
into items, items are stored and forwarded by the daemon, a client reduces
them into a model, a projection turns the model into a view, and each client
renders the view. The chain names the suites and it maps where fixtures come
from: a recording becomes interpreter emissions, emissions become a model, a
model becomes a view. Qualification runs real providers and real hardware and
feeds recordings and baselines back to the start.

The chain is not the whole product. Provider IO, SQLite, effect execution,
the Swift bridge, process ownership and native interaction sit beside it and
each can fail on its own. They get their own suites.

## Boundaries and suites

Unit tests live beside the code whose value or state transition they
exercise. Use values and explicit signals for parsing, reducer and
concurrency tests. When an assertion concerns command arguments, environment
or working directory, inspect the prepared command rather than launching a
provider.

```sh
just test-crate model
just test-crate claude -- sdk::query::tests
```

Above them sit the suites below. The middle rows are parallel seams, not
rungs every fixture must climb. A test belongs at the lowest boundary that
contains the implementation it must catch: a reducer test of a permission
command does not replace the Swift test that proves the button sends it, and
neither needs a relay and a simulator.

| Suite | Contract | Inputs and oracle |
| --- | --- | --- |
| Interpreter, per provider driver | `step(state, fact / input / tick)` yields keyed items, appends, snapshots, status and effects with no hidden IO, time or randomness. | Recordings and small authored event sequences; reviewed emission goldens plus explicit intermediate outcomes and invariants; a checkpoint at every relevant prefix. |
| Provider adapter | Real provider bytes and hooks reach the interpreter correctly; semantic commands produce the correct provider writes. | Strict read and write replay with complete consumption, expected effects, and acknowledged end of file, drain and join. |
| Single daemon | Ingest opaque framed records; commit items, status and cursor atomically; assign revisions; page and subscribe without losing committed data; reclaim only safely ingested history. | A synthetic journal writer, temporary real files and SQLite, controlled failure cuts; expected rows and stream log; store-reader conformance across memory, file and replica implementations. |
| Many daemons | Production runtimes enforce discovery and trust, routing, subscription, entitlement, revocation and artifact contracts. | A testnet topology, real loopback transports, scripted external boundaries, the policy clock, adversarial peers; typed observations and event traces. |
| Client model | `update(model, msg)` returns correct state and effects; keyed order and revisions never regress; an input reaches a settled, rejected or uncertain state. | Authored items, commands and effect results plus selected derived and graduated fixtures; explicit state and effects, invariants, replay and checkpoint equality. |
| Store, effects, bridge | Effects reach the intended service; identity and cancellation reach the right client; Swift decodes and applies the Rust view and command contract. | Service-side command records and real store implementations; shared schema and view fixtures; applied diffs compared with a fresh projection. |
| Projection | `project(model)` returns content with stable row and group identities; every item has exactly one membership; attachments keep their positions. | Small model states; reviewed view data goldens; independent invariants. |
| Native presentation | The supplied view is drawn correctly and interaction produces the correct command on that platform. | Terminal cells and semantic styles; phone image comparison; geometry, accessibility and command assertions. |
| System composition | Built binaries and platform facilities launch, connect, survive or terminate as promised and clean up what they own. | Real processes, PTYs, sockets and files; exact stdout, stderr, exit and signal; process identity; lock ownership. |
| Journeys | A person completes a declared task through the real terminal or phone UI and the production amux path. | A served topology with declared substitutes; a real client; behavioural checkpoints, independent host and provider records, and selected reached-screen goldens. |

The interpreter, single-daemon and projection suites are born with the
journal architecture; until then the fold semantics sweeps, the derived-row
goldens and the renderer goldens carry their weight.

Choose cases by distinct transitions, authorities and failure cuts. For each
promise cover the successful transition, the meaningful refusal, and any
ordering or recovery cut that changes the result. Add a regression when it
exercises a new cause, not merely a new screenshot name. Deduplicate
equivalent causes and oracles at the same boundary, not repeated product
words: "allow this ask" legitimately appears in interpreter semantics, in
Swift command dispatch and in one journey, because each catches a different
failure. Avoid the product of providers, routes, themes, screens and failure
modes.

Properties compress coverage where they mean something: every prefix can be
checkpointed and resumed; a newer revision never regresses state; catch-up
plus live equals uninterrupted delivery at every cut; every projected item
has one membership; applied bridge diffs equal a rebuild. A replay property
proves reproducibility, not correctness: a reducer can replay its own
mistake. Pair each property with explicit expected facts at meaningful
transitions.

## Executable specs

The daemon specs under `crates/testnet/tests/spec` and the reducer specs
under `crates/ui-state/tests/spec` state whole behaviours in domain language
and read top to bottom as documentation.

```sh
just spec
just spec -- a2a_cross_device
```

A spec declares its topology, then says what happens in prose verbs and
observations. Assertions live in the spec body. A harness method is a verb
that changes the world, an observation that waits for a consequence, or a
resource that owns a lifetime; it is never a whole scenario. When a harness
function is the entire body of a test, the test has moved into the harness
and the spec has stopped being a document.

Spec tests use scripted discovery and the loopback UDP fault proxy. Every
listening daemon is advertised at a stable proxy address while its real QUIC
endpoint stays private to the harness, so discovery is deterministic and
pairing, trusted links and session streams traverse the same controllable
datagram path. Use `TestNet::direct_latency` and `TestNet::loss` to shape
direct traffic, `TestNet::udp_blocked` to isolate one daemon, and
`TestNet::rebind_client` to move its endpoint. Do not rely on the machine's
multicast DNS state in a spec.

## Time

Three notions of time are kept apart.

1. **Product time** is what policy reads: expiry, retry, the remembered UDP
   failure, entitlement refresh, token and artifact TTLs, retention, agent
   grace. It is injected. A spec advances it and asserts before, at and after
   the real configured boundary. Monotonic intervals and wall timestamps are
   distinct inputs, and a token issuer and its verifier share one clock.
2. **Transport and platform time** belongs to the socket stack, processes and
   native UI. It runs normally. A QUIC retransmission timer is not an amux
   cooldown. A claim that needs a transport timer to elapse lives in the small
   real-time transport suite in the system lane and is labelled integration.
3. **Harness deadlines** are real, bounded waits on actions, observations,
   dumps and teardown. They diagnose a hang, still fire when product time is
   stopped, and are never the product oracle.

The two clocks coexist because they never share a timer. A spec advances the
policy clock by two seconds in microseconds of real time; a live QUIC
connection never notices, because its idle timeout is measured on the
transport's clock. Do not pause the runtime globally around real IO: timer
auto-advance can outrun socket readiness, and an advance to a policy expiry
can expire a connection at the same time.

Readiness waits on notifications. Subscribe before inspecting current state,
so a change between inspection and waiting cannot be lost, and take the
observed revision or state back so cause and outcome can be related. Keep a
bounded failure deadline and report the observed state when it expires. A
stuck predicate is a failure, never a pass.

Negative assertions need evidence. For deterministic components, drain
scheduled work to a declared logical boundary and inspect the complete event
trace. For real peers, subscribe before acting and observe the whole declared
absence window, or obtain an explicit refusal. One empty poll proves nothing.

Real sockets and child processes still need to make progress. Run their IO on
real time and advance policy time only for the behaviour under test. Recorded
provider scenarios own their streams and virtual clocks, so the harness may
run them concurrently; completing recorded output closes the stream before
the simulated process exits, and no live settling delay belongs in a replay.

Each independent scenario is one test, so the harness can schedule it. Share
immutable fixture construction once per harness when it is expensive; each
test owns its mutable session, output and observation state. A serial loop
over a corpus conceals the cases from the harness and hides every failure
after the first.

## Fixtures

Three kinds of asset, kept distinguishable and all first class.

- **Provider evidence**: sanitized recordings from a real provider, with
  version, model, invocation and hashes, under the `claude-specs` and
  `codex-specs` fixture roots. Produced by the probes; replayed strictly.
- **Reviewed derivations**: the committed row files produced through the
  production backends and compared byte for byte. They become interpreter
  emission fixtures with the journal architecture.
- **Authored scenarios**: small synthetic boundary cases, UI states and
  journeys, marked scripted. They test claims a live capture may never reach.

A capture verifies input reality, a derivation pins a transformation, an
authored case isolates a boundary. Keep one compact checked chain across the
layers; most edge cases should not load unrelated provider traffic.

Each derived asset names its source and hash, schema and generator, update
command and consumers. Ordinary runs compare and never regenerate; the
`UPDATE_*` flags are refused in asserting CI. A changed derivation fails
until its new outcome is reviewed. Graduated debug reports keep their
provenance and an expected outcome; they are evidence, not unquestionable
truth. Each corpus has one owning home and explicit consumer metadata rather
than relative paths from other crates.

## Journeys

A journey starts at a real user entry point, traverses the production amux
path, and observes the outcome. The terminal driver runs the built client in
tmux against its installation front door; the phone driver runs the built app
on a pinned simulator. Both attach to a served test network with declared
substitutes for the provider and the cloud account. Most testnet specs are
daemon and network integration, not journeys: they start below the UI.

One manifest schema under `journeys/` declares each story once: claim,
starting condition, topology and provider fixture, supported clients, acts,
expected observations, checkpoint baselines and required capabilities. The
terminal and the phone enact the same story idiomatically. Shared intent, not
shared keystrokes.

The shared stories are: reach a host and open its work; complete a
conversation that needs a decision, run separately for Claude PTY, Claude SDK
and Codex; leave and recover from cache produced by a real first run; manage
an agent through create, rename, stop, resume and delete; send and revisit an
attachment or review; keep authority boundaries visible across a profile
switch, a lost entitlement and a refused write. Native stories stay native:
account sign-in, purchase and restore, report retry, an accessible task at
large text and local-network entry on the phone; second attach, resize,
escape and detach on the terminal. Signals, raw bytes and backpressure are
system tests, not journeys.

A full pass requires all four:

- The real interaction path performs the action under test. A seeded cache
  cannot prove first-run caching; an act-only run is diagnostic.
- Exact semantic and independent host observations establish identity,
  delivery, refusal and negative controls. An app record saying "sent" is
  not evidence of delivery.
- Selected screens reached by those actions compare against reviewed
  platform goldens: terminal text plus semantic styles, phone images plus
  geometry. Projection goldens remain a lower-level check.
- Evidence records build and fixture identity, completed acts, observations,
  expected, actual and diff captures, exits and teardown. Partial work cannot
  report success.

Pin viewport, simulator, theme, script text and display identities; normalize
only declared volatile fields; mask only system chrome. An ordinary one-press
story cannot retry invisibly; a recovery story declares its retries.

## Lanes

- **Fast lane**: every push, all platforms. Offline, no credentials, never
  updates goldens, no wall-time settling interval in semantic cases. Unit
  tests, both spec suites, adapter replay, store and effect integration,
  projection and renderer goldens.
- **System lane**: real transports and processes with bounded deadlines,
  including the small real-time transport suite.
- **Journey lane**: both clients through the shared manifest. Desktop on
  every push; phone on the capture runner.
- **Tool lane**: the repository's own machinery rather than the product —
  script contracts, source and dependency policy, the CI command surface and
  the shipping scope audit. It runs with the fast lane and needs nothing the
  fast lane does not.
- **Qualification lanes**: live provider compatibility and measured
  performance, explicitly provisioned on enrolled machines, scheduled or
  change-triggered. Results say `pass`, `fail`, `unavailable` or `not_run`.
  Credentials, spending and hardware are explicit capabilities that an
  ordinary push test never inherits.

Live compatibility keeps the three provider entry points. A run with no
scenario reports `not_run`; it never implies compatibility passed.

```sh
just codex-live -- SCENARIO
just claude-pty-live -- SCENARIO
just claude-sdk-live -- SCENARIO
```

Production sign-in, StoreKit purchase and a real-cloud phone conversation
are the by-hand QA recipes in the phone justfile. Scripted journeys prove the
app handles the boundary; live runs prove the boundary still interoperates.

## Discoverability

`tests/catalog.toml` lists every suite and workload group once: its contract
in a sentence, owner and implementation paths, focused recipe and selection
syntax, boundary and oracle, real and substituted dependencies, time
requirements, required OS, toolchain, features, simulator, hardware,
credentials and cost, lane, evidence directory, and the fixture or baseline
update command. `just tests-list` prints it; `just tests-check` fails when a
listed recipe, Cargo target, feature, Swift target, journey or baseline is
missing, when a test target exists that the catalogue does not list, or when
a selection would match nothing. The catalogue is an index, not a runner:
Cargo, XCTest and the drivers still execute the tests.

Every lane emits one result envelope: suite and case, revision and build,
capabilities, selected versus completed work, status, duration and evidence
paths. A skipped compatibility main or an act-only journey cannot count as a
green lane.

## Failure evidence

Rust reports captured output for a failed test, but an outer timeout may kill
a harness before it can do so. Rerun the narrow target with `--nocapture` and
record whether its executable reached readiness. On macOS, a new executable
can pause under host security assessment before its first instruction; match
the exact process with system logs before calling that a protocol or shutdown
failure. A passing rerun alone does not explain the original failure.
