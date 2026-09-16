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
