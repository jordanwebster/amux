2026-09-18 — **Scroll-back memory return is guarded by its absolute ceiling.**
The row's 1.10x budget remains enforced, while its relative baseline is
explicitly null. Identical-code runs split between ratios near 0.6 and 1.0
depending on whether allocator and kernel memory returned before or after the
initial physical-footprint snapshot. The three growth rows keep their drift
gates.

2026-09-18 — **Idle summarizer CPU is guarded by its absolute ceiling.**
The row's 1.0% budget remains enforced, while its baseline is explicitly null:
five identical-code runs varied by 42% because the measurement is dominated by
macOS health-tick park and unpark cost. The stable CPU-per-row metric and every
other percentage drift gate remain unchanged.

2026-09-18 — **Mac14,6 baselines use the active-cluster reference state.**
On macOS 26.5.2, the release `bundled,perf` desktop qualification recorded
its baseline with one busy core maintained by the cluster warmer; the phone
baseline was refreshed by the same quiet `just perf --baseline` invocation.
Every desktop and phone row remained inside its unchanged absolute budget.

2026-09-18 — **Desktop performance drift uses a defined CPU-cluster state.**
The release performance harness now keeps one core busy in a re-exec child for
the complete desktop qualification, ties that child to a parent-owned pipe and
reaps it after the report. Reports and desktop baselines name the reference
state and reject mismatches, while the memory-only soak remains unchanged.
Cold-start teardown also stops and waits for the installation daemon launched
by the measured client, so repeated qualification leaves no detached server.

2026-09-18 — **Store rebuild proof uses two compiled family definitions.**
The store suite now keeps a process built with the supported chat-family v2
fixture live while the current v3 test binary opens and rebuilds the same
database. The stale owner proves its commit and page are fenced by generation,
reloads the empty rebuilt family, retains the durable view, and refuses to
reopen the newer format without changing its shape or generation. Desktop and
iOS recipes build the separate fixture and print bundled-SQLite linkage proof.

2026-09-18 — **Suspend crash cuts preserve the sealed sequence contract.**
The daemon protocol specification now abandons a live emitting test agent at
each durable cut, builds a fresh host over only the surviving suspend files,
and proves the subscriber saw every numbered pre-seal row. A saved but
unconsumed seal resumes the same identity at its watermark with the documented
continuous, reset, and truncated cursor outcomes; a consumed seal, including
one followed by a resumed publication, recreates the agent at sequence one.
The focused protocol recipe now forwards test-harness flags such as
`--nocapture` without mistaking them for a narrower test-name suffix.

2026-09-18 — **The desktop paints its selected profile before discovery.**
Bare `amux`, `amux ui`, and store inspection now resolve UUID, label,
remembered, and single-profile selection from the durable installation
registry and open that profile's SQLite store before the installation front
door or profile socket is awaited. The release cold-start workload uses the
same split installation layout and supported invocation, and `perf --only
cold-start` can requalify it without conflating unrelated performance rows.
The warm-start
journey seeds from a captured real Claude Code session, holds the profile
socket open without answering, proves the remembered card and closed composer
gate, then restores the socket and observes confirmation in the same TUI.

2026-09-18 — **In-process PTY protocol checks honor the explicit-start
boundary.** The provider-plane helper now starts its inert scripted Claude PTY
with a throwaway event channel before opening either exposed plane, then aborts
the ingestion task. The downstream `test-tui`, `test-ui`, doctest,
`release-check`, `embedded-check`, and `mobile-check` CI legs pass; the four
Codex streaming and scrolled-back expected frames now match the existing
store-backed fold projection, which presents the reasoning text without the
legacy synthetic `summary:` prefix. None of these legs needs loopback access;
the encompassing CI run still leaves its listener-bearing testnet checks to an
authorized driver.

2026-09-18 — **Claude SDK derivation replays follow prompt identity.** The
immutable real-session captures still describe the provider versions that
produced them, while the replay harness overlays the deterministic prompt UUIDs
the current daemon sends on the wire. Strict replay therefore continues to
verify every captured exchange and every byte of derived output after prompt
identity became explicit.

2026-09-18 — **The listener-release regression waits on the OS clock.** The
test still proves a profile restart yields while its previous listener owns
the address, but its retry deadline now advances in real time. A transient
reuse of the just-freed ephemeral port can therefore clear before the test
exhausts the production-sized retry window; the separate permanently occupied
case keeps fast simulated-time coverage of the deadline.

2026-09-18 — **The authenticated agent journey follows authoritative fleet
removal.** Its exit-code assertion now observes the session-close stream
directly while the runtime independently confirms the agent leaves the fleet,
so the test no longer imposes an ordering on those two protocol streams.

2026-09-18 — **The shared-store runtime passes strict lint again.** Store
worker launch policy now travels in one named configuration value, keeping the
profile, window, maintenance and recovery choices explicit without an
overloaded constructor. The already-drawn first-frame signal also uses the
direct conditional form required by the workspace lint policy.

2026-09-18 — **Injected PTY fixtures follow the explicit-start boundary.**
The delivery, protocol-plane and installed-keymap unit fixtures now start
their inert scripted backends before asserting behavior that belongs to an
active session, matching the host's summarizer-before-ingestion order.

2026-09-18 — **Store recovery has an operator guide.** The UI boundary now
names the daemon's advisory fleet summary beside the rule it excepts. The
debugging guide explains what quarantine moves aside, what remains visible
while durable views are unresolved, how to inspect the current transcript and
restore durable service with the desktop commands, and why the phone recovers
that state on relaunch instead.

2026-09-18 — **The phone's schema tests follow the pinned projection again.**
Removing the provider layers' retained presentation feed added a fleet update
to the pinned phone schema (the Stop hook now marks the agent as needing you)
and changed the pty session and rewritten message it pins, but the Swift
schema tests still read the old positions and values. They now read the
current event positions, the unknown pty gate and phase, and the merged
message text.

2026-09-18 — **The phone's remembered fleet goldens show each agent's stored
standing.** Store seeding now accepts the standing each machine last published
and stores it as that machine's summary, so the cached-fleet captures draw the
permission request and question in the header, working and finished rows with
their real ages, and only the offline machine's agent without a standing. The
offline machine in those captures is now air rather than Studio, so the agent
asking for permission is on a machine the phone last knew was answering; the
cached conversation captures keep Studio away and are unchanged.

2026-09-18 — **Mac14,6 has a qualified ten-minute soak baseline.** On macOS
26.5.2 with the release `bundled,perf` build, `just perf soak --baseline`
recorded 13.313 MiB client peak, 1.049 MiB per idle daemon agent and 16.572 MiB
per active daemon agent. An immediate independent run passed the 10% drift
gate at -1.8%, -0.1% and +2.5%; both fitted slopes remained under their
absolute ceilings.

2026-09-18 — **Memory soaks enforce their own stable baseline.** Ten-minute
soaks now record and read a workload-specific baseline without touching the
fast report. Peak and per-agent memory enforce the 10% drift gate, while fitted
MiB/minute slopes are explicitly ceiling-only because percentage drift around
zero is not meaningful. Shortened diagnostic runs cannot record or apply a
baseline.

2026-09-18 — **Phone stores maintain and recover without a desktop operator.**
The cold-start store read and the first callback frame now enable the same
hourly, 100 ms maintenance worker used by the terminal, with the phone's
200 MiB reclamation target. A completed corruption quarantine remains beside
the replacement store and is resolved automatically on relaunch, restoring
writable durable views; phone diagnostics describe that recovery instead of
directing people to desktop store commands.

2026-09-18 — **Restored SDK rows retain their renderer position.** The SDK feed
cleanup keeps no discarded presentation model, but its durable row DTO still
carries the stream sequence that app projections use to order and diff stored
history.

2026-09-18 — **A Claude Stop hook publishes one parent completion.** Structured
PTY ingestion is now the sole publisher of the completion lifecycle envelope;
the host no longer emits a second copy while enqueueing the same hook. Hook
deduplication also precedes lifecycle publication, so a provider retry inside
the suppression window cannot notify the parent twice.

2026-09-18 — **Continuation tests follow the complete recorded corpus.** The
Claude PTY, Claude SDK and Codex gates now discover every recorded row fixture
instead of maintaining partial lists, and the shared postcard vocabulary
round-trips every merge defect. Codex streaming deltas retain their stable item
type until completion rather than exposing the transport method name; this is
a value correction, so the encoded entry layout and version remain unchanged.

2026-09-18 — **Synchronized fleet loads cannot revive removed agents.** A
store refresh may advance the standing and progress of a card already held by
the authoritative live fleet, but it cannot change membership or replace live
inventory facts after snapshot synchronization. Cold-start loads retain their
remembered-card behavior before the connection reaches that boundary.

2026-09-18 — **Phone store fixtures keep the app dependency boundary.** The
debug-only remembered-store seeder now obtains the provider summary version
through `ui-state`'s existing public helper. This removes an unnecessary direct
production dependency from `app-runtime` to `fold` while preserving the exact
version used by the reducer and daemon summaries.

2026-09-18 — **Provider folds enforce their whole-entry ceiling.** After the
per-field caps, every provider now trims combined oversized entries in a fixed
order until their encoded bodies fit the store budget, retaining identity,
finality and obligation state. PTY result-only tools are final even when the
observer joined after their invocation, and summary selection keeps outstanding
work unknown unless it also adopts the observation's attention.

2026-09-18 — **Provider observation no longer rebuilds discarded feeds.**
The Claude PTY reducer facade now derives only its running facts and
obligations, while the Claude SDK observer retains only session, turn, task and
todo state. Drawable entries remain owned by the durable folds, eliminating
per-row construction of a throwaway SDK feed and no-op PTY entry sinks.

2026-09-18 — **Development iOS builds refresh their shipping-path stand-in.**
`just ios rust` marks the shipping framework only when it supplies the
development slice as a placeholder, and restages that placeholder whenever
the Rust fingerprint changes. This keeps the generated C header current while
leaving a real `just ios package` framework untouched; the shipping recipe
removes the marker after it owns the path.

2026-09-18 — **Inventory revision I/O failures no longer stop lifecycle
processing.** Clearing an agent's task, withdrawing it, and committing server
suspend now apply the local lifecycle change even when the durable revision
block cannot be extended. The daemon logs the revision file and error, withholds
the corresponding authoritative fleet event, and retries the same reservation
on the next lifecycle event; it never publishes an unreserved revision.

2026-09-18 — **Claude SDK prompts retain one identity through resume.** The SDK
stream now sends the same UUID that amux publishes for an accepted prompt, and
resume bootstrap excludes prompt publication until the gap, transcript
history, ready row, and live session facts are complete. Claude Code 2.1.274
preserved the supplied UUID in an MCP-free real-binary capture: the streamed
UUID `f033f0fe-9d1d-42ed-8edc-35912343b923` appeared unchanged on the transcript
user row and as the single `user:` store key before and after resume. The
maintained live SDK capture separately confirmed the non-UUID input identity
`70726f6d7074` across the live row, provider transcript, and resumed history.

2026-09-18 — **Structured PTY ingestion starts after its summarizer attaches.**
External read-only hook sessions, supplied provider sessions, and scripted PTY
sessions are now constructed inert. The host subscribes the daemon summarizer
at the empty tail before it starts ingestion, so an immediately queued first
permission row contributes to the published standing instead of forcing an
unknown truncated baseline. The host also owns and monitors the ingestion task
for these injected sessions in the same way it does for launched sessions.

2026-09-18 — **The combined performance command records both baselines.**
`just perf --baseline` now forwards baseline mode to its iOS qualification leg,
so one quiet reference-machine run records both the desktop and phone medians
it just measured. Previously the phone suite ran and passed but silently kept
its older baseline, leaving later phone runs to compare the current app with a
pre-flight runtime.

2026-09-18 — **Commit drift uses the established reference value.** The final
baseline-completion run replaced commit latency's existing 5.068 ms reference
with a 4.007 ms fast-mode sample even though the measured workload was
unchanged. Repeated qualification receipts put its other filesystem timing mode
at 5.0–5.3 ms, so the Mac14,6 baseline again uses the earlier reviewed value.
The 20 ms p99 budget and the 15% time-drift limit remain unchanged.

2026-09-18 — **Served journeys exercise the store-backed client path.** The
agent, SDK and Codex relay journeys now open their conversations through a
temporary SQLite store, so their transcript assertions observe the same
canonical windows as the shipping clients after the provider presentation
feeds were removed. Report conversion's eviction refusal is likewise pinned
to a current stored boundary; a transition-era truncated feed without stored
history is correctly classified as a partial session.

2026-09-17 — **A busy store open retries the complete SQLite operation.** Two
clients creating the same store can contend while setting the new-file pragmas,
before the schema transaction begins. The one permitted `Busy` retry now
reopens and reconfigures SQLite from the start instead of retrying only the
later initialization transaction, so concurrent first openers converge while
an exclusive sidecar lease still returns `Busy` after its single five-second
wait.

2026-09-17 — **The phone C-boundary test follows automatic quarantine.** A
corrupt remembered-fleet store is now asserted to report that it was
quarantined and that daemon-retained rows can be recovered, then a second
cached-fleet read must return the replacement empty cache. The stale assertion
still expected the pre-recovery instruction to close another process and
relaunch, even though the app-runtime boundary had already completed recovery.

2026-09-17 — **The dependency policy covers store-backed report replay.** The
TUI report player reconstructs canonical stored windows with provider fold
types, so the production dependency policy now permits the direct `tui` to
`fold` edge that the replay implementation already requires. The policy had
still described the pre-store renderer boundary and stopped `just ci` before
tests despite the workspace building successfully.

2026-09-17 — **The repaired harness has a complete reference baseline.** On
the quiet Mac14,6 reference machine running macOS 26.5.2, the release
`bundled,perf` qualification records every desktop metric after the store-window
flood and live-summarizer repairs. The four real-exec cold-start rows are numeric
again: 22.266 ms for 40 agents and 23.369 ms for 200 agents, with their worst
samples also inside the unchanged budgets. The same run's measured iOS suite
passed with a 2.6 ms median store read and 449.5 ms median first frame.

2026-09-17 — **The phone suite waits for the machine to go quiet.** `just perf`
runs the desktop workloads and then the phone suite in one command, so every
phone measurement was taken while the machine was still working through what
the desktop run had just finished. Measured on the Mac14,6 reference machine:
run straight after the desktop suite, the cold store read took 30.6 ms against
its 10 ms budget and the worst cold first frame 974 ms against 600 ms; the same
suite, same tree, on an idle machine took 2.5 ms and 455 ms. Pick-up recovery
moved the same way, 331 ms against a recorded 242 ms under load and 208 ms
idle. Under heavier pressure the simulator kills the app outright, which
reaches the run as a refused connection to the app's test door rather than as a
slow number, and that is what five `just perf` checks failed on today.

Nothing measured here had regressed: desktop and phone each pass on their own,
and the full command passes once the phone suite waits. `scripts/quiet` blocks
until the one-minute load average falls below a threshold, and the phone recipe
calls it before taking its simulator lease. A machine that never settles is
measured anyway with a line saying so, because the budgets already catch a
loaded machine — that is how this was found — so waiting only avoids a wasted
run, while refusing would turn a busy machine into a hard failure.

2026-09-17 — **Quiet-machine summarizer baselines are recorded.** On the
Mac14,6 reference machine running macOS 26.5.2, the release `bundled,perf`
`--only summarizer` workload records 5.350 microseconds of process CPU per row
across five active repetitions and 0.282% of one core across three consecutive
ten-second idle windows. No perf, soak, Cargo, or Simulator workload was active
during the run.

2026-09-17 — **Summarizer performance samples are repeatable.** The daemon
memory harness now waits on each live summarizer's folded-through signal instead
of charging a busy-yield loop to process CPU. Qualification takes the median of
five independent 20,000-row active repetitions and three consecutive ten-second
idle windows with 200 live tasks, and reports those sample counts. A release
`perf --only summarizer` mode runs the listener-free workload in isolation
without permitting a partial baseline rewrite.

2026-09-17 — **Live summarizer costs have a reference-machine baseline.**
The Mac14,6 baseline now records the shipping ring-reader and summarizer-task
measurements: 5.104 microseconds of process CPU per consumed row and 0.128% of
one core while 200 summarizers idle for ten seconds. Both remain well inside
their unchanged absolute budgets; the replaced values measured only direct
fold calls and assumed ticks, so they were not comparable to the live workload.

2026-09-17 — **The summarizer benchmark enters its Tokio timer context.**
The ten-second idle interval is now constructed inside the benchmark runtime,
so the release performance binary can measure the shipping summarizer tasks
without panicking before the timer starts.

2026-09-17 — **Daemon performance qualification exercises live rings and
summarizers.** The idle-agent memory sample now fills every one of its 200
structured replay rings to the 1 MiB byte ceiling with valid provider rows,
waits for each daemon summarizer to consume its cut, and lets the agents become
idle before sampling. Summarizer idle cost now records process CPU across an
actual ten-second wall-clock interval with 200 running tasks, while active cost
writes rows through 20 shipping rings and waits for the live folds to consume
them, accounting for ring delivery, payload serialization, and summary-cut
replacement instead of timing pure folds or assuming a tick rate.

2026-09-17 — **Provider folds retain their edge-case contracts.** Coverage
formerly held only by the deleted `a2a_claude_inbound`, `claude_sdk_feed`,
`a2a_codex_inbound`, `codex_feed`, and `feed_replay` client specifications now
lives beside the shared folds. It pins both Claude message carriers and their
turn distinction, SDK interruption and subagent-prompt behavior, Codex tool
ownership and turn targeting, last-turn context usage, and inert Claude PTY
control rows. A TUI golden proves failed SDK tools remain named, failed, and
readable. The retained live-conversation fixture now has an active provenance
and privacy guard, and documentation points to its actual location.

2026-09-17 — **Stored chats preserve provider edge semantics.** Finished
Claude SDK stream blocks stay complete when their trailing stop events arrive,
subagent reads stay outside the session's exploration runs, and failed
TodoWrite calls remain visible and named in both Claude transports. Claude PTY
bookkeeping arrays no longer paint as unknown rows, and interrupts without a
known prompt time no longer invent zero-length turns. Malformed Codex MCP
startup updates remain visible as protocol drift instead of being counted as
ready. The affected provider entry versions, and the SDK checkpoint version,
advance so previously stored incorrect rows are rebuilt. Store-backed goldens
cover the finished SDK reply, subagent exploration boundary, and MCP drift.

2026-09-17 — **Flood performance rows repaint the stored transcript.** The
frame-under-flood workload now sends every generated Codex batch through the
open chat's store stream and applies the canonical commit acknowledgement
before drawing. Every measured pulse must advance the stored tip and paint its
last provider row, so the frame and keypress budgets cannot pass while timing
an unchanged transcript. Reconnect and live-attach qualification now seed the
client's 5,000-row cursor in SQLite instead of requiring the daemon's 1 MiB
replay ring to retain that entire history; measured reconnects still request
and account for only the rows published after the stored cursor. The setup
allows provider lifecycle rows to put the daemon watermark ahead of that
cursor; only the measured after-cursor batches require exact row counts.

2026-09-17 — **Report replay preserves modern raw stream semantics.** New
report headers identify store-backed chat recordings, and reports from the
transition infer the same format from retained store traffic. Their raw attach
messages now pass through the ordinary reducer exactly as they did live,
instead of being converted into chat-store commits. Reports recorded before
store-backed chats still materialise their provider rows through the isolated
legacy oracle and continue to reproduce.

2026-09-17 — **Phone projection tests exercise the canonical store window.**
Host-loss and reconnect coverage now starts with a stored conversation and
proves replay does not duplicate it; authoritative agent removal drops the
fleet member but leaves an already-open stored conversation readable until it
is closed. The streaming workload commits all 1,000 rows through the chat store
path and bounds each frame by the rows in that delta, while Codex delta coverage
proves an amended stored message replaces the phone row in place. Vacuous
provider-window assertions were removed.

2026-09-17 — **Golden fixtures now exercise the same stored rows as
production.** Approval resolution preserves a Codex work item's command and
moves an accepted item back to running, while Claude SDK exit envelopes accept
the empty body emitted by the daemon. The chat harnesses no longer inject a
replacement Codex item, a space into an exit notice, or a truncated store
boundary for a provider gap. Consequently, the Codex error-gap golden loses
only its former top “earlier history unavailable” line: that history marker now
comes exclusively from a real store boundary. The recorded Codex journey also
waits for its stored turn completion before declaring the replay finished.

2026-09-17 — **A stopped store worker always ends its client session.** Every
executed store operation that discovers corruption now reports the worker
failure independently of its reducer result. A stale operation or superseded
chat attempt can still discard its ordinary result, but it can no longer leave
the runtime alive behind a closed worker channel; shutdown completes the
quarantine and emits the same terminal diagnosis as an active operation.

2026-09-17 — **Phone cache launch completes corruption quarantine.** The
launch-time cached-fleet read now retries a corrupt open once so an uncontended
store is quarantined immediately and the next launch starts from an empty
derived cache. An unresolved quarantine no longer hides otherwise readable
fleet rows merely because the durable local-host marker is unavailable; the
phone projects those rows with its local host unknown.

2026-09-17 — **Persisted provider entry encodings are version-pinned.** Claude
PTY entries now use entry version 2 after their body variants and stored tool
and interruption content changed, so an older cache is selectively discarded
instead of being decoded under the new shape. Each provider fold now snapshots
one postcard entry for every body variant beside the entry version, making a
future shape change require an explicit version decision.

2026-09-17 — **The store window is the only retained conversation.** Claude
PTY, Claude SDK, and Codex provider layers no longer retain presentation rows;
they keep only running state such as attention, session facts, obligations,
open work, plans, cursors, and attachment metadata. Desktop and phone render
canonical SQLite entries, and only store boundary markers can report truncated
history. The terminal's store-entry adapter uses the existing block cache, so
steady redraws still repaint no transcript blocks.

Stored SDK entries now carry the finality, error detail and parent-task
attribution needed to reproduce the existing chat frames directly from the
canonical window. Historical terminal reports recorded before store-backed
chat are migrated during replay into an isolated in-memory mutation oracle;
production rendering still has no provider-window fallback.

The daemon summarizer was audited independently: production code owns the
closed `AgentFold` and calls only its baseline, summary, version, and row-fold
operations; it never constructed or read the deleted client observation
windows. Its measured inline per-agent fold state is therefore 360 bytes before
and 360 bytes after this change. Codex's tip now keeps the provider's fixed-size
112-byte token-usage fact instead of only its context-meter projection so a
stored turn can reproduce the existing usage line; the closed `AgentFold` size
is still dominated by another provider and no feed-sized allocation moved into
the daemon. The removed allocation was client-only: as many as 1,000 provider
presentation entries per chat in addition to the store window.

2026-09-17 — **The phone stops visibly when its store is unusable.**
The cached-fleet bridge now distinguishes an absent disposable store from a
store that exists but cannot be opened or read: the former is an empty cold
cache, while the latter returns the desktop runtime's cause-and-remedy
diagnosis. A store failure during a phone session closes the runtime and emits
one terminal projection event. Swift replaces the whole shell with that
diagnosis and one Relaunch action, so no fleet or conversation survives as a
degraded live-only client. The failure page is pinned in light and dark, and
the bridge and iOS documentation describe the shared failure contract.

2026-09-17 — **A desktop session never falls back to live-only operation.**
The reducer now has one drawable conversation source: canonical store-backed
entries. Store-open failures and unrecoverable operation failures stop the
runtime, close its network and chat streams, and synchronously retire the store
worker; corruption still requests quarantine, and shutdown waits long enough
to distinguish a completed quarantine from one held up by another client.
After the terminal leaves its alternate screen, the process reports one line
that names the store, the cause and a concrete remedy. Permission, disk-full,
I/O, newer-format, unqualified-SQLite and corruption cases each retain their
own diagnosis, including the fact that daemon-retained rows survive a corrupt
cache. Holding a dead session behind an error banner was rejected because it
would preserve the same degraded mode under a different presentation.

Generation moves and conflicts still reload, one failed commit still retries
after one second, a busy history page is dropped, and unresolved quarantined
durable views remain unrestored until the operator runs the explicit resolution
command. Startup cannot dial the daemon until both store reads have settled, so
an unusable store exits without making a network connection.

The remembered-chat runtime coverage now waits for the fleet and both durable
effects it actually consumes, then retires its first worker before reopening
the same store. It matches a process relaunch without depending on queued
write or detached-thread timing.

2026-09-17 — **Quarantine recovery is an explicit, atomic operator action.**
`amux store resolve` first reports each unresolved manifest, the durable
families known to have existed (or that they could not be determined), and the
paths holding the moved database files. It changes nothing until an interactive
confirmation or `--yes`, then takes the store lease exclusively and clears
exactly the reported unresolved rows in one FULL-durability transaction. A
concurrent client, a newly pending quarantine or a changed report refuses the
resolution; an interruption leaves all confirmed rows unresolved or all
resolved. Resolution enables the fresh store's durable reads and writes but
does not claim its empty durable tables recovered quarantined data.

2026-09-17 — **Every bounded recorder window starts from an exact on-disk
checkpoint.** The recorder now owns only its 2 MiB/count-bounded ring of
serialized reducer messages. Each runtime with report storage writes a private
rolling Model checkpoint at startup and atomically replaces it before a message
would cross either ring bound; an individually oversized message advances the
checkpoint again after it folds. Report bundles embed that checkpoint and the
following ring, so ordinary, invariant and panic reports all replay through the
same unconditional fold without a captured-model or recent-context mode. The
sticky invariant warning is report framing applied after replay, and recorder
retention attributes zero bytes to a Model checkpoint.

On the Mac14,6 reference machine, eleven release-profile serialize, private
temp-file write, `sync_all` and rename samples measured a 7.009 ms median for
the 1,570,638-byte desktop Model with ten chats at 96 entries each, and a
6.646 ms median for the 1,181,749-byte phone shape with one 800-entry chat.
That bounded reset cost stays synchronous: even added to the measured 8.978 ms
flood-frame render it remains inside one 16.667 ms frame and well below the
50 ms key-to-flush budget, while preserving an atomic checkpoint/ring cut for
panic capture. At 2,000 rows/s it is paid only when the unchanged 2 MiB ring
fills, not per row.

2026-09-17 — **Daemon replay retention is one byte budget per buffer.** Claude
PTY, Claude SDK and Codex structured rings now retain at most 1 MiB of encoded
rows, with no row ceiling, idle timer or second idle-trim budget. Claude PTY raw
replay likewise retains 1 MiB. Raw eviction advances to a complete ANSI/UTF-8
boundary, so a late raw attach never begins inside a terminal control sequence;
a vt100 repaint test forces the nominal cut through a colour sequence after
more than 1 MiB of cursor movement, colour and clear output and reproduces the
uncut screen.

The daemon soak now measures the Claude PTY owner set it reports: one structured
ring, one raw replay buffer and one live summarizer per agent. Agent counts,
corpus, pulse cadence, warm-up, reset and sampling remain unchanged. The next
driver-owned four-minute diagnostic supplies the measured per-active-agent row
and the residual summarizer fold-state accounting; its expected composition is
about 1 MiB structured plus 1 MiB raw plus the small fold state.

2026-09-17 — **A store-backed session retains one small chat cache.** The
client's rising footprint was bounded fill, not an unbounded leak: before the
two-minute diagnostic regression ended, each chat was still filling an
800-entry store window, a duplicate 1,000-entry provider feed and 4,096 row
identities. The ten-minute gate likewise remained a ramp until the window filled
at about 400 seconds. Under the real allocator, ten chats and the soak's exact
4,800-row corpus retained 11,614,080 Rust bytes plus 2,152,560 SQLite bytes,
or 2,868.1 B per delivered row. The 1.000 MiB/min budget at 1,200 rows/min is
873.8 B per row.

Store-backed provider layers now discard their presentation feed after updating
running attention, summary, facts and obligations; SQLite's canonical window is
the only drawable owner. The obsolete UUID/content dedupe sets are gone from
both the provider layer and durable Claude fold, whose version advances so an
old checkpoint reloads rather than being misread. The desktop window is now a
96-entry scroll cache—about two to five dense 20–40-row terminal viewports—and
pages 96 older entries at a time. Four release-mode page-ins through the
shipping store worker took 0.511, 0.521, 0.523 and 0.608 ms, a 0.523 ms median.
The phone still explicitly retains 800 entries because it does not page.
SQLite's per-connection cache is 128 KiB, enough for these sub-millisecond page
reads without growing once per delivered row.

The same allocator test now retains 3,692,912 Rust bytes plus 112 SQLite bytes,
or 769.4 B per row. Store windows including their fold heads fell from 980.9 to
247.5 B/row, provider state from 492.5 to 44.3, other model state from 125.5 to
76.8, and SQLite from 448.4 to 0.0; the bounded recorder is 185.8 and remaining
runtime/store-worker ownership is 291.7 B/row. The recorder still keeps only its
2 MiB recent-message ring during steady state; the authoritative Model now
lives only in its rolling on-disk checkpoint. Earlier real-client diagnostics
measured
5.162 MiB/min before these repairs and, at commit 6c525e1f, 2.271 MiB/min with a
22.204 MiB peak and a zero slope after reset. The next driver-owned four-minute
diagnostic supplies the final after slope and peak; the unchanged ten-minute
soak remains the qualification gate.

2026-09-17 — **Memory repair runs keep the real soak workload.** The original
ten-minute client run grew at 4.010 MiB/min with a 56.688 MiB peak because the
diagnostic recorder retained serialized stream and store messages up to its
10,000-message count ceiling—about 3.5 KiB per delivered row. The recorder's
shipping 2 MiB byte ceiling now advances those messages into its exact replay
checkpoint instead. The first shortened run then reported 5.309 MiB/min with
a 35.016 MiB peak: the planned minute-three reset and reopen moved the
allocator to a new plateau in the middle of the two-minute regression, so a
one-time roughly 7 MiB step looked like continuing growth. Diagnostic runs now
report the worse steady-state slope on either side of that known discontinuity
while still counting the step in the peak; the ten-minute qualification keeps
its original after-warm-up regression. `AMUX_PERF_SOAK_SECONDS=240` runs the same on-disk client,
ten chats, row rate and two-minute warm-up for repair diagnosis, moves reset
and reopen to minute three, and labels itself as shortened diagnostic evidence.
The reset trigger follows elapsed wall time, so real provider I/O cannot defer
it past the diagnostic window; an unset duration remains the ten-minute
qualification.

2026-09-17 — **Cold-start qualification now launches the shipped terminal client.**
The 40- and 200-agent rows exec the release `amux` binary under a pseudo-terminal,
point it at an offline fixture profile with more than 10 MiB stored, and stop at
the first seeded fleet row written to the terminal. The performance-only direct
profile route bypasses daemon auto-start without changing production launches;
the former in-process render stand-in is gone. The harness answers the generic
xterm device-attributes probe because a pseudo-terminal supplies transport but
no terminal emulator; without that answer, crossterm's synthetic two-second
probe timeout dominated every launch. A cold-only diagnostic measured 12.0 ms
and 13.8 ms medians with 17.0 ms and 16.3 ms worst samples. Both rows enforce a
median below 100 ms and a worst sample below 200 ms. The four previous
cold-start baselines measured the removed in-process stand-in, so they are
explicitly unavailable until the next deliberate reference recording; their
metric keys and absolute budgets remain enforced, while every unchanged row
continues to enforce its committed drift limit.

2026-09-17 — **Scroll-back qualification walks the client window.** The
50,000-entry workload now opens through the offline UI runtime, executes every
older-page request on the store worker, reconciles and paints after each
result, enforces the desktop window budgets throughout, and follows the same
client back to the newest stored row before measuring returned memory. That
walk exposed the diagnostic recorder retaining every serialized page result;
its replay ring is now bounded by bytes as well as message count, folding
evictions into the exact replay checkpoint. The same Mac14,6 runtime walk fell
from 1.794× retained footprint to 1.057× after following the tip, within the
1.10× budget.

2026-09-17 — **Flood qualification measures a key through the flushed frame.**
The release-loop workload now spreads 2,000 rows across every second in the
runtime's production batch sizes, drains and reconciles them in interactive
loop order, and draws after each pulse. It reports render-through-terminal
flush p99 for every flood frame and key-arrival-through-first-reflecting-flush
p99 for thirty visible composer keypresses. Baseline recording admits an
intentional metric-set change while ordinary qualification still rejects a
missing or extra row. On the Mac14,6 reference machine, 240 flood frames
measured 8.978 ms p99 against 16.667 ms and thirty keys measured 16.808 ms p99
against 50 ms.

2026-09-17 — **Attach and reconnect qualification now cross the live client boundary.**
Reconnect measurements open real exact-cursor subscriptions to an 8,192-row
daemon ring and count the protobuf messages received by the client, including
opening facts. The 10, 100 and 1,000-row deltas measured 3,796, 37,726 and
377,026 encoded bytes. Exact-cursor opens no longer replay the cold-open
attachment snapshot at sequence zero. Attach timing now opens 5,000 stored
entries through the UI runtime while a scripted SDK agent publishes 2,000
rows/s, paints in 3.0 ms, and reaches live with persisted commits drained in
1,028.2 ms after a full second of paced flood traffic on the Mac14,6
reference machine.

2026-09-17 — **The client memory soak runs the shipping client stack.**
Ten chats now open through the UI runtime over a real testnet daemon and an
on-disk store in the sampled process. Scripted providers publish the corpus,
oversized message, unresolved asks and reset from another process, while a
real SQLite writer lock stalls the client's store worker as rows continue.

2026-09-17 — **Performance qualification fails closed across platforms.**
The combined recipe now stops on a release-build or desktop-measurement
failure, and a phone failure remains the recipe's failure. On Macs with Xcode,
the phone suite takes its own lease and prepares the pinned simulator instead
of depending on some simulator already being booted.

2026-09-17 — **Mobile profile tests wait for the boundary they prove.**
The account-switch test waits for its injected inventory result instead of an
unrelated retired store command, while the off-screen attention test observes
the open conversation's needs-you gate before moving its account off screen.

2026-09-17 — **End-to-end fixtures do not consult the live update feed.**
Ordinary scenarios use an inert loopback manifest URL, including fixtures made
from the worktree template; the dedicated update scenario still owns a local
manifest server. The top-level help fixture also names the shipped store
inspection command.

2026-09-17 — **A stopped testnet releases its scripted providers.** The
terminal shutdown path clears the harness-only provider registry after every
daemon has stopped, so an executor that outlives a topology cannot retain its
Claude or SDK fixtures. Restart paths keep their sources for resume coverage.

2026-09-17 — **Embedded integration tests wait for command readiness.**
The attached-daemon and multi-account tests now wait for both inventory
snapshots, matching the reducer's write gate. A transport connection alone can
arrive before the daemon has confirmed the hosts and agents a command targets.

2026-09-17 — **The Mac14,6 release performance baseline is recorded.**
The enrolled reference Mac now has a committed baseline for the complete
desktop qualification under the `release` profile with `bundled,perf`
features. A second run kept every metric inside its absolute budget and drift
limit, the phone suite passed on the pinned simulator, and the ten-minute
memory soak kept both client and daemon growth within their ceilings.

2026-09-17 — **Client and daemon memory stay bounded under sustained structured work.**
The release soak keeps ten chat windows active while fresh provider identities stream for
ten minutes, including oversized and unresolved work, a stalled persistence interval and
a reset. Alongside it, 200 idle and 20 active daemon rings run their live summarizers. The
report samples the platform's named process-memory measure every five seconds, preserves
the full observation count and interval, and enforces the required slopes, peak and
per-agent ceilings.

2026-09-17 — **Desktop performance is qualified at product boundaries.** A
release-only harness now reports machine, configuration, workload contract,
budget, baseline and drift for steady and flooded frames, stored cold starts,
attach and catch-up, provider tip bounds, 50,000-row paging, competing-reader
commits, maintenance, summarizers and exact-cursor wire deltas. It refuses
unknown hardware, keeps baselines keyed by hardware model, and runs the phone
performance suite too when a simulator is already booted.

2026-09-17 — **Performance measurements use the product's real boundaries.**
The terminal trace now records input arrival and the time spent rendering,
drawing changed cells and flushing them. The test harness can sample a
process's physical footprint on macOS and RSS on Linux without treating those
measures as interchangeable, and provider-erased folds expose their retained
tip bytes. Simulator-driven app builds now compile the Rust bridge with the
release profile, with that profile and its features included in the reuse
fingerprint.
The build script remains runnable with Apple's bundled Python 3.9 as well as
newer Python installations.

2026-09-17 — **The app bridge documentation follows the per-account store.**
The bridge README no longer promises a JSON fleet cache or treats remembered
cards as display-only data. It documents the per-account SQLite path, the
cached-fleet fallback and ownership contract, startup ordering and inventory
confirmation, plus the debug entry points that seed a store and open one of
its remembered conversations.

2026-09-17 — **The phone's performance run starts its test relay again.** The
test relay's machines listen on Unix sockets inside a temporary root, and under
the per-user temporary directory the machine called `desktop` put its socket
at 104 bytes, past the macOS limit, so the lifecycle group of `just ios perf`
failed before measuring anything. The root now lives in `/tmp`; the lifecycle
group runs and passes.

2026-09-17 — **The phone shows where a stored conversation is missing
history.** A conversation read from the store drew its entries alone, so after
a reconnect the machine could not fully serve, the rows from before the break
ran straight into the rows after it. The phone projection now places every
break the store window holds (missing history, a change of entry version,
evicted history) as a row of its own between the entries it separates, for
Claude over a terminal, the Claude SDK and Codex, and keeps positions stable as
the window slides past it. Breaks travel in a `history` feed layer the app
draws as a rule, and the terminal client places them through the same
reading-order helper. A new `cached-chat-gap` goldens state photographs a
stored conversation with its missing-history rule in both appearances.

2026-09-17 — **The phone's cold start is measured on a quiet machine.** With
no other simulator suite running, five cold launches read a first frame median
of 458.5 ms against the 500 ms budget and a store read median of 4.8 ms against
its 10 ms budget. The performance document records this beside the contended
runs that read 463 to 484 ms.

2026-09-17 — **The phone's cold start has a store budget of its own.** Each
cold launch now reports its store read as a measurement beside its first
frame, held to a median of 10 ms with no baseline drift allowance, and every
launch must mark one. The end-to-end cold first frame budget on the pinned
simulator rises from 460 ms to 500 ms: runs under other worktrees' simulator
load read 463 to 484 ms while the store read stayed at 4.6 to 7.3 ms, and the
460 ms gate was being missed the same way in unrelated work. A budget with an
empty tolerance is now judged against its budget alone.

2026-09-17 — **A phone launch reads its store once.** The app read the
selected account's remembered fleet in its composition and again when the
runtime started, into the same stores, both before the first frame; switching
account did the same. The runtime's read is now the only one. The cold-start
probe also stopped re-reading the store on every pass of the root view. Cold
first frame now measures a median of 464 ms with one 4.6 ms read per launch,
still over the 460 ms budget on a machine whose unrelated launch phases have
slowed by more than the miss.

2026-09-17 — **The performance document records what the store costs a
launch.** A cold launch's first store read takes a median of 4.6 ms, and the
store brings at most 1.5 MiB of object code to the phone bridge, most of it the
SQLite amalgamation. The cold-start split now reports the first store read of
each launch, not a later one after the first frame. Cold first frame measured
463 and 484 ms in two runs, over the 460 ms simulator budget; the miss is
recorded rather than absorbed.

2026-09-16 — **The phone's cached screens are photographed from real
stores.** Driving builds gain two bridge entry points that write an account
store through the phone's own store, runtime and reducer and open one of its
conversations before anything connects. Three new states use them: a fleet
whose machine is not answering, the same fleet after the machine removed two
agents, and a conversation painted from its stored window; app-hosted tests
assert each and the goldens manifest catalogues them. Reading the cached fleet
now marks store-read signposts, the cold-start probe reads its forty-agent
fleet from a store an unmeasured launch writes first, and the cold-start
report states the store's share of drawing the first frame.

2026-09-16 — **Phone conversations open through the store.** Opening a
structured conversation on the phone now opens its store-backed chat, so the
reader paints the entries the device kept and then catches up from the stored
cursor; closing it flushes and releases the stream. The feed a phone receives
is projected from that chat window, with positions that stay stable while the
window slides and a new window when it stops being one continuous run. The
translation from stored entries to renderer rows moved into the shared UI
state crate, so the terminal and the phone paint a remembered conversation
from the same code.

2026-09-16 — **The phone's remembered fleet is read from its SQLite store.**
The per-account JSON fleet file is gone: each signed-in account on screen now
keeps its fleet in its own store, and the cached-fleet entry point reads it
through the same reducer and projection the running library uses, so every
remembered card is unconfirmed and unsendable. Remembered rows of a paired
machine that has not answered survive the phone's own synchronization until
that machine's inventory confirms or removes them; unpairing drops them in the
reducer and marks them absent in the store. The event queue withholds its
first fleet until the store's rows are installed, so a launch never blanks the
rows it drew.

2026-09-16 — **Claude task tools now drive fleet todo progress.** Both Claude
folds recognize successful `TaskCreate` and `TaskUpdate` pairs from current
Claude Code transcripts, retain a bounded resumable task registry, and keep
the first active task visible through history replay and provider readiness.
Legacy `TodoWrite` sessions remain supported, while conversation resets clear
both forms of todo state. The contract is pinned by an authenticated Claude
Code 2.1.273 capture that also includes an automatically denied write.

2026-09-16 — **Desktop store journeys prove their visible boundaries.** The
warm-start recording now keeps one client alive from a genuinely stopped
daemon through confirmation, chat scroll-back records the SQLite page and its
bounded window, subscription diagnostics attribute open chats to each terminal
across relaunch, and entry-family rebuilding is observed by a restarted client
while another provider survives. SDK resume capture now requires transcript
rows produced by an authenticated Claude Code binary using its current task
tools under an isolated MCP configuration, and rejects synthetic substitutes.

2026-09-16 — **Profile switches leave store work behind immediately.** Retiring
a profile no longer joins its SQLite worker on the UI thread, even when that
worker is waiting to publish into a full message channel. Store polling keeps
its one-second cadence while commands arrive, stops after retirement, and the
startup and maintenance gates are now tested at the reducer and first-frame
boundaries they protect.

2026-09-16 — **Store-backed chat recovery no longer deadlocks.** Catch-up keeps
reading until its replay transition can be committed before applying stream
backpressure. Closing fences conflicts and storage failures instead of
reopening the chat, failed invalidation falls back to a live tail, replacement
streams inherit an existing pause, and oversized canonical results reload from
SQLite.

2026-09-16 — **Stored chats page through history without growing forever.**
Reaching the oldest visible row now fetches the preceding SQLite page. The
bounded chat window retains the end being viewed while paging or streaming,
stays within 800 entries and 16 MiB, and reloads the newest stored window when
the reader returns to sticky-bottom following. Authoritative provider-ready
rows also reopen a cursor that began without enough stored state, while an
empty replay remains safely closed.

2026-09-16 — **Reopened chats keep their pending obligations.**
Provider tips now retain the bounded facts needed to restore answerable Claude
PTY, Claude SDK and Codex asks at the stored cursor. Opening or reconnecting a
chat rebuilds those obligations and its last known turn condition before newer
rows arrive; an unknown pre-cursor state keeps sends closed instead of claiming
the provider is ready.

2026-09-16 — **Stored conversations use their native chat presentation.**
Every durable Claude PTY, Claude SDK and Codex entry now passes through the
same provider renderer as a live stream, preserving prompts, replies, thought,
tool details and results, approvals, agent messages and other provider-specific
blocks. Reopened SDK chats also retain todo progress from their durable summary,
while historical permissions remain visible without becoming answerable.

2026-09-16 — **Remembered fleets stay useful without opening hidden chats.**
The remembered-chat view now restores only the fleet cursor; a chat load and
daemon subscription begin only after the user opens that conversation. Host
facts and authoritative local inventory cuts are persisted, so an agent that
disappeared while clients were away is absent on the next launch. Offline
remembered rows retain their host, age and last standing, while authentication
and daemon recovery guidance remains visible beside the cached fleet.

2026-09-16 — **Store-backed desktop journeys are replayable end to end.**
The `amux store dump` command now renders a profile's stored chat head,
summary, segments, boundaries, keys, revisions and text. A tmux-driven harness
replays offline warm start, high-volume catch-up and paging, two clients sharing
one transcript, gap and version recovery, and a real Claude Code SDK resume,
leaving readable frames, terminal recordings, dumps, subscription diagnostics
and assertion results for each journey. Pending stream mutations coalesce behind
the active SQLite commit so burst traffic does not become one durable
transaction per row.

2026-09-16 — **Client stores maintain themselves after launch.**
The UI store worker now waits until the first frame is clear, then runs bounded
maintenance whenever it is idle, no more than once an hour. Maintenance runs
concurrently with the operation edge so a newly queued read or commit reaches
the store and makes reclamation yield. Explicit chat opens update eviction
recency, while remembered startup and reconnect loads leave that user signal
untouched.

2026-09-16 — **Stored chats recover in place across invalidation and quarantine.**
Invalidating an obsolete or unreadable chat tip now opens a successor segment
from the exact stored high-water and previous cursor, so the store accepts the
first recovered commit while keeping the old segment behind its boundary. A
concurrent invalidation conflict installs the fresh load instead of stranding
the chat, and unresolved durable view state no longer disables otherwise
healthy derived fleet and chat families.

2026-09-16 — **Remembered chats now look remembered while they catch up.**
The terminal paints canonical stored entries before a live provider stream is
ready and keeps positioned missing-history, version-change and eviction
markers in the scrollable feed. Remembered fleet rows retain their last
standing and age while clearly closing sends, and chat chrome now distinguishes
delayed catch-up, host progress ahead of the local head and live-only
persistence failure. Dark and light captures lock every store-backed state.

2026-09-16 — **Store recovery tests now cross the failure boundaries they protect.**
Migration coverage fails after schema work but before its ledger write and
proves the whole open transaction rolls back. Maintenance interrupts a real
integrity statement, large-store open records a bounded SQLite instruction
count over hundreds of megabytes of chat and fleet rows, and chat tests now
exercise equal-version fence conflicts plus family replacement by a second
process, including newer-format refusal.

2026-09-16 — **One maintenance run restores cache budgets.**
Maintenance now repeats bounded transcript eviction, metadata retirement,
retired-table deletion and incremental vacuum work until it converges, reaches
its deadline or yields to a queued store request. Whole-store pressure evicts
least-recently-opened chats to below the restoration target, while desktop and
phone per-chat caps remain 50,000 and 20,000 entries respectively. Explicit
chat opens alone update eviction recency, and sweeping a long-absent agent now
removes all of its derived chat rows while preserving its durable removal
fence.

2026-09-16 — **Stored history keeps exact page and boundary positions.**
Backward paging now advances from each page's oldest entry, returns every row
once, and carries all segment markers crossed by that page. Invalidation marks
the position where its successor will open. Empty-segment collapse consults
actual entry rows and retains one ordered marker inside each collapsed run, and
an Evicted marker from metadata retirement remains visible after caching
resumes, even when the successor has no older retained entry.

2026-09-16 — **Recoverable store data no longer triggers quarantine.**
Unreadable derived fleet rows now report an incompatible cache format while
leaving durable views available, and unreadable chat tips or summaries enter a
baseline-recovery state that can invalidate and open a successor segment.
Stale derivations return a fresh conflict load, while invalid caller mutations
and fold disagreements are refused distinctly from physical SQLite corruption.
Actual corruption during commit or invalidation now stops the worker, releases
its lease and requests quarantine like every read path.

2026-09-16 — **Store leases and linkage checks cover Windows.**
The shared store now uses the standard library's cross-platform file locks for
its shared lifetime lease and exclusive quarantine pass, preserving the bounded
busy wait without a Unix-only dependency. Release linkage inspection reads a
Windows executable's PE import table directly, accepting bundled SQLite while
rejecting any `sqlite3.dll` dependency; Mach-O and ELF checks retain their
symbol and dynamic-library proof.

2026-09-16 — **Every client carries one qualified SQLite build.**
SQLite is now bundled by default through the store, app runtime and iPhone
bridge, so desktop and mobile clients use the same pinned library with the
WAL-reset fix. Release checks reject a dynamic SQLite dependency or an
undefined open symbol, and the complete store suite now cross-compiles and
runs on the leased iOS simulator while reporting the linked and runtime
library identities. SQLite fixes therefore ship to phones in app updates.

2026-09-16 — **The TUI restores each profile before it connects.**
Each selected profile now opens its shared SQLite store on a dedicated worker,
records the cached fleet and remembered-chat results before the first daemon
dial, and executes typed loads, commits, pages, invalidations and durable view
operations behind freshness envelopes. Open chats resume from their exact
stored cursor with bounded batching and pause/resume backpressure; fleet-only
agents no longer open streams. Cached cards remain remembered and all sends
stay closed until the new connection completes its inventory snapshot, while
store failure leaves the conversation available in live-only mode.

2026-09-16 — **Chats paint from the store and catch up under reducer control.**
The pure UI reducer now owns store loads, exact-cursor subscriptions, provider
fold heads, visible windows, paging and flush state behind profile, chat,
stream and operation fences. It paints retained windows before opening their
streams, persists optimistic batches, rebases later work over canonical
results, pauses at the pending-byte ceiling, reloads after conflicts or moved
generations, and degrades visibly to a bounded live-only materializer when
persistence is unavailable. Store startup also restores the cached fleet and
remembered chat before network inventory, and recorded reducer specs replay
store results into the same model.

2026-09-16 — **Store qualification reports its SQLite identity.**
Before accepting or refusing a SQLite library, the store now records its
version, source ID and compile options. The same values are emitted by a
cross-platform test, giving simulator and release checks durable evidence of
the exact system library they qualified.

2026-09-16 — **The shared store enforces its cache and disk budgets.**
Bounded maintenance now evicts old transcript pages behind a monotonic,
positioned boundary, collapses excess empty segments, and reports when durable
or pinned data prevents the store returning to its target size. Alias or
tombstone overflow atomically retires the affected cache without resetting its
fence. All writes preflight physical growth near the disk reserve; durable view
state uses FULL synchronization and quarantine gating, while data-version
polling and readable transcript dumps expose changes and stored history to
clients and diagnostics.

2026-09-16 — **Chats resume from canonical SQLite windows.**
The shared store now loads one bounded transcript window with its continuation
head, fleet standing, progress, redirects, positioned gap markers and paging
token. Optimistic writers commit against generation, fence and head-version
tokens in one immediate transaction; provider entries use the shared merge and
alias algebra, and each result returns the canonical bodies and placements the
visible window needs. Concurrent writers reload on conflict, stale pages are
rejected after any content change, and idempotent invalidation preserves stored
history while closing only the live segment.

2026-09-16 — **The shared store keeps fleet membership ordered across clients.**
Fleet host facts, agent facts, folded standing and progress now materialize in
one SQLite transaction per delta behind durable host cutoffs and per-agent
removal fences. Complete snapshots confirm their members without replacing
newer facts, remove unseen older members, and converge across processes in
either arrival order. Absent rows retain their first removal time for a
seven-day sweep while their durable fences continue rejecting delayed events;
reachability remains an online-only update and cached fleet reads return every
host and agent row.

2026-09-16 — **The shared store opens through a fenced SQLite lifecycle.**
Each profile store now holds a shared sidecar lease on its dedicated worker,
qualifies and configures SQLite before reading schema state, verifies the
durable migration ledger, and rebuilds mismatched derived families by
generation without scanning their rows. Bounded maintenance retires old
tables, checks integrity, checkpoints WAL and incrementally vacuums under an
interrupt deadline. Structural corruption closes the process's handle and
leaves a durable request; a later exclusive-lease open idempotently quarantines
the database, WAL and shared-memory files and records unresolved durable state
in the fresh store.

2026-09-16 — **SDK resume restores block-form human prompts.**
Historical user rows containing text and images now retain their provider
message unchanged and enter the shared fold as replayed prompts. Canonical
interrupt blocks remain status events, while tool-result rows keep their
existing correlation fields and never masquerade as human input.

2026-09-16 — **SDK resume finds Claude's exact project directory.**
Transcript lookup now replaces every non-ASCII-alphanumeric path character in
the same way as Claude Code, so dotted worktree paths resolve correctly. When
no current or sibling worktree path matches, one unambiguous session file in
the project catalogue is accepted as a recovery fallback.

2026-09-16 — **SDK todo specs follow the resume boundary.**
The UI integration contract now proves that a ready row preserves task state
recovered from resume history, while a later conversation reset still clears
that state. This keeps the client-side specification aligned with the shared
Claude SDK fold and daemon resume behavior.

2026-09-16 — **Claude SDK resume behavior is pinned to a real provider row.**
The daemon SDK suite now exercises present and absent transcript files, the
4 MiB and 2,000-line cuts, a partial final line, marker ordering, recovered
todos, historical permission tools without live obligations, and overlapping
final rows. Its identity fixture comes from the authenticated live SDK harness:
the provider's live final, raw transcript row, and resumed historical row share
one UUID and block at the same slot, then fold into one canonical entry.

2026-09-16 — **Resumed Claude SDK sessions publish their transcript history.**
The daemon resolves the resumed session file across sibling worktrees, reads a
single fixed tail, and maps supported transcript rows into the SDK vocabulary
with provider activity times and historical provenance. Gap, begin and complete
markers now frame that prefix before ready and current session facts; live SDK
events wait in a byte-bounded queue during the read. Missing files retain the
existing startup path, recovered todos survive ready, and conversation reset
remains their authoritative clearing boundary.

2026-09-16 — **Fleet standing and native chat now agree on provider rows.**
Fleet badges, status labels, and family needs all use the same selected summary
and working-staleness cap. Recorded Claude PTY interruption rows settle to idle,
and projection fixtures now complete the provider's stop lifecycle before
snapshotting. Codex permission fixtures carry the item identity enriched by the
daemon, so family alerts come from the shared fold rather than injected state.

2026-09-16 — **Fleet revisions reserve durable ranges ahead of publication.**
Each daemon now persists a 1,024-revision upper bound before using any value in
that range. Normal agent and standing events allocate from memory, while block
exhaustion performs the durable replacement and directory synchronization.
Restart skips the unused remainder of the prior block, so no authoritative
revision can be reissued after a crash.

2026-09-16 — **Daemon standing publishes only meaningful cuts.**
The summarizer now compares each folded summary with the last summary it
actually published, so unchanged provider rows and one-second lifecycle ticks
do not create fleet traffic. Progress is independent and advances only after
new rows, at its two-second or 200-row cadence, without forcing a duplicate
summary. Health transitions and real lifecycle changes remain immediate.

2026-09-16 — **Fleet rows consume daemon standing without opening every chat.**
The UI now selects one effective summary by fold version and structured-log
position, preferring the daemon on ties and filling only fields the winner
explicitly does not know. Stale and incompatible summaries remain visibly
aged instead of becoming fresh guesses, while an open chat can win with its
own shared fold when it moves ahead. A two-terminal tmux scenario proves both
clients observe an unattended agent finish even when neither client subscribes
to that agent's transcript.

2026-09-16 — **Daemon-folded agent standing now travels with the fleet.**
Every local Claude and Codex session subscribes to its structured log before
the provider starts, folds standing without materializing entries, and emits
coalesced summaries plus same-cut progress under durable host revisions. A
separate health supervisor marks lagging or disconnected folds stale and
restarts them at an explicitly unknown current baseline. Fleet snapshots and
both event streams now carry validated summary and progress values, including
provider exit state, while terminal-only and test sessions remain unannotated.

2026-09-16 — **The delivery-key allowance is proved by folded rows.**
The identity gate now materializes representative creating forms and every
recorded provider corpus, classifies each delivery-keyed entry from its folded
shape, and rejects any fallback outside the published list. Claude PTY rows
missing identities required by their typed form, plus SDK results and agent
messages missing their required ids, now use the existing unrecognized class;
they can no longer masquerade as valid provider entries.

2026-09-16 — **Claude SDK rows assign every emitted entry a unique slot.**
Prompts assembled from content arrays now use the first text or image block's
slot, so earlier unknown blocks cannot collide with them when the row has no
native identity. Oversized rows reserve the last admissible slot for their
clipped-row marker. Every delivery key remains derivable from that row alone,
and applying either mixed or oversized rows can no longer produce an
equal-revision merge disagreement.

2026-09-16 — **Final Claude SDK blocks retain their streamed placement.**
When a final assistant row proves which provisional text or thinking block it
completes, the authoritative content now amends that provisional entry before
the alias gives it the final row identity. The final entry therefore stays at
the block-start position, including ahead of tools introduced later in a
whole-message final, while clients that first see the final row still place it
at that row and converge on the same key and body.

2026-09-16 — **Codex streams now fold into durable native entries.**
Items, turn snapshots, completed turns, steer echoes and agent envelopes use
their provider identities across replay and late amendments, while the small
set of rows without native identity remains explicitly delivery-keyed. The
persisted tip carries only bounded approval obligations and summary knowledge;
completed text replaces streamed components, token usage supplies context, and
ready session facts supply the model. Recorded-corpus tests now prove postcard
continuation at every cut, deterministic enriched approval and steer rows,
bounded lifecycle behavior and the complete cross-provider duplicate list.

2026-09-16 — **Claude SDK streams now converge on durable final entries.**
Streamed blocks retain provisional provider identities only while their message
cursor is known, then alias into row-UUID final keys whose authoritative
components replace the streamed prefix. Historical rows restore transcript and
todo facts without reviving old obligations, while ready clears only live
obligations. Task lifecycle patches remain row-addressable after eviction and
adopt their launch entries in either arrival order. The recorded SDK corpus now
proves postcard continuation, canonical identity and bounded tips at every cut.

2026-09-16 — **Claude PTY transcripts now fold into durable native entries.**
The pure PTY fold keeps feed bodies out of its bounded continuation tip while
emitting provider-keyed partial entries, row-UUID text components with native
ordering evidence, late tool-result amendments and inferred-turn aliases.
Recorded-corpus tests checkpoint every cut and prove restored folding matches
an uninterrupted materialisation for the tip, summary, entries and redirects.
Lifecycle knowledge, postcard-safe agent messages, clipping and the one-megabyte
tip ceiling now follow the shared store contract.

2026-09-16 — **Transcript mutations share one deterministic merge algebra.**
Provider entries can now merge independently revisioned fields, idempotent
source-keyed components and authoritative final replacements through reusable
postcard-safe primitives. A store-free oracle applies coalesced atomic groups
with canonical alias placement, promotion, tombstone precedence, cycle
rejection, lifecycle fence provenance and the entry budgets that the future
SQLite materialiser and client windows must match.

2026-09-16 — **Provider semantics stay beside their extracted owners.**
The shared Claude folds again document which feed facts are authoritative,
which are inferred, and how provider block, subagent, task and form identities
are interpreted. Codex classification and write-state narrowing, shared agent
message presentation, and answerable-dialog sanitization retain their design
rationale beside the code that now owns each decision.

2026-09-16 — **Bare Claude commands keep their unstated provenance.**
Coverage now pins that a provider-recorded slash command without human origin
or prompt-source facts remains an unstated prompt without opening a turn, even
after a sequence of measured turns.

2026-09-16 — **Codex extraction retains its structural tripwires.**
Unit coverage now follows Codex state into the shared fold for feed bounds,
identity indexes, pending asks and network-policy fallback parsing, while the UI
facade keeps coverage for stream classification and duplicate in-flight input.

2026-09-16 — **Authoritative Codex closure survives a stale observer.**
A provider-confirmed closed thread now keeps its closed send gate after the
session stream becomes stale, while other stale Codex observations still
degrade to unknown.

2026-09-16 — **Codex observations now live behind a bounded shared window.**
Codex feed construction, row correlation, approval obligations, token usage and
activity classification now belong to `fold`. A serializable bounded window
keeps the visible entries and eviction offset in the reducer model, while the
UI facade preserves the feed, paint-watermark and mobile projection APIs and
retains attachments, provider settings, lifecycle and in-flight input overlays.
Authoritative host inventories now forward their complete agent records before
their membership cut, so clients can materialize a fresh remote fleet without
depending on separate per-agent snapshot events.
Provider-derived fixtures now reconnect across semantic reset cuts, retaining
only the fresh generation (including a compaction boundary and summary), and
pin the stable item identity and resolution carried by Codex approval rows.

2026-09-16 — **Claude SDK observations now live in the shared fold.**
SDK feed classification, block and message cursors, task and todo progression,
ask decoding, session model and context facts, and attention transitions now
belong to `fold`. The UI keeps the same public feed and projection APIs through
a facade while retaining optimistic answers, input state, attachments and
connection-local overlays.

2026-09-16 — **Claude PTY observations now cross a client-neutral facade.**
The shared fold crate now owns Claude transcript row classification, provider
fact parsing, todo progression, model and context observation, attention
transitions, diff facts and the provider-native feed entry vocabulary. The UI
keeps its existing Claude APIs as aliases and thin adapters while retaining
asks, optimistic sends, stream gates and renderer-specific attachment content.
The new inventory revision stays absent from human-readable agent JSON while it
is zero, preserving existing mobile projections until an authoritative revision
has actually been observed.

2026-09-16 — **Codex correlation is verified at its provider boundaries.**
Approval requests now have protocol coverage through event ingestion for both
provider item and call identifiers, including ordinary resolution and transport
loss. Steering coverage drives the live Codex control path and verifies that
the resulting row repeats its stable input id and accepted text.

2026-09-16 — **Trust removal withdraws the peer's fleet inventory.**
Unpairing a host now removes every agent learned from that host, emits their
withdrawals and forgets the host's inventory watermark so a later pairing can
start from a lower fresh cut. Ordinary route loss continues to retain the last
authoritative inventory. Local and remote delete calls also reconcile from the
host's locked inventory cut before returning, preventing stale client entries.

2026-09-16 — **Structured row provenance is fixed when the row is published.**
Provider activity timestamps and historical provenance now live in each retained
structured row, so live delivery and later replay expose identical facts for the
same sequence. Claude transcript and SDK messages and Codex events preserve their
provider-recorded time, while ordinary publications default to live provenance.

2026-09-16 — **Fleet inventory publications have a durable host order.**
Each host now reserves and synchronizes its next inventory revision before an
agent appears, changes or leaves. A subscriber opens on one locked
`HostInventory` cut followed by a matching completion watermark, and remote
clients reject duplicate or older revisions while retaining the last complete
inventory through reachability loss. Codex approval rows now preserve their
item and resolution, and successful steer results repeat the stable input id
with the text the provider accepted, so independent observers need no hidden
request state.

2026-09-16 — **A suspended sequence can authorize one continuation only.**
Suspend preparation now drains and parks every path into an agent's structured
log before sealing its watermark. The saved record carries a unique seal that
is durably consumed before a resumed provider starts; replaying a consumed
record recreates the provider under a new agent id instead of reusing its
sequence. Aborting a preparation first records the seal as invalid, then
unparks publishers, so provider output held at the barrier is either published
after abort or discarded only when the agent shuts down.

2026-09-16 — **Structured replay keeps a bounded, explicit semantic cut.**
Each provider log now applies its row and byte ceilings, clips an oversized row
to identity plus a visible marker, and trims an idle ring to its smaller memory
budget. Replay facts follow the complete cursor truth table, including empty
retention, bounded tails and cursor-ahead resets. Provider relinks and resets
publish a fresh marker at a retained reset position and terminate every open
subscription with a reset reason, so reconnecting from the formerly exact
cursor cannot mistake a new conversation for a continuous suffix.

2026-09-16 — **Session streams carry typed rows and explicit replay facts.**
Clients, routed node services and provider runtimes now exchange typed session
arguments, inputs and outputs instead of re-decoding opaque protocol bytes at
each boundary. Structured rows carry the daemon publication time, optional
provider activity time and a replay-versus-live marker. Replay selection uses
an exclusive `after` cursor or a bounded tail and opens with retained,
selected, through and reset positions plus a Continuous, Truncated or Reset
outcome; reconnecting exactly at the current through position replays no rows
and remains Continuous.

2026-09-16 — **The fold and store share one bounded, postcard-safe contract.**
Heads, segments, entries, pages, commit results and fleet deltas now use the
same I/O-free values in `fold`, including the corrected replay cuts, boundary
tie-breakers and canonical commit tokens. Entry keys reject identities beyond
512 encoded bytes, and provider tips, entries and partials must satisfy a
sealed compile-time postcard-safety check that excludes JSON value trees.
Existing human-readable model JSON keeps its wire shape while the persisted
binary form uses external enum tags that postcard can decode.

2026-09-16 — **iOS verification tests exercise the repository Python boundary.**
The CLI sandbox now supplies the `scripts/python` interpreter wrapper that the
verifier actually invokes, so its full-run, missing-baseline and machine-error
contracts no longer depend on an obsolete `python3` PATH shim.

2026-09-16 — **Desktop SQLite selection resolves at every Cargo boundary.**
Every desktop root forwards an explicit bundled feature to `store`, including
the product, embedded runtime, test harness and screenshot tool. Mobile checks
omit that feature, leaving `store`'s bundled feature default-off and linking the
qualified system SQLite library.

2026-09-16 — **Structured observations have one vocabulary below every client.**
The daemon, shared store and UI reducers now meet on sequence, protocol,
attention, lifecycle and summary types owned by `model`; `ui-state` re-exports
the existing UI-facing names while keeping its richer local task list. Empty
`fold` and `store` crates establish the dependency direction before provider
code moves, and SQLite bundling is an explicit desktop build choice so iOS can
continue linking its qualified system library.

2026-09-14 — **The phone pool's hooks run on a Python that can read them.**
Creating, probing and preparing a simulator is done by hooks that ran a bare
`python3`. A hook inherits whatever PATH started it, and on a Mac that is
often the 3.9 the system ships, which cannot even parse these scripts: the
pool then reports that a device does not exist, tries to create one, fails the
same way, and the command that wanted a phone dies with a create failure. The
hooks go through the same interpreter picker the recipes use.

2026-09-14 — **The phone lease was being skipped by everything that needed it.**
A device is leased so two checkouts never drive one simulator, and `scripts/with`
asked for that lease only when `WT_TARGET` was in the environment. That variable
is set when the worktree tool activates a shell, which nothing started outside
one has — an agent, a runner, a plain terminal — so those commands silently
dropped the pool and fell through to the fixed device name that every checkout
shares. Two checkouts then drove one phone, and the one that prepared it second
uninstalled the application the first was testing. What makes a checkout the
tool's is the directory it keeps there, so that is what is asked now, and a
command that drives a device without a lease in such a checkout is refused with
the wrapper to run it under.

2026-09-14 — **One command at a time on one simulator.**
UI tests were dying intermittently with `Test crashed with signal kill`, at a
different step each time and with no crash report, and a `simctl` call right
after one of them once failed asking after an application that was not
installed. The simulator's own log says what happened: the test runner was
force-quit by a request from the Mac, in the same second as the application
under test was uninstalled and a fresh build put in its place. That is what
preparing a device does — every running app is terminated, the build is
reinstalled — and it was being done to a device another command was in the
middle of using. Two checkouts can mean the same device more easily than it
looks: a phone is only leased when the worktree tool has activated the
environment, and anything started outside that activation falls through to the
same fixed device name in every checkout. Preparing a device now waits for
whoever is driving it and keeps it until the command ends, so the second
command queues instead of pulling the first one's application out from under
it. A command that starts another command passes on what it holds, so a recipe
that calls a recipe does not wait for itself.

2026-09-14 — **The cached chat's relaunch is filmed, not just photographed.**
A still cannot say that the rows a relaunch drew out of the cache are the rows
that were already there, or that the turn the phone missed arrived under them
without moving them. The cached-chat journey now films the simulator across
exactly that stretch, with the camera the streaming conversation already used:
the test writes a word into its own container and the Mac starts and stops the
recorder on it. That recorder writes a frame only when the screen changes and
holds the last one it has until another arrives, so the state a film ends on is
never in it: the first films stopped on the reconnecting chat and never showed
the turn they were taken for. The missed turn is now left up long enough to
read and the phone is then put away, and that is the change which writes it.

2026-09-14 — **The cached-chat journey relaunches without orphaning its test.**
The simulator now force-quits a UI-test runner shortly after that runner sends
a standalone termination request to its application. The application was gone,
but so was the test before it could inspect the cold launch. The journey now
puts the first launch in the background, takes the relay offline before the
machine produces the missed turn, and asks XCUITest for one relaunch operation.
That operation still replaces the application process and exercises the cache
on disk, without exposing the runner to the broken intermediate lifecycle.

2026-09-14 — **A machine you have not called is not a machine that is down.**
The phone's fleet reported every remembered machine as unreachable until this
session had heard from it. That reads as a fact about the machine, and the home
screen treats it as one: a dark machine outranks every other reason a row might
need attention, so a conversation remembered waiting on a person announced
itself as an unreachable machine instead, for as long as the account link took
to come up — and on a launch with no link at all, indefinitely.

A first frame drawn from disk is a picture of what this device last saw, and it
now says so about machines exactly as it already did about agents: each one is
reported with the reachability it was written down with, marked as remembered,
until this session actually hears otherwise. The moment a machine answers — or
the account's paired list stops naming it — what it says replaces the memory.

2026-09-14 — **A silent machine no longer erases the conversation you were
reading.** A client's own node finishes its inventory snapshot the moment it is
up, and while the account link is away it finishes with nothing at all from the
machines it cannot reach. The reducer treated that completed snapshot as proof:
every row read from the cache was dropped, along with its conversation and its
place in the stream. On the phone that showed as a remembered chat opening with
an empty transcript — the header knew the agent, and there was nothing under it.

A snapshot this device completed on its own behalf is now only authority over
this device. A row nobody has confirmed this session survives it, with its
transcript and its cursor, as long as its machine is still one the account is
paired with and that machine has not itself said what it is running. What does
disprove such a row is unchanged and still immediate: the machine's own
inventory arriving without it retires it there and then, an unpaired machine
takes its rows with it, and a row on this very device is gone if the local
daemon did not name it, because that daemon is authority over its own agents.

The fleet screen already drew remembered rows by re-adding them on the way out;
that rescue is now mostly redundant, and the one thing it still decides — is
anything on screen only a memory — is read off the rows themselves.

2026-09-14 — **A seeded phone reads what the client wrote.** The harness that
gives an iPhone journey a phone with memories was writing a file nothing reads:
it dated from before the cache moved to one directory per account, and it
carried a fleet event rather than the fleet index the shared cache keeps. So
the two home journeys seeded nothing and drew nothing.

Rather than teach the harness the format a second time, one remembered account
is now pinned beside the projection's own schema: a fleet index and a chat
layer for each state a home row can be in — a request waiting for permission, a
finished turn with its landed edits, work under way, a session idle — folded
from a scripted Claude session and written through the cache the phone itself
reads. A test re-folds them and fails on any drift. The harness copies those
files and rewrites only what a run cannot know in advance: which machines are
running, which agents are on them, and when each last did anything.

Seeding a chat is not an extra: a remembered row's attention is not a field of
the fleet index. The library reads it off that agent's own cached conversation,
so a row remembered as needing permission has to be seeded with the chat in
which it asked, and a row remembered in no state at all is seeded with no chat.

2026-09-14 — **A failed journey keeps what it found.** A UI test that fails
still writes its record and still leaves the app's own runtime log behind, and
both used to be thrown away with the container: the run reported an assertion
and nothing about the screen behind it. Whatever the test managed to leave is
now collected either way, and a failing run keeps the app's log beside the test
output, so the next reading of a failure starts from what the app was doing
rather than from a second run.

The first thing it says is that a remembered chat does not draw. With the relay
down, the conversation journey's relaunch shows the fleet row it remembers and
opening that row shows a chat with nothing in it at all — the header knows the
agent and when it last moved, and the transcript is empty. Letting the relay
back fills it, so only the cached open is wrong.

2026-09-14 — **Leased phones, and three checks that came over red.** Two fixes
to the same problem met in the iPhone recipes. Every recipe that touches a
device now runs under a wt lease, so two checkouts never drive one simulator at
once and a capture is no longer killed halfway by a sibling's install. And the
interpreter those recipes run on is resolved rather than inherited, because a
Mac's `python3` is frequently the system's 3.9, which cannot read the manifests
these scripts parse. Both belong: the lease on the outside, the resolved
interpreter on the inside.

Three checks needed repair before the tree was green again. The golden manifest
gained a screen for the two row states nothing had ever photographed, while the
total that guards the catalogue still named the old count. The verification
runner's sandbox fakes an interpreter on the path, so asking for one by
repository path found nothing there; that path now stands in for the same stub.
And a bridge counts as current only when its slices hold archives rather than
merely existing as directories — a distinction a restored build cache makes
real — which left the test that proves cargo goes unrun asserting it against an
empty directory that no longer qualifies.

2026-09-13 — **A remembered chat, proved on a phone.** The iPhone journeys now
carry the whole story the cache exists for, against a machine the runner is
really running. The phone pairs with it, reads a turn of one of its agents over
a real relay, and is taken away; the machine runs a second turn while nobody is
watching, and the relay is taken down before the phone comes back. The launch
that follows can ask nobody anything, so everything it draws it kept: it opens
on the fleet with the row it remembers, saying so on the row, and the chat
draws its own rows the moment somebody opens it — the same rows, under the same
identities, with nothing of the turn it missed. Letting the relay back appends
that turn underneath, and every earlier row is still in its place under the
identity it was drawn with.

It is a UI test rather than a plan spoken through the app's door, for two
reasons. Opening a chat is a tap, and only an accessibility client can press
one; and the story is two launches of the same install with the app taken away
between them, which only a test driving the app can do. The app is killed
rather than asked to stop, exactly as a phone kills it, so what survives to the
second launch is whatever the runtime's own short write window had already put
on disk — the test waits for that window before taking the app away, because
nothing flushes on the way out.

Row identity is read through the door rather than off the screen. On screen a
transcript row is named by its kind, which every row of that kind shares, so a
claim that a row is the same row has to be made against the identity the row
was drawn under: its layer and its own number within the window.

2026-09-13 — **The suite that catches drift could not run.** Three of the
iPhone package's test resources are symbolic links to files the Rust workspace
owns — the pinned projection schema, the ask and queue fixtures, and the
measurement document — so that changing the source fails the Swift suite
rather than drifting past a stale copy. Moving the app one directory deeper
left each of them pointing a level above the repository. They resolved to
nothing, the two package suites that read them failed to build, and the check
whose whole job is noticing a changed contract was the one that could not run.

2026-09-13 — **A remembered chat opens on its own rows.** A phone that has a
conversation in its cache now draws it the moment somebody opens it, before
the relay is up and before the machine that owns it has said anything, and the
stream that will refresh it is opened by that tap and by nothing earlier: a
fleet on screen subscribes to no conversation at all. The stream then resumes
after the sequence the cache kept, so a machine that can continue from there
sends only what was said since — the rows already on screen keep their
positions, the delta arrives underneath them with identifiers of its own, and
nothing is rewritten. A machine that cannot continue says so, and the whole
remembered window is dropped behind a boundary rather than spliced onto rows
it may not follow.

The indicator while that settles is the one the app already has: the
conversation's foot, which says a session is replaying what it missed while
the stream catches up, and names the machine as unreachable while the relay is
still away. The remembered rows themselves are drawn exactly as live rows are.
They are not dimmed, greyed or shimmered: they are the real conversation, they
are what the person opened the chat to read, and animating text somebody is
reading to say that more of it is coming would cost the reader more than it
tells them.

One thing the phone could not read. A cached conversation reports a stream
phase no live session ever reports, and the app's decoder knew every phase but
that one — so the first remembered chat anybody opened would have thrown its
whole session away and left the screen with no gate, no provider and no way to
send. The phone knows the phase now, and the pinned projection the app's suite
decodes carries a remembered conversation, so a client that forgets it again
fails there instead of on somebody's screen.

2026-09-13 — **A silent machine keeps its remembered rows.** The phone decided
what was worth writing to its cache from two facts that look decisive and are
not: the relay was up and its own node had finished its snapshot. That node is
no authority over another machine's agents. It completes as soon as it is up,
with nothing at all from a machine that has not answered, and the reducer then
drops every row that snapshot did not name. So a phone that reconnected while
one paired machine stayed quiet went on drawing that machine's rows as
awaiting — and wrote the blank behind them to disk, taking the fleet cards and
the cached chats with it. The next cold start opened on nothing.

The cache now records exactly what the screen calls reconciled: every paired
machine has answered over a relay that is up, and no row on the fleet is still
only a memory. It is the same verdict the fleet callback carries, so what the
person sees and what the phone keeps can no longer disagree.

2026-09-13 — **The phone recipes pick their own interpreter.** The scripts
behind `just ios …` read Cargo manifests with tomllib, which arrived in Python
3.11, while macOS still ships 3.9 as /usr/bin/python3. A shell whose PATH did
not reach a newer interpreter — a CI runner, an agent's stripped environment —
failed on the import line with nothing to say about what was missing. The
recipes now run `scripts/python`, which finds the first interpreter that has
tomllib on PATH or in the two prefixes a Mac keeps a newer Python in, and says
what to install when there is none.

2026-09-13 — **A remembered row says what the last turn changed.** The fleet
card had a field for the arithmetic of a finished turn and nothing ever filled
it: the inventory a machine sends counts no changed files, so a row that said
an agent had finished could not say what it finished. The numbers are in the
chat itself. Every landed edit a Claude session states carries the file and
the lines it moved, so a card now sums the edits between the prompt that
opened its most recent finished turn and the row that closed it — distinct
files counted once, however many times the agent touched them.

The same feed comes back from the cache, so a phone opening cold states the
same turn it stated before it was closed, before any machine has answered.
Two turns are deliberately not summed. One whose opening prompt has been
evicted may have landed edits that went with it, and the remainder would be
too small, so the row says nothing rather than an understatement. And Codex
names the files a turn touched without ever counting lines; a row reading
"+0 -0" over three changed files would be wrong, so a Codex row states its
turn without arithmetic until the provider counts them.

2026-09-13 — **The phone remembers through the shared client cache.** The
iPhone app kept its own fleet file beside the reducer and merged it back into
every callback. That file could never hold a chat, so a cached transcript on
a phone would have needed a second format with its own rules. The app now
uses the client cache the rest of the workspace uses, one directory per
account under the phone's cache directory, and its runtime seeds its Model
from it and writes back through it.

Two things moved rather than disappeared. A remembered row is still only a
memory until the machine that owns it answers, and the reducer cannot carry
such a row past a completed snapshot, so the phone's projection keeps drawing
it until a machine's own inventory or the account's complete paired-host list
removes it. And a phone's node finishes its snapshot as soon as it is up,
with nothing from machines it reads over a relay that is away; writing that
back replaced what the phone last really saw with a blank, so a cold start
after an offline session opened on nothing. A runtime can now be told when
what it holds is worth remembering — the phone says yes only while its relay
is up and its fleet is whole, and the whole fleet is written the moment it
is, rather than waiting out a write window the connection may not survive.

2026-09-13 — **Merge the native iPhone app and move the flight onto just.**
Main now carries the app, so this branch takes it. Both sides had typed the
session seam independently again: main's host-side supplier of provider
sessions is richer than this branch's SDK-only opener, so the supplier wins
and the opener is gone, including on the resume path main never exercised.
Everything else keeps this branch's design in main's layout. The client
service routes a repository listing by the host id it reads, and forwards
the same message the host answers. The agent event stream carries main's
host inventory, so a conversation the person opened survives an unreachable
host; that map also remembers a chat opened before its card arrives, which
is what this branch's fix was for.

Two on-disk shapes moved. The cached chat layers gained the app's todos,
cursor and provider facts, so all three layer schemas are v2 with fixtures
regenerated under the new names. A stream's opened message defaults its
resume outcome to fresh, which is exactly what every recording written
before the field meant, so the committed phone bundles still replay.

The flight's own commands now run through just: main retired wt as the task
runner, and the milestone and task checks said wt. Its gate asked whether
the app had landed; it now asks whether the app and its crates are here and
still build for the phone.

2026-09-13 — **Refuse a permission decision the wire cannot express.** The
merge had left the Claude SDK permission decision's wire encoder infallible:
any JSON that was not an explicit deny went out as an allow, so a client
sending a malformed decision would have let a tool run. Encoding now fails
for a missing or unknown behavior and the client reports it as an encode
error, as both parents did; the decoder no longer pads an allow with null
keys, so every input shape round-trips equal and the wire tests say so for
every Claude PTY intent, SDK input and Codex input. The default log filter
and the cache scenarios name the client crates main renamed, which is what
had hidden the discard log; `just e2e-build` builds the retained-rows
example the scenarios drive; and the runtime's cache tests cover the
discard that follows pairing.

2026-09-13 — **Keep the cache's disk out of the reducer crate.** The client
cache had landed in `ui-state` whole, file I/O and log lines included. The
on-disk shape now stays there as values and pure functions: the fleet and
layer files, their schema versions and bounds, header and semantic checks,
bounded encoding that sheds the oldest entries, and the pairing that turns
parsed files into cached state. The store that owns the directory, writes
atomically and discards what it cannot trust moved beside the runtime in
`ui_runtime::cache`. The reducer specs round-trip through the serialized
form without a temporary directory; the runtime's own tests cover the disk:
discard logging, oversize files removed before parsing, an unusable
directory starting cold, and concurrent writers in threads and processes.

2026-09-13 — **Merge the crate split from main into the cache work.** Main
split the daemon into model, wire, host-api, node and agent-runtime crates
while this branch was typing the session seam end to end; both had built
the same enum pair at the host boundary. The merge keeps this branch's
design and lands it in main's layout: session arguments, inputs, outputs,
the shared replay query and replay facts are values in `model`, the
protobuf oneofs are translated once in `wire`, and the host API, node
services and client all speak those values. Name-based agent lookup and
its ambiguity error are gone from the wire; the CLI resolves names at its
own edge. The scripted Claude SDK peer no longer hides behind a build
flag: the runtime exposes an ordinary session opener, the harness installs
it, and the stub binary, its example reader and the cache scenarios live
with the rest of the test infrastructure in `testnet`. The client cache
sits in `ui-state` for now; its file half moves next to the runtime in a
follow-up. Seven daemon unit tests that main's test-ownership refactor had
removed stay removed.

2026-09-11 — **Record the offline terminal warm start end to end.** The cache
harness now drives a real terminal in four scenarios. Each seeds an agent's
chat, stages what the scenario is about while the terminal is closed, then
starts the terminal again with the daemon stopped mid-syscall so only the
cache can answer: the fleet paints its remembered row, Enter opens the
remembered chat on its cached transcript, and the daemon is let go to finish
the story. Resume replays only the rows after the cached cursor over one
subscription; an evicted cursor refolds a tail behind the missing-history
boundary; sequence numbers continue across a suspend and resume, so the
cursor taken before it still resumes cleanly; and a chat layer written under
another schema is discarded with one log line and no error on screen.

Two things had to be fixed to get there. Opening a chat before the
connection was up spent the cached cursor on a subscription that could not
be made, so the reconnect replayed a plain tail; the subscription policy now
waits for the connection, which arrives with the inventory that opens the
stream from the cursor. And the row reader can now send several prompts over
one subscription — the session's ready row is what permits a prompt, and a
later subscription may find it already evicted.

2026-09-11 — **Open the fleet from the profile config on disk.** Bare `amux`
and `amux ui` no longer wait on a server round trip to learn which profile
they are opening. When the disk already names it — an explicit `--config`
path, a profile selected by id, or the id remembered from the last session —
the config file is loaded and the terminal starts at once, painting its
cached fleet while the connection is still being made. Choosing a profile by
account name, or having nothing remembered, still needs the directory of
profiles and resolves as before. Every connection attempt continues to
resolve the profile through the server, start it when absent, refuse a
profile reported unavailable — now as the fleet's disconnect reason rather
than a failure before anything is drawn — and record the selection.

2026-09-11 — **Script the offline terminal warm-start captures.** The cache
harness now takes a scenario name and shares one setup: an isolated daemon
with the scripted Claude process on PATH, a profile whose terminal opens chats
on Enter and asks for a twenty-row tail, and helpers that read a cache
directory or assert one claim about an agent's session in a daemon debug dump.
The resume, gap, suspend and schema scenarios seed a real agent through a
terminal that caches its chat, then stage what each is about while the
terminal is closed. Their warm half is blocked: pausing the daemon leaves the
terminal with nothing on screen, because every command resolves its profile
through an unbounded front-door request before the fleet is drawn.

2026-09-11 — **Complete the shared client cache boundary.** The UI library now
owns the versioned fleet index, protocol-specific durable chat layers, atomic
bounded storage, warm model seeding, cursor continuation and gap refolding,
and coalesced runtime writes as one documented facility. Each client supplies
its own kind-owned profile root; schema mismatches and invalid or oversized
files degrade to a logged cold start rather than a user-facing failure.

2026-09-11 — **Persist UI cache changes from the runtime shell.** A runtime
with a client cache now restores its model before connecting, tracks durable
fleet and per-agent changes after each fold, and coalesces row bursts into one
write per agent every 250 milliseconds. Agent updates and removals reach disk
before the fleet index, while explicit shutdown flushes bypass the delay and
preserve the same ordering. Deterministic paused-time tests cover coalescing,
inventory removal, write order, and immediate flushes. Embedded-daemon tests
restart the runtime from that cache and verify both delta-only continuation and
a single bounded-tail refold when the daemon has evicted the saved cursor.

2026-09-11 — **Bound every client cache file before it reaches a cold
start.** Fleet snapshots above one MiB are rejected before parsing. Chat
snapshots above two MiB shed the oldest quarter of their retained entries per
pass, recording those evictions so the feed keeps an honest history boundary;
if non-feed state alone exceeds the limit, the old agent file is removed and
that chat starts fresh. Oversize files already on disk are likewise removed
before their JSON is parsed.

2026-09-11 — **Keep fleet and chat caches in kind-owned atomic files.** The
UI cache now writes a separately versioned fleet index and one versioned
durable layer envelope per agent beneath a client-kind directory. Loads check
size, schema, ownership, complete shape, layer pairing, cursor consistency,
and fleet membership in a fixed order; one invalid chat cannot poison the
remaining cache, and every rejected file is removed with one diagnostic line.
Temporary files use process-unique names and are synchronized before an
atomic rename, so readers never observe a partial JSON document.

2026-09-11 — **Give each chat layer an explicit durable form.** Claude PTY,
Claude SDK, and Codex folded state now converts through protocol-specific
cache structs with independent schema versions instead of serializing the
live model. Reload rebuilds dedupe indexes and clears connection-scoped replay
flags, staleness, optimistic echoes, and in-flight inputs. Recorded provider
rows exercise each round trip, while committed, version-named JSON fixtures
make an on-disk shape change require a deliberate schema bump.

2026-09-11 — **Prove command-line agent selection across paired hosts.** An
end-to-end scenario creates colliding agent names on two hosts and confirms
that attach refuses the ambiguous name with both ids, host ids, and working
directories. The same scenario attaches to a uniquely named remote agent and
exchanges input and output successfully.

2026-09-11 — **Make the Rust client API identify agents by id.** Per-agent
client operations no longer perform hidden fleet scans or accept ambiguous
names. Message sends take the recipient's agent-and-host address and build the
authenticated envelope in the client, while artifact puts explicitly state
whether they are agent-authored attachments and child deletion carries the
caller's id. Existing UI, CLI, examples, and test-network callers now resolve
names at their own edge before using the shared API.

2026-09-11 — **Forward host requests unchanged through the client service.**
Routed per-agent calls now use the shared host request message itself: the
client service reads only the agent id needed to choose a host, executes local
requests directly, and forwards the original message to a remote host. Session
arguments and inputs are decoded only on the host that owns the agent. Message
delivery likewise validates the typed envelope sender while retaining the
original wire envelope for remote delivery.

2026-09-11 — **Use typed session values directly in the UI runtime.** The UI
now selects structured subscription arguments through one exhaustive protocol
match, sends provider-native inputs inside the shared session enum, and turns
shared sequenced rows into feed entries through one checked conversion. The
remaining UI protocol-name constants and string conversion have been removed.

2026-09-11 — **Carry typed session values across client and host services.**
Session subscriptions, inputs, controls, and outputs now cross the public
client, routed client service, daemon internals, and host service as shared
Rust enums. The protocol oneofs are translated exactly once at the wire edge;
protocol-name strings, opaque protobuf byte payloads, and the per-protocol
codec functions are gone. Structured protocols share one sequenced row type,
while their provider-specific input vocabularies remain typed. The CLI, UI
runtime, test network, and SDK row example now construct and consume these
values directly, and focused tests round-trip every top-level enum arm and
reject missing nested oneofs.

2026-09-11 — **Share host request messages across the client service.** The
client protocol now reuses the host inventory stream and every host request
type for per-agent operations. The shared create, delete, and artifact requests
carry the optional destination host, calling agent, and agent-attachment fields
needed at the routed boundary; sends carry the complete envelope. The separate
agent reference, mirrored client requests, client inventory messages, and
ambiguous-name error detail are gone from the wire format. Generated Rust and
the descriptor set were regenerated, and existing callers now construct the
shared messages while the public client temporarily resolves its legacy name
inputs against the fleet until the CLI-only resolution change lands.

2026-09-10 — **Reject structured replay cursors ahead of the sequence watermark.**
A resumed buffer now reports a replay gap when a client's cursor implies a
next sequence beyond the buffer's current successor. The subscription uses the
existing tail fallback, forcing clients to refold behind a missing-history
boundary instead of silently accepting sequence numbers that the resumed
backend can issue again. The exact successor of a suspended cursor remains a
contiguous empty replay.

2026-09-10 — **Persist structured sequence cursors when agents suspend.** Every
Claude, Codex, and test-agent suspend record now requires the structured log's
current sequence number, captured asynchronously with the rest of the backend
state. Suspended files written without that field are deliberately incompatible:
the loader resumes no agents and emits one error naming the rejected file.
Restored structured logs begin at the stored cursor, including Codex logs with
capture enabled, so their first new row is the cursor's successor and a client
continuing from that cursor sees no replay gap. Focused tests cover capture and
continuation for every structured backend plus the missing-field boundary.
The installation-level restart spec now suspends through the administrative
RPC, reopens the persistent installation, resumes the scripted SDK agent, and
proves that a pre-suspend cursor receives the very next sequence without a
gap. Resumed testnet SDK agents retain the same offline provider seam as fresh
agents.

2026-09-10 — **Add a deterministic offline Claude SDK transport.** The
scripted stream-JSON provider and its host seam were first copied byte for byte
from nativeapp commit `1c112501`, preserving the real SDK backend boundary. A
separate follow-up commit added configurable assistant rows per prompt, idle
ticks, and a public transport-generic serve loop; the stdin/stdout executable
is only a front end over that loop. An isolated live smoke now launches the
stub through the real daemon, sends a prompt through the public session API,
reads the sequenced stream, and observes idle eviction at the configured
30-row retention bound. The daemon retention and client replay-tail settings
are explicit test knobs. A runtime-level regression now drives a structured
agent open through the shell and observes the configured replay tail at the
stream dispatcher, ensuring that the client knob is not merely parsed but
actually applied. Provider meaning and richer row shapes remain the recorded
corpora's responsibility. When nativeapp later enters this branch
through main, `crates/amux/src/testnet/sdk.rs` will conflict because this branch
extends the copied file; resolution must retain the row-count, idle-tick, and
generic stdio changes on top of the shared baseline. Formatting, workspace
lint, focused memory and process tests, and the offline real-daemon smoke pass.
2026-09-13 — **Moved the iPhone app to `apps/apple/`.** The app was built on
its own branch at `ios/` and has now merged, so it takes the place the
licensing split made for it. Applications sit under `apps/`, the licence that
covers them sits at `apps/LICENSE`, and the root LICENSE scopes by that
directory rather than by one app's name — so a second application needs no
licensing decision, only a directory.

Two hundred and three references followed the move, in recipes, scripts, the
CI workflow, the end-to-end topologies and the docs. `target/ios/` did not:
that is where the Rust bridge's xcframework is built, it has nothing to do
with the app's sources, and eighty-five references to it are deliberately
unchanged. The root justfile now names the app's recipe file by path, so
`just ios <recipe>` keeps working from anywhere in the checkout.

Entries below this one describe the app at `ios/`, because that is where it
was when they were written.

2026-09-13 — **Split the repository's licensing by directory.** The core is
now dual-licensed MIT or Apache-2.0, the Rust ecosystem's convention, so
anything depending on amux composes with it without reasoning about licences.
The iPhone app at `ios/` is source-available under the Functional Source
License 1.1 with an Apache 2.0 future licence: everything is permitted except
shipping it as a competing product, and every version converts to Apache-2.0
two years after it is published.

The two-year clock runs per version from the day code is made available, which
for a public repository is the day a commit is pushed. Nothing has to be
tracked, no change date nominated, and several applications on different
release schedules need no shared calendar — each commit dates itself. The terms land
before the app itself does, so no version of it is ever published under a
licence that does not mean to cover it. The app moves to `apps/apple/` after
the merge, taking its licence with it.

# amux Development Log

Dated decisions, behavior changes, and regression causes. Compacted 2026-09-14;
full entries remain in Git history. Later entries supersede earlier behavior.

## 2026-09-14

- SwiftUI controls at rest must have no scale wrapper: `scaleEffect(1)` still
  resamples glyphs, and a settled `.scale` transition changed card layout rounding.
  Press/arrival transforms are conditional; custom styles explicitly dim disabled buttons to 0.5.
- Fleet `RowState` centralizes precedence and wording for list, drawer and VoiceOver.
  Offline hosts outrank requests; expired working inference shows neither a mark nor
  a state word. `home-offline` captures both cases; precedence has direct unit coverage.
- Golden `--update` compares before copying and rewrites only mismatches, reporting
  each replacement. Previously every update compared the replacement with itself.
  Catalogue checks also reject baseline files not claimed by the manifest.
- Cold transcript openings could miss the tail when the bottom inset arrived after
  the initial scroll. Observe that inset and retry up to eight times until reader input.
  Lazy offscreen height estimates also shifted glyphs fractionally; affected capture
  fixtures now retain the visible tail. All three golden quarantines were removed.
- GitHub capture differences came from SpringBoard chrome: two booted simulators
  could leave the home indicator visible, and status-bar appearance updates arrived late.
  The comparator excludes manifest-declared system chrome and marks exclusions in diffs;
  app-pixel tolerance remains two per channel with a 64-pixel ceiling. Nightly goldens resumed.
- Simulator recipes resolve `golden`/`small` device roles through wt leases; a worktree
  without its required lease is refused. CI uses pinned standalone devices. Lease
  acquisition resets app/accessibility state; generic simulator builds require no lease.

## 2026-09-13

- Fleet marks now identify only attention requiring a person. Working is a word;
  offline rows name the machine; unreadable providers say to update amux. Expired
  Claude PTY working inference makes no diagnosis beyond the row's last-seen age.
- iPhone verification separates push-time structural/unit checks, capture checks,
  and shipping package/scope checks. `just ios verify` runs the complete sequence;
  `just ios release` requires shipping checks. Rust workspace checks run in their own CI jobs.
- XCFramework freshness and packaging both inspect every slice's actual library.
  Restored caches can contain framework directories without archives; directory
  existence alone falsely skipped rebuilding and then falsely skipped repackaging.
- The iPhone app moved from `ios/` to `apps/apple/`; `target/ios/` remains build output.
  Relative test-resource symlinks needed an extra parent level after the move.
  Core code is MIT/Apache-2.0; applications are covered by `apps/LICENSE` (FSL-1.1-ALv2).
- Native integration now has three crates: `app-runtime` owns account sessions,
  projections/cache and batching without node dependencies; `app-embedded` owns the
  provider-free installation; `app-ffi` exports `amux_app_` C functions. Only explicit
  debug tools permit plaintext loopback relays. Views do not own attached daemon lifetime.
- Scripted provider input observation is fallible. Dropping an unexpected-input
  refusal left an optimistic phone echo waiting forever for a row the script would
  never emit. Refusals now reach the original send outcome.
- Retry Now cooldown/coalescing is tested with a virtual clock and no socket:
  one burst shortens one wait, preserves subsequent backoff, and a later press works.
  Real failed dials consumed the cooldown on Windows and made earlier timing tests flaky.
- Complete native visual galleries and corrected performance baselines were approved.
  The optimized coherent run recorded 444.5 ms median cold frame, 75.38 MB footprint,
  zero median streaming hitches/idle commits and 241.7 ms foreground recovery.
  Physical-phone cold start and ProMotion hitch behavior remain unqualified.

## 2026-09-11–12

- Split the Rust monolith into `model`, `wire`, `settings`, `artifacts`, `client`,
  `host-api`, `node`, `agent-runtime`, `ui-state`, `ui-runtime`, `tui` and CLI `amux`.
  Node owns routing/profile authority; agent-runtime owns providers and persistence;
  client connections are explicit; the reducer is pure; UI resources belong to ui-runtime.
- Host factories create isolated provider runtimes per profile; embedded owners omit
  them. Admission uses owned work leases/exclusive barriers: close admission before
  draining accepted work and deleting storage. Node journals opaque provider update state.
- Testnet owns whole-daemon topology/specs, live-provider harnesses and `testnet serve`.
  Hidden harness seams compile normally; test-only feature/profile variants were removed.
  TUI fixtures retain one intentional non-default feature; provider corpora have spec-crate owners.
- `just` owns build/test/lint/CI recipes; wt owns worktrees, resource leases and daemons.
  Rust 1.98.0 and formatting nightly 2026-08-30 are pinned. Product and tests share
  the development configuration; dependency policy checks declared Cargo edges.
- wt 0.4.0 replaced the earlier build-retention experiments. Two warmed APFS snapshots
  reused product/test builds without compilation; five concurrent edit/test/lint/build
  cycles stabilized around 13.8 GB logical output per tree. Earlier wt 0.3.0 sweeping
  alternately deleted valid dependency fingerprints and caused unchanged builds to recompile.
- The selected iPhone design now uses production shell, stores and native editor across
  every surface. Notification/Mute exclusions were removed from production; no parallel
  preview UI ships. Accessibility targets are at least 44 points without enlarging artwork.
- Malformed performance fixtures had produced near-empty transcripts and a preconfirmed
  cached fleet. Their earlier green results and 68.4 MB memory baseline were invalid;
  corrected tests assert the actual 1,000-row content and cached-to-confirmed transition.
- Transcript optimization separates observation boundaries and bounds append publication
  to 33 ms while applying every event immediately. Cached folds reopen only affected groups;
  replacement-only and eviction-only updates must invalidate them or finalized rows disappear.
- The largest text size exposed a layout cycle: the bottom panel was sized from the
  viewport it reduced. Size from the outer page instead. Global geometry probes on live
  rows also caused churn; exact probes now belong to explicit audit/capture/placement work.
- iPhone report replay restores routes, account/access state, capture/ranking clocks,
  draft text/caret/tokens, open panels, dismissed changes and reading position.
  Position is an entry plus an offset within it, measured in viewport coordinates;
  stale absolute feed offsets restored the reader eleven rows away.
- Replay compares against the report's own frozen frame; updating replay output cannot
  overwrite that oracle. Fixture openings reset view identity and environment defaults,
  preventing prior menus, navigation and accessibility text sizes from leaking.

## 2026-09-10

- iPhone release writes version settings, archives, exports and validates with Apple
  before committing/tagging. It never pushes or uploads. Failure leaves only project
  version edits; rehearsal verifies the worktree is unchanged. Same unreleased marketing
  version is allowed with a new build; the first build number must account for App Store history.
- Export uses manual Apple Distribution signing and an installed, unexpired profile.
  Team ID comes from certificate OU, not the Development certificate's personal identifier;
  expiry is compared in UTC. Step timeouts must finish before the wrapper's recovery deadline.
- Apple validation exposed a missing icon missed by simulator tests. The existing app's
  1024px artwork is committed; source/bundle checks require a top-level `CFBundleIconName`,
  no alpha and the generated 120px icon. Device PNG checks handle CgBI before IHDR.
- Transcript tail placement follows completed layout, viewport and insets, then yields
  to reader scrolling. Waiting for every lazy markdown row could wait forever on offscreen
  work; scrolling inside geometry callbacks used unfinished layout. Further inset repair: Sep 14.
- Profile restart fixtures wait up to five seconds to rebind the same TCP address without
  holding their registry lock; occupied ports fail rather than silently changing address.
  Workspace test deadline returned to 900 seconds after 150 seconds killed healthy hosted runs.

## 2026-09-09

- Configuration owns `cloud_url` (default `https://amux.sh`); attaching a relay supplies
  route/credentials only. Overwriting cloud identity with relay address broke QR pairing.
  QR and printed code now use the same authenticated account route; neither redirects configuration.
- Unpair closes trusted streams/tunnels/direct links but retains the relay's reachability
  claim and opens no trust-replacement window. Deleting that claim made re-pairing impossible
  until a still-connected host happened to reconnect to the relay.
- The production phone→cloud→relay→Claude path passed sign-in, both pairing methods,
  prompt/reply and restored launch. Refresh tokens are imported once, then rotated/restored;
  replaying the original token after rotation caused `invalid_grant`.
- Entitlement gates on account-service `me { access { pro } }`, including non-purchase
  grants; billing records, token tiers and phone-clock expiry are not alternate authorities.
  Production GraphQL needed bearer authentication as well as browser cookies (`me: null` before fix).
- StoreKit transactions are posted to the authenticated account service before finishing.
  Refused/unsubmitted purchases remain retryable; accepted purchases awaiting entitlement
  show “still switching on” and retry the access read without reposting or selling twice.
  A real App Store sandbox purchase still requires an attended physical-phone check.
- Native bundle identity is `sh.amux.app`, continuing the existing App Store listing
  and its products. Simulator builds sign ad-hoc with their own Keychain group;
  linker-only signing caused refresh-token storage failure `-34018`.
- The app owns startup/restoration, per-account token requests and cached fleets outside
  debug tooling. Fatal startup releases the dead bridge, retains cache/diagnostics and
  retries with fresh credentials; ordinary transport failures retry through the live worker.
- Mobile installation relocation rebases only validated paths in each old allocated
  namespace. Keys, profile IDs, trust and cache survive container moves. Desktop relocation
  stays refused by default; cross-profile paths, external paths and symlinks fail.
- Performance runs measure optimized production pages with controls present, require
  fresh artifacts, and validate the actual echo frame. Lifecycle checks require a new
  confirmation count after foregrounding, not the sticky old `reconciled` flag.
- A performance test bundle's direct Swift-package dependencies linked duplicate shipping
  bridges and silently lost plaintext-network measurements. Taking types from the host
  keeps one bridge; network observations now come from the live relay.
- Simulator cold-start budgets distinguish framework loading from app work: linking
  StoreKit/AuthenticationServices explained the roughly 310→440 ms shift. Simulator
  median/worst limits became 460/600 ms; physical-phone claims require separate measurements.
- Report upload retries retain identical bundle bytes/identity; sent is terminal and
  late replies cannot overwrite a newer capture. Reports record the built Git revision.
  Report tooling/resources are absent from Release, checked at symbols and bundle scope.

## 2026-09-06–08

- The phone keeps one isolated profile/device identity, trust store and fleet per account.
  Inactive accounts fold their own attention; switches reject prior-generation results.
  The phone's own host is excluded from machine lists; fixtures must give it a distinct ID.
- Backgrounding holds no connection; foregrounding reconnects. Closing a conversation
  releases its stream. Host loss retains the last transcript and remembered attachment;
  return resubscribes without navigation, while confirmed deletion releases the attachment.
- Operation results belong only to the conversation that dispatched their IDs. Only the
  newest dispatch may supply its visible refusal; earlier and cross-agent failures had
  appeared beneath unrelated in-flight messages. Result retention is bounded.
- iPhone drafts use atomic inline tokens in a native text editor; commands are first and
  unique, Return inserts a newline, and Send is explicit. One queued message per agent can
  be replaced or restored for editing. Failed sends retain the editable draft and attachments.
- Native diff review uses one frozen, file-addressed document with both line number sets
  and repository/artifact identity. Comments return as one canonical review token;
  file ordering and line anchors stay stable while the working tree changes.
- Golden captures use the simulator display; report captures remain in-process. Capture
  clocks, caret/motion and locale are pinned. Screenshot notifications arrive after the
  system image, so an app report frame has no guaranteed timing equivalence to that image.
- Recorded-provider cleanup drains trailing output under backpressure before shutdown.
  Test startup scheduling has a separate budget from the behavior being tested; existing
  shell/in-memory fixtures avoid macOS assessment delays on fresh temporary executables.

## 2026-09-04–05

- Installations own independent account profiles behind a separate administrative front
  door; profile client sockets and peer tunnels do not expose administration. UUID paths,
  strict config shapes, root locking and atomic registry writes enforce storage isolation.
- Account binding validates userinfo and stages rotating credentials atomically; failed
  staging preserves accepted credentials. Logout rejects late refreshes. Lifecycle mutations
  outlive caller cancellation; watches begin with a snapshot and report terminal overflow.
- Deletion records durable intent, closes admission and drains artifact work before teardown.
  Slow provider input releases its guard before delivery so it cannot block lifecycle.
  Suspend snapshots every profile before stopping agents; a journal retains partial recovery.
- Shutdown closes the owned listener before acknowledging, drains accepted replies and
  preserves replacement sockets. Explicit unlock avoids fork-inherited descriptors holding
  installation locks. Windows skips Unix directory syncing and preserves failed replacements.
- Claude SDK gained native streaming chat, all typed asks, queued prompts and controls.
  Accepted prompts publish identifiable rows for echoes/replay; failed writes do not.
  SDK working state is authoritative and does not use the PTY inference timeout.
- SDK context counts the latest parent call, resets after compaction/clear, and stays
  unknown without evidence. Codex context likewise uses per-turn usage, not cumulative
  spend. SDK task rows reconcile by tool-use identity instead of duplicating subagents.
- Codex 0.153.4 naming no longer materialized a rollout. Fresh agents now complete a
  history-inclusive resume of the same thread and adopt its event registration before
  readiness; failures retry that ID. This supersedes the August naming-only workaround.
- Managed SDK launches leave user hooks to Claude settings. Live 2.1.261 evidence covers
  Stop hooks and typed asks; a real user-dialog frame remained unobserved, not qualified.

## 2026-09-02–03

- Attachments use SHA-256 identities, per-agent ownership and replayable metadata before
  provider delivery. Validate the full pin list before changing lifetime; unsent artifacts
  expire, sent artifacts stay pinned, and remote viewers verify bytes in a bounded cache.
- Image/file/paste/review elements share one lossless grammar; malformed mentions remain
  prose. Reviews freeze Git base/HEAD/blob identity and path/side/line anchors; UTF-8 byte
  counts frame comment bodies. Untracked-file diffs do not modify the real Git index.
- Composer tokens occupy one character; failed sends restore tokens rather than exported
  markup. Kill/yank must retain valid token payloads after send. macOS image opening uses
  known image type because extensionless artifact paths were classified incorrectly.
- Reports are private, versioned directories under configured reports storage, declaring
  present/absent parts. Bounded model/renderer traces replay through the live mutation path;
  clocks, notices and quit expiry are recorded. Paint caches are rebuilt, not serialized.
- Release excludes interactive trace/replay capture and per-frame buffer copies. Explicit
  report-path replay needs no installation. Graduated fixtures redact configured hostnames
  and provider ownership identifiers while preserving replay and source identity.

## 2026-08-29–31

- Canonical `claude`/`codex` sessions expose one owned event stream plus a control handle;
  `pty-host` owns process groups and bounded termination. Agent kinds/driver/protocols are
  typed and exhaustive; provider vocabulary stays native through client folds.
- Strict multi-transport replay accounts for every read/write and preserves causal order.
  Corpora retain capture versions, content hashes and later verification ledgers; scripted
  data cannot claim live provenance, and new tool membership cannot rewrite old captures.
- Claude PTY clients send semantic intents, not key bytes. Provider-owned bounded TOML
  keymaps resolve against version evidence; unsafe menu extrapolation refuses input.
  User overrides cannot inherit capture verification. CRLF-aware ledger removal fixed Windows corruption.
- Unified diffs carry independent old/new coordinates; ask-time snippets never invent
  absolute positions. Landed Claude previews retain at most 16 hunks, 64 rows and 8 KiB;
  valid hunks survive unknown trailers, and truncation must leave visible change evidence.
- TUI chats share frame geometry, viewport, block painters and cached hit-testing while
  retaining native content. Warm composer/wheel changes repaint no feed blocks; 1,000-row
  120×40 release frames measured about 1.3/1.8 ms for Claude/Codex against an 8 ms budget.
- Protobuf generation moved out of build scripts into committed source to prevent shared
  build-output contamination. Byte-contract recordings, goldens and generated output use LF.

## 2026-08-23–25

- Agent messages carry daemon-authenticated sender/envelope identity; recipient records
  distinguish message, completion and exit. Families rank by the loudest descendant need;
  a parent's docked answer addresses the child's native ask, never a copied parent obligation.
- Spawn delivers its initial prompt within bounded readiness or removes the child.
  Children inherit provider permission policy and directory. Model-facing stop is limited
  to direct children; human cascades report unreachable descendants. CLI active-work cascades
  require `--force`; interactive deletion shows the full subtree for confirmation.
- Claude inbox delivery confirms synchronously on `queue-operation enqueue` within two
  seconds. Waiting for the later turn-consumption row caused duplicate PTY resends while
  busy. Timeout retires the socket; stream closure falls back once; process restart clears the latch.
- Managed Claude version discovery is bounded/cached and refreshed by managed transcript
  evidence, never external sessions. Child environments scrub inherited messaging secrets;
  PTY fallback sanitizes controls. External sessions remain unable to receive writes.
- Managed Claude/Codex tools use session-scoped stdio MCP with the daemon's frozen exact
  executable/config/socket/identity route. Each call reconnects, but interrupted mutations
  are not retried. The global Claude plugin and Codex dynamic-tool route were retired.
- Reusable `pair --demo` PINs support unattended app review as an explicit mode;
  ordinary pairing remains one-shot, five-minute and five-attempt.

## 2026-08-09–18

- UI became a serializable message reducer with effects and bounded replay. Each provider
  classifies phase, attention and write gates together; read-only is orthogonal. Replay
  reports unknown attention; interrupt remains available during an in-flight Codex steer.
- Invariant violations became loud but nonfatal by default in every build: error log,
  bounded dump attempt and sticky UI warning. Unknown agents remain visible but unreadable.
- Claude transcript loss came from inherited child-session environment markers; managed
  launches scrub them. Duplicate hook registrations are deduplicated. Only the head PTY
  ask may be answered; bounded replay must unlock even when its readiness marker was evicted.
- Codex agents are persistent threads on a supervised shared app-server. Raw terminals
  are lazy `codex resume` processes against that same server; final detach tears them down
  after measured idle cost of 147.7 MiB. PTY preparation runs outside the agent-registry lock.
- Terminal restoration covers panic and catchable signals. Bare `amux` prints help off-TTY;
  interactive creation checks the terminal before creating an agent. Detach returns to shell,
  switch returns to fleet; guarded Ctrl+C preserves drafts through a yankable clear.

## 2026-06 and earlier

- June protocol rewrite replaced route stacks with host-ID routing and one proxy hop:
  advertise only adjacency and forward only to adjacency. Every peer call uses an explicit
  open/data/close tunnel with pinned end-to-end mTLS; forwarding grants no call authority.
- PIN and QR share SPAKE2 with different out-of-band secrets. QR carries host/cloud/secret;
  SSH exchanges trust separately. Reauth is fire-and-forget; link closure ends failed refresh.
  Wire version reset to 1 under equality matching; no historical compatibility was retained.
- Embedded clients separated local-agent hosting from transport/runtime ownership.
  Binary self-update polling belongs to desktop daemons, not embedded mobile clients.
  Windows ConPTY teardown hangs required explicit platform exclusions in affected tests.
- January–May established PTY multiplexing, remote subscriptions, persisted suspend/resume,
  hooks/structured sequence numbers, multi-tenant cloud relay and the embedded library.
  Early WebSocket/MessagePack and route-stack designs were superseded by the June protocol.
