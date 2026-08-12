# The amux client layer

Status: normative design, pre-implementation, revised after external review
(see git history; review findings in the 2026-08 devlog entries). This
document owns the client side of amux: the `amux-ui` state library, its
renderers (the TUI first, desktop and mobile clients later), and the rules
that keep per-agent knowledge in the right place. Companions:
`docs/PROTOCOL.md` owns the wire, `docs/ARCHITECTURE.md` owns the system.
The executable half of this document will be the amux-ui spec suite,
mirroring `crates/amux/tests/spec/`; where prose and passing spec
disagree, the spec wins.

Crate shape: `amux-cli` → { `amux-tui`, `amux` }; `amux-tui` → `amux-ui` →
`amux`. One shipped binary — `amux-tui` is a library the CLI invokes (bare
`amux` opens it), never a second executable. The TUI consumes `amux-ui`
exclusively; it never reads `amux::Client` directly. There is exactly one
reducer implementation — this crate. One Runtime per client process, one
Model per daemon connection; renderers access the Model in-process by
borrow or (later) out-of-process via serialized Deltas — never by
reimplementation.

## The reducer core

`amux-ui` is a reducer over reified inputs. Three commitments, from which
everything else here follows:

1. **Inputs are reified.** Every stimulus — server event, user command,
   effect result, tick — is a serializable `Msg` value.
2. **Transitions are pure.** `update(Model, Msg) -> (Model, Vec<Effect>)`
   reads no clock, performs no IO, holds no hidden state. IDs,
   randomness, and observed time enter through Msgs; reducer-visible
   collections have canonical iteration order.
3. **Effects are data.** `update` returns them; the runtime shell executes
   them against `amux::Client` and feeds the results back as Msgs.

High-rate streams are coalesced into batched Msgs **before recording** —
the recorded Msg is the batch, so replay is independent of arrival
timing. The determinism guarantee is scoped and enforced: the same
reducer build, folding the same checkpoint and ordered Msgs, produces
identical Models, Deltas, and Effects; replay folds but never executes
Effects; and the spec suite proves it differentially — fold-from-recording
must equal live state after every Msg. Determinism is the goal; bug
reproduction (ring-buffer the Msgs, dump, replay, commit the redacted
recording as a regression fixture) is the dividend. Dumps can contain
prompts, code, and paths: local-only, shared only deliberately.

Pragmatics, not dogma: the shell's edges may be actor-shaped tokio tasks
so long as everything funnels into one ordered Msg stream. Shell-private
state may manage resources (sockets, reconnect timers, buffers) but may
not make semantic decisions — anything that affects which Msgs or Effects
exist must itself enter as a Msg.

Rendering is event-driven — no unconditional periodic redraw. Drain every
pending Msg, fold, draw once; idle input renders in the same loop
iteration; a flooding stream batches to a frame budget and never starves
input. Ticks exist only as data for time-dependent display, scheduled
only while something on screen needs them. A renderer is a pure function
`render(&Model, &ViewState, FrameContext { viewport, theme, now })` —
ViewState is renderer-local state (focus, scroll, drafts, navigation),
FrameContext the frame's environment, and purity means: no inputs besides
these three.

## Edge vocabulary

Only the edges are contracts; Model shape and reducer internals churn
freely under the compiler. The names follow CQRS deliberately: commands
in, state folds, deltas out.

- **`Command`** — client → reducer. The only write surface. Dispatch
  returns an `OpId`; outcomes return as state (`OpFinished` deltas),
  because a lost outcome must not leave a spinner lying. While
  disconnected, Commands fail fast with an error outcome — there is no
  offline queue. (A future mobile client may add one with idempotency
  receipts; that is additive shell work, not a reducer change.)
- **`Delta`** — reducer → clients. Entity-keyed, idempotent, upsert-shaped
  (`AgentUpserted`, `HostRemoved`, `Connection(..)`). A delta says "the
  state IS this", never "this happened"; applying one takes a keyed store
  and last-write-wins, zero domain logic. When serialized across a
  process boundary: sequence numbers plus snapshot-on-subscribe. This
  boundary is a *local projection surface* between amux-ui and its own
  renderers — it carries interpreted Model state by design and is
  distinct from the peer wire, which the facts-only rule below governs.
- **`Effect`** — reducer → shell. Internal, never a public contract.
- **`Ephemeral`** — reserved; an uninhabited enum until the first genuine
  one-shot exists. Membership rule: something is ephemeral only if a
  client that misses it entirely still renders a correct screen (a bell,
  a haptic, a row flash). Unsequenced, droppable, never read by `update`.
  Anything whose loss could make a screen wrong is state and goes through
  the Model. One-shots that depend on viewer context (a bell only when
  unfocused) are emitted unconditionally and suppressed by the renderer
  from its ViewState — tri-state focus, never suppressing on unknown —
  with typed skip reasons so "why no bell?" is answerable.

**Authority.** Subscriptions are the sole authority for entity state. An
RPC result mutates only pending-operation state, never an entity —
arrival order is not freshness, and a stale rename response must not
overwrite a newer subscription upsert. Async results carry the id of the
request that solicited them; results for superseded requests are
discarded. Reconnect replaces state by snapshot under a new epoch, with
an explicit synchronized marker separating catch-up from live, so
renderers can tell "loading" from "empty".

**Flow.** Every Msg kind declares its flow class: lossless (bounded
queue, producer waits), coalescable (batched into one Msg before
recording), or droppable (Ephemeral only). Overflow fails loudly — a torn
stream is a bug to fix; a silent drop is a lie. The Ephemeral rule
classifies for correctness; the flow class is for backpressure; every Msg
kind answers both.

**Retention.** Everything the Model retains is explicitly bounded. When
content is evicted, live obligations — a pending permission or question —
are never evicted with it: evict bytes, keep obligations, refetch content
through the Effect seam.

Renderer-local state — focus, scroll, drafts, navigation — stays in
renderers. Deltas eliminate client *domain* state, not view state.

## Kernel and per-agent layers

The kernel models what the protocol itself models, agent-agnostically:
fleet inventory (`AgentCard`), hosts and presence, session lifecycle,
connection and auth state (including "authentication required" — login
itself is CLI-owned via `amux init`; the UI layer only renders the
state), and pending operations.

Agent identity is typed, never normalized away. Each agent type gets its
own layer: a typed child model consuming that agent's *native* protocol,
with typed per-agent Command/Delta variants. Layers are allowed to be
structurally different — a transcript-shaped Claude layer, a
control-plane-shaped Codex layer — the asymmetry is expressed, not
papered over. There is no generic intermediate representation of agent
content and there are no capability flags. A client that does not know an
agent type degrades to the `AgentCard` and can still attach to its raw
terminal when the session advertises the agent-independent core protocol
`terminal_v1`; every PTY-backed session currently advertises it.

Where shared chrome needs cross-agent facts (badges, sort order),
per-agent **summarizers** derive kernel vocabulary — a handful of fields.
Summarize for chrome; never normalize content.

**Attention** ("this agent needs you") is the canonical summary: derived
at observation time by a per-agent fold — stream entries in, attention
out. Summarizer folds are pure and depend on nothing in the runtime, so
*where* one executes is a deployment decision, deliberately unspecified.
Interpreted state never rides the peer wire. Summaries are honest about
incompleteness: history is bounded at the source, so a fold over a
truncated or reset stream reports `Unknown` rather than guessing —
streams carry truncation/reset facts, backfill is bounded with snapshot
fallback, and degradation is always to `Unknown`, never to a wrong badge.

## Facts, translation, interpretation

The boundary between `amux` (core) and the UI layer, stated as three
verbs. This rule governs the peer wire; amux-ui's Delta boundary to its
own renderers is a different surface (above).

- Core **transports facts**: it parses, types, frames, injects, links —
  hook events, transcript rows, byte streams. Typing a fact is not
  interpreting it.
- Core may **translate**: a per-agent adapter may normalize the provider's
  *own* data into generic inventory vocabulary (e.g. a single
  `provider_label` for display, however the provider expresses it).
  Translation applies agent knowledge to that agent's own facts, field to
  field. (If one generic label ever proves too narrow, the reserve is a
  typed per-agent oneof on the inventory message, precedented by
  `CreateAgentRequest`.)
- Core never **interprets**: it preserves provenance but never applies
  precedence across sources (provider facts vs user facts), and never
  makes presentation policy or derives UX state. All interpretation
  happens in UI-layer folds at observation time.

And in the other direction: views **format, never decide**. Any
derivation a renderer wants — sorted fleet, display-name fallback,
staleness — is computed in the Model, once. The boundary is elastic
downward: the first time two renderers want the same derivation, it moves
into the Model or the agent layer. Presentation frameworks may differ;
derivations must not. Two clients computing the same derivation
independently is how projection drift starts.

## Chrome-first TUI

`amux attach` remains raw byte passthrough to the agent's own TUI; the
amux TUI is the *chrome* around it: fleet, attention, create/rename/
delete, host state. Attach from the chrome suspends the TUI (leave the
alternate screen, restore termios), runs the existing passthrough
in-process, and resumes the chrome on detach; late attach renders via
buffer replay (best-effort — the history is a bounded byte tail).

The chrome draws on the alternate screen and **never writes to terminal
scrollback**. This is load-bearing: emitted scrollback is immutable state
living outside the Model, and every system that writes it ends up
hand-rebuilding it on resize. The cost — no native scroll/search over
chrome history — is accepted; content history belongs to the agents, not
the chrome.

Every passthrough exit path restores the terminal (leave alt screen, show
cursor, reset modes, re-enable the keyboard): RAII-backed, with a panic
hook that restores before reporting — guaranteed on orderly exits,
best-effort on unwind, and nothing survives SIGKILL.

No split panes, ever. amux multiplexes *agents*; tmux and terminal
emulators multiplex *screen real estate*; running amux inside tmux
composes the two. This is also what keeps terminal emulation out of the
chrome permanently. The leader key is configurable (default `ctrl-a`) so
nesting works.

A first-party chat UI is a later milestone, built per-agent on typed
structured input/output protocols rather than terminal bytes: semantic
messages are compact over the network, replayable, permit local echo, and
stay resize-independent — a property that holds only if nothing on that
path renders into scrollback (a constraint, not an expectation). On
Windows the chrome must build and run (crossterm); byte passthrough is
untested pending e2e-driver support there, and the structured chat path
is the eventual guaranteed Windows client experience.

## Testing

- **Tier 1 — reducer prose specs.** Msg sequences in, Model assertions
  out; chapters read as documentation, same standing as the protocol spec
  suite. The load-bearing test is **differential**: after every Msg,
  fold-from-recording equals live state — a property, not a snapshot.
  Fixtures are both authored and recorded: real captures carry provenance
  (what produced them, with what versions), and a redacted field
  recording graduates into a committed regression fixture.
- **Tier 2 — integration.** The existing testnet harness drives a real
  daemon and test-agent through the shell; assertions on the Model.
- **Tier 3 — golden frames.** Renderers are pure functions of
  (Model, ViewState, FrameContext), so render one frame from a fixture
  and diff. Two backends: ratatui's `TestBackend` for widget frames, and
  a vt100-parser backend over real escape output for everything emitted
  as raw escapes (attach handoff, terminal hygiene) — the surface
  `TestBackend` is blind to. A scripted-shell fixture mode serves future
  GUI/mobile snapshot tests. No network, no clocks, no flake.
- **Tier 4 — smoke.** A thin true-e2e set for what the lower tiers cannot
  see (FFI, threading, real terminals — the attach/restore path
  especially). Kept deliberately small.

Assertions draw a three-way line. Input violations — a Msg the protocol
says cannot arrive — hit tripwires at the receiving reducer arm: refuse
the write, request a dump. State coherence — the Model's structural
index (ids, epochs, counts, phases), never content — is checked by
`Model::check_invariants` at the fold seam in every build: panic in
debug, dump once per violation kind in release, keep folding.
Renderer-vs-Model staleness is neither: a stale ViewState is tolerance
territory, clamped at render, never asserted against.

## Rejected alternatives

Recorded so they are not helpfully reintroduced. The first two are now
empirical: contemporary multi-agent clients shipped them, and the
predicted failure modes are visible in their trees (see the 2026-08
review notes in the devlog).

- **A generic normalized agent IR** (normalized envelopes + capability
  flags, as in the React Native app's projection layer). The IR grows
  toward the union of all agents' concepts, filters content lossily, and
  every new agent feature is a schema change through every layer.
- **Notifications-out for domain state** ("this happened" events with
  per-client folds). The fold — ordering, catch-up, reconnect,
  idempotency — is the hard 20%, and per-client copies of it drift.
  Deltas keep the fold in one spec-tested place.
- **Reactive stores / signals as the core.** Maintained derived state
  reintroduces projection drift, and replay becomes a retrofit instead of
  a property.
- **Actor-fragmented state.** Cross-actor ordering is nondeterministic;
  reproducibility dies. Actors are fine at the shell's edges only.
- **Host-side derived attention on the wire.** Interpreted state
  advertised authoritatively fleet-wide, with clearing-rule iteration at
  daemon-deploy speed and no client-side escape hatch. Facts on the wire,
  folds at observation time.
- **Writing chrome output to terminal scrollback.** Buys native
  scroll/search at the price of resize reflow, cell consolidation, and
  render caches — past frames become immutable state outside the Model.
- **Split panes** (above). **Unconditional periodic redraw** (above).

## Deferred decisions

Doors left open on purpose; any future answer must respect the stated
constraint.

- **Subscription policy.** V1 subscribes to local agents (and any agent
  the user interacts with); broader eager subscription is a kernel Effect
  policy change and must move no interpretation anywhere.
- **Push notifications.** When wanted, the sender runs the same summarizer
  fold over facts it can already see (the folds are pure, so extracting
  them into a shared crate is mechanical); interpreted state still never
  rides the peer wire, and multi-writer "attention" pushes are
  specifically to be avoided.
- **Lightweight per-agent stream views** (e.g. a hooks-only protocol
  alongside `terminal_v1`) as a bandwidth lever: a new `io_protocols`
  string on the existing subscription surface, not a new RPC, and still
  fact emission.
- **Session-content access** goes through the Effect seam; no layer may
  assume content is remote-fetched or locally-stored.
- **Content windowing.** Transcript-scale entities are windowed when the
  chat milestone arrives — deltas apply within a window, an epoch guards
  snapshot/live reconciliation. Nothing in V1 touches content.
- **Offline command queueing** (mobile): additive shell work with
  idempotency receipts; the reducer's fail-fast contract is unchanged.
- **Typed agent identity on the wire.** `agent_type` string +
  `io_protocols` strings function as an open capability set; when the
  inventory message is next reshaped, prefer a typed known/unknown agent
  descriptor. Raw terminal attachment is already agent-independent
  through `terminal_v1`; only the typed descriptor remains deferred.
- **Naming fields.** Current leaning: `name` (user-assigned, written only
  by rename) plus adapter-translated `provider_label`, display fallback in
  the Model; the provenance machinery in `agents/naming.rs` then deletes.
  Lightly held — settle it when the chunk is picked up.
