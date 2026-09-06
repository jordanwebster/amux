# The amux system architecture

**Status**: current (2026-09-05). This document describes the system —
processes, servers, trust machinery, service surfaces, and internal
layering. Its companion, [`PROTOCOL.md`](./PROTOCOL.md), owns the wire:
links, frames, tunnels, the routing rules, and the pairing flow. When this
document and the code disagree, the code and the spec suite
(`crates/amux/tests/spec/`) win.

## Processes and deployment shapes

`amux server start` runs an **installation**: one daemon process supervising
any number of **profiles**. A profile is a complete amux device, with its own
key, `host_id`, trust store, agents, artifacts, routing, tunnels, local socket
and at most one cloud link. `Installation` (`installation/`) owns the registry,
exclusive root lock and profile lifecycle; each `ProfileRuntime` (`profile/`)
owns that device's services. Startup attempts every profile independently and
reports a failed profile while the others keep serving. Dropping clients does
not stop their profiles.

Profiles isolate amux identity, trust, state and routing; they do not sandbox
code running as the same OS user. Agent processes retain that user's filesystem
and process access. Desktop profiles share one daemon process, so a process
crash or binary replacement affects the whole installation.

An **embedded installation** runs the same supervisor inside its host app.
The host retains one `Installation` across screens and obtains a `Client` for
each profile it displays. The [embedding entry point](#embedding-an-installation)
below describes storage, credentials and lifecycle calls. Without the
`local-agents` feature, local listeners and process hosting are compiled out.

`amux server start --cloud` instead runs a **cloud relay**
(`ServerMode::CloudRelay` in `server.rs`). It mints a throwaway `host_id`, loads
no device identity, and starts only the JWT-gated `LinkService`. Its per-user
routing instances are forwarding infrastructure, not device profiles.

Around the daemon sit its clients and consumers:

- **CLI** (`crates/amux-cli`): discovers and administers profiles through the
  installation's front door, then uses the selected profile's `ClientService`
  for agent operations. Hidden subcommands are protocol plumbing:
  `amux relay --profile <UUID>` bridges stdin/stdout to that profile's socket
  (the receiving end of an SSH link),
  `amux pair-recv` runs the responder side of an SSH pairing identity
  exchange, and `amux mcp agent` serves the agent tools over stdio MCP.
  [`A2A.md`](./A2A.md) owns that tool contract.
- **UI runtime** (`crates/amux-ui`): a reactive client library over the
  same `ClientService` surface, for embedding in apps. It joins attachment
  puts before a send, folds stream refs, fetches opened artifacts through the
  viewing-profile cache, and leaves presentation to its client.
- **Artifact library** (`crates/amux-artifacts`): dependency-light
  content-addressed storage with an authoritative per-agent Owner role and a
  disposable per-viewing-profile Cache role. It depends on neither the daemon
  nor the UI, so another client can reuse the storage contract directly.
- **Test harnesses**: debug builds compile an in-process harness
  (`amux::testnet`) that builds production profile runtimes and installations —
  real identities, real trust stores, real localhost TCP with device mTLS, an
  optional in-process cloud relay — for the spec suite, plus `WirePeer`, a scripted
  protocol actor for wire-conformance tests. `crates/e2e-runner` drives
  real compiled binaries end to end.

## Accounts, configuration and local entry points

A profile can remain unbound for local, LAN or SSH use, or bind to one cloud
account. An account is identified by cloud service and subject and can bind at
most one profile in an installation. On desktop, the daemon validates staged
login credentials and obtains the account name and email from userinfo; only
the daemon refreshes credentials. Embedded hosts supply their own credential
providers as described below. A local rename overrides the account label.
Logging into another account cannot rebind an existing profile. First login
can adopt a pristine unbound profile silently; retained trust, agents or
artifacts require confirmation before adoption.

Every eligible bound profile maintains its cloud link regardless of which
profile a client views. Logout removes its credential and cloud link but
preserves its account reservation, key, trust and agents. Logging back into
the same account reconnects the same device. Pause retains the credential and
survives restart; resume reconnects it. Both leave local and direct peer
operation intact. Explicit, confirmed deletion destroys the profile's data
and closes its clients. `amux server stop` stops the installation and kills
its agents; startup does not automatically resume suspended agents. Internal
installation suspend/resume operations support binary updates.

The installation configuration defaults to `$XDG_CONFIG_HOME/amux/config.yaml`
(fallback `~/.config/amux/config.yaml`). It owns `root`, `front_door_socket`,
device name, keep-awake, keybindings, UI preferences, keymaps directory, update
manifest URL and an optional shared reports directory. The root defaults to
`$XDG_DATA_HOME/amux` (fallback `~/.local/share/amux`). A UUID, independent of
the account or label, names each profile's directory and socket:

| Path beneath the installation root | Owner and contents |
|---|---|
| `registry.yaml`, `lock` | Installation registry and exclusive ownership |
| `state/last-profile` | Client-side last-used UUID; never a server-wide selection |
| `profiles/<UUID>/config.yaml` | Profile paths, cloud URL, optional LAN `tcp_port`, absolute `installation_config` reference |
| `profiles/<UUID>/credentials.yaml` | Profile credential, mode `600` |
| `profiles/<UUID>/data/` | Identity, trust, agents, artifact cache and default reports |
| `profiles/<UUID>/state/state.yaml` | Profile runtime state |
| `profiles/<UUID>.sock` | Profile's plain gRPC socket, mode `600` |

`AMUX_CONFIG` (or `--config`) names a profile configuration, which explicitly
locates its installation. Unknown config fields, a missing installation file
or disagreement with the profile's allocated paths are errors. Cloud
connectivity follows binding, credentials and pause intent; it is not a
configuration mode. Worktree tooling generates both configuration files and
probes the installation with `amux profiles`. Existing single-device layouts
are not migrated: re-initialise and pair devices again for each account.

The **front door** is `InstallationConfig.front_door_socket`, normally
`amux.sock` in the per-user runtime directory. It serves only `ProfileService`
and `InstallationService`, as plain gRPC with owner-only socket permissions.
A third-party client uses the following contract:

1. Connect to the front door and call `ProfileService.ListProfiles` or
   `WatchProfiles` to discover UUIDs, labels, account email, intent, observed
   connection status, availability and socket paths.
2. Choose an available profile and open a second plain gRPC connection to its
   returned `socket_path`; use `ClientService` there to list or operate agents.
3. Keep that connection bound to the chosen profile. Another client's selection
   cannot retarget it. A deleted profile closes its clients; rediscover before
   connecting again.

There is no selection preface or client-readable registry contract. Watch
starts with a snapshot and `SnapshotComplete`, then ordered changes; a lagged
stream ends with `ABORTED` and the client resubscribes for a fresh snapshot.
Mutating administration requests carry operation UUIDs for retry deduplication
in a bounded in-process ledger; use the same UUID when retrying the same request,
and a new UUID for a new operation. Rename and delete also carry the revision
the caller observed.

`amux init` creates the installation and an unbound profile, asking about
keep-awake. `amux profiles` lists profiles; `--profile <name|UUID>` selects one
for commands such as `amux --profile Work list` or
`amux --profile Work profile pause`. Successful client selections remember the
UUID, so renaming does not change the default device. An explicit unknown or
ambiguous selector fails instead of falling back. Managed agent hooks and MCP
servers use their launching profile's exact route regardless of ambient
configuration or the last-used selection.

In the TUI fleet, `<leader> p` opens the profile switcher. Selecting a profile
replaces the UI runtime with an empty model connected to that profile; results
from the previous runtime are discarded. Reports and the artifact cache follow
the new selection. The other profiles keep running and connected.

## Embedding an installation

The host entry point is `Installation::open(InstallationOptions)`. Open one
durable, host-owned root with `InstallationRoot::OnDisk`, for example a directory
in the app's data container. Reopen that same root after relaunch to retain
profile UUIDs, device identity, trust and account bindings. Only one installation
can own the root at a time. Build `amux` with `default-features = false` when the
app must not host local agent processes.

With a host-supplied `root: PathBuf`, `settings: InstallationSettings` and
`providers: Arc<dyn Fn(ProfileId) -> Arc<dyn CredentialProvider> + Send + Sync>`,
opening looks like this (the installation and credential types are exported by
`amux`; `PathBuf` and `Arc` come from `std`):

```rust
let installation = Installation::open(InstallationOptions {
    root: InstallationRoot::OnDisk(root),
    settings,
    listeners: Listeners::InProcessOnly,
    credentials: CredentialSource::HostProvided(providers),
    identity_http: reqwest::Client::new(),
}).await?;
```

`Listeners::InProcessOnly` exposes no local Unix or LAN listeners; profiles have
no advertised socket path. Use `installation.profiles()` for the directory and
`installation.watch()` for its initial snapshot and later changes. On
`ProfileEvent::Lagged`, subscribe again. Check each profile's `available`,
`observed` and `startup_error` rather than assuming that opening the installation
made every profile ready.

Create with `installation.create(OperationId::new(), label).await?`, retain the
returned `record.id`, and register its host credential provider before calling
`installation.bind(operation_id, request).await?`. A `BindRequest` names
`BindTarget::Explicit(id)`, `cloud_url`, `staged_refresh_token` and
`adopt_non_pristine`. Binding validates the login with the identity server and
refuses an account different from the profile's existing binding. Ask the user
before setting `adopt_non_pristine` for a profile that already holds trust,
agents or artifacts.

`CredentialSource::HostProvided` leaves secret storage and ongoing refresh to
the host. Its factory must be ready for existing bound profiles when `open`
starts them. Implement `CredentialProvider::access_token` and `invalidate` per
profile; amux checks the returned token's userinfo subject against the binding.
The initial bind still consumes its staged refresh token, so that token must
be separate from the refresh-token chain the host provider owns. The bind
result does not return a rotated token to the host. Host-provided credentials
are not written to profile credential files; the host must retain its own
credentials across relaunch and handle refresh rotation safely on cancellation.

Obtain a profile's agent API with `installation.client(id)?` and its
`ProfileAdmin` with `installation.admin(id).await?`. Use the admin handle for
`start_pin_pairing`, `start_qr_pairing`, `pair_pin_cloud_peer`,
`pair_qr_cloud_peer`, `list_peers` and `unpair`; pairing and trust administration
are separate from the screen's `Client`. Keep the installation alive while
replacing screen clients: dropping the last client stops no profile or cloud
connector. Lifecycle calls (`logout`, `pause`, `resume`, `rename`, `delete`)
also belong to the installation. Rename and confirmed deletion take the
profile revision the host last observed. Logout keeps the device for later
login; deletion removes its keys, trust and agents. Clear host-owned secrets
in the host's storage as part of signing out.

Await `installation.host_suspend()` when the host becomes inactive to tear
down every cloud link while retaining identities, trust and local API access.
Await `installation.host_resume()` on return to request fresh credentials and
reconnect eligible profiles independently; rejected credentials in one account
leave other accounts connected. Recreate remote subscriptions after resumption;
old cloud streams do not survive. These hooks are separate from agent suspension
for desktop updates. Finally, await
`installation.shutdown(ShutdownReason::UserRequested)` before stopping the
async runtime or reopening the root, so transport and runtime teardown finish.

The library integration test exercises two accounts through the real relay with
local agents compiled out. The phone app's adoption of this API and account
switching is separate work; a suspended phone app receives no background alerts.

## Provider crates and daemon adapters

Provider process behavior lives below the daemon in canonical crates owned by
this repository. `claude::pty::Session`, `claude::sdk::Session`, and
`codex::Session` each expose the same boundary shape: one owned, ordered event
stream paired with a cloneable control handle. The crates own provider launch,
native transport parsing, and provider facts; no callback trait crosses into
amux. [`PROVIDER_CRATES.md`](./PROVIDER_CRATES.md) owns the complete boundary
and test story.

The layering is deliberate:

1. **`pty-host`** owns provider-neutral PTY spawn, the single output stream,
   input and resize handles, exit monitoring, and process-group termination.
   Claude PTY sessions, the Codex raw plane, and the test agent all use it.
2. **`claude` and `codex`** own provider sessions. Claude's PTY driver combines
   a PTY, hook stream, transcript stream, and observed version in one source
   bundle; its SDK driver owns the stream-JSON event/control boundary. Codex
   owns one app-server thread event/control boundary.
3. **`crates/amux/src/agents/claude` and `agents/codex`** are adapters. They
   translate provider events into amux-owned structured rows, route typed input
   to controls, supply the A2A carrier, and persist only the provider identity
   needed for resume.
4. **The daemon** owns agent identity, protocol exposure, sequencing, replay,
   fan-out, outstanding obligations, delivery policy, suspend records, and UI
   layers.

`AgentBackend` is the common daemon seam, but the provider sessions behind it
remain intentionally different. Claude PTY is an interactive process owned by
amux; Claude SDK is a stream-JSON process and never tails a transcript; Codex
is a thread on a shared, supervised app-server. Codex additionally exposes a
raw PTY running `codex resume`, so the genuine Codex TUI and amux's native chat
can be live on one agent at once. Durable Claude session ids and Codex thread
ids make suspend/resume survive daemon restarts. [`CODEX.md`](./CODEX.md) owns
the Codex detail, and [`CHAT.md`](./CHAT.md) owns Claude PTY chat behavior.

Raw subscription lookup is a two-phase boundary. While the local-agent
registry is read-locked, the service validates the requested protocol and
clones only an owned, agent-specific raw target. It releases that guard before
any Codex socket connect, PTY open, or process spawn. The target is a snapshot:
if lookup wins a race with deletion, it remains bound to that exact old session
and can never redirect to a replacement registered under the same id. Codex
also rechecks that the snapshotted session is not stopped and that its
thread/socket endpoint is still current before publishing the prepared PTY;
otherwise it terminates the unpublished process and refuses the subscription.
A Codex-session preparation mutex preserves one-spawn fanout, while the Codex
runtime mutex is held only to snapshot or publish cache state.

Each desktop profile owns a **local Unix socket** (`ProfileConfig.socket_path`,
mode `600`) for its clients. Its **external TCP listener** (`tcp_port`) is off
by default; LAN-direct reachability requires a separate opt-in port for each
profile and feeds that profile's dispatcher. Outbound, each profile dials its
cloud service (TCP + WebPKI TLS + JWT) when eligible and re-dials the direct
reachabilities in its own trust store.

## Identity and the trust store

Each profile generates an Ed25519 keypair and a random 128-bit `host_id`,
persisted in its own data directory (`ProfileConfig.data_dir`, allocated at
`<root>/profiles/<UUID>/data`):

- `device.key` — the private key, PKCS#8 v1 DER, mode `600`. It never
  leaves the device.
- `host_id` — 16 raw bytes, mode `600`. Independent of the key, so a
  future key rotation can preserve the device's stable identifier.
- `trust.json` — the trust store, mode `600`.

All three are written atomically (write-temp-then-rename). The profile's
non-secret runtime state lives separately at its `state_path`.
Pairing windows and pinned keys belong to this profile alone. Two devices
using two accounts pair once per account; a key pinned by one profile grants
no authority in another profile on the same installation.

The trust store (`trust.rs`) maps
`host_id → { pubkey, name, paired_at, reachabilities }`. It is the entire
trust model: a pinned pubkey is what lets a peer's mTLS handshake
terminate into the trusted services. Entries are added only by successful
pairing and removed by local revocation (`amux unpair`) or profile deletion.
Outside an authorized pairing exchange, inbound protocol messages cannot mutate
trust. The store is local-only — never sent
to the cloud, never synchronized between devices.

`reachabilities` is not trust; it is the list of **dialer-responsibility
markers** this device learned as an initiator: `Cloud`, `DirectTcp { addr }`
(the listener address it dialed), or `Ssh { target, profile }` (the SSH
destination and remote profile UUID). SSH pairing exchanges that UUID alongside
the device identity, so reconnecting runs `amux relay --profile <UUID>` even
after a remote rename or default-selection change. Re-establishment is always
the dialer's job: on startup the `ReachabilityLinkConnector`
(`services/reachability.rs`) walks the
store and dials every `DirectTcp`/`Ssh` entry; `Cloud` entries need no
action because the cloud connector brings up that link separately. The
acceptor side of a pairing records no reachability it didn't dial — an
accepted socket's source port is not a reusable address. A trusted peer
with an empty list is a peer we trust but have no stored way to reach;
it shows up offline until it dials us.

## Servers and connection admission

Each profile runs two long-lived tonic servers
(`services/startup/mod.rs`), each fed an mpsc stream of accepted
connections. The installation serves administration separately:

| Server | Hosts | Fed by |
|---|---|---|
| **Installation front door** | `ProfileService`, `InstallationService` | Installation Unix socket; in-process owner channels |
| **Trusted Server** | `ClientService`, `AgentService`, `LinkService` | Local Unix socket; pinned-mTLS streams from the dispatcher |
| **Pairing Server** | `PairingService` | Anonymous-TLS streams from the dispatcher, only while a pairing window is open |

The front door is separate from both profile servers. The split exists so
that authorization is decided **once, at connection admission**, in one place.
A connection lands on a server with a fixed
service set; the services it cannot reach do not exist on its connection,
so an unauthenticated peer cannot even probe for them. Per-RPC
interceptors were considered and rejected (auth scattered across N
services), as was a server-per-connection (needless lifecycle churn).

## The dispatcher

`dispatcher.rs` is the single admission point for every inbound stream
that needs a TLS handshake. Two sources feed it: sockets accepted on the
external TCP listener, and inbound tunnels that terminate at this daemon
(the `TunnelPool` hands each one over as a byte stream). Both get the
same treatment — the dispatcher always presents the device's self-signed
certificate and *requests* a client certificate:

| Handshake outcome | Authority granted |
|---|---|
| Client cert pinned in the trust store | Trusted Server, bound to that peer's `host_id` |
| Client cert not pinned | Rejected during the handshake (`PinnedClientCertVerifier`) |
| No client cert, pairing window open | Pairing Server (no trust-side authority) |
| No client cert, no pairing window | Closed |

The `host_id` bound at the handshake is load-bearing: a `Hello` whose
`host_id` contradicts the mTLS-bound identity is rejected, so a paired
peer cannot impersonate another peer at the link layer, and tunnel-borne
calls carry the authenticated peer identity into the services.

The profile's local Unix socket bypasses the dispatcher entirely: arrivals there
are classified `LocalTrusted` and feed the Trusted Server directly, with
OS file permissions as the gate. SSH is deliberately **local-equivalent**:
`amux relay --profile <UUID>` bridges the SSH stream into that profile's socket,
so anyone who can SSH into the daemon's account already has what the socket
grants — that is the existing OS trust boundary, not a new one. Peer *calls* still
authenticate uniformly: every call rides a tunnel, and every tunnel runs
a pinned mTLS handshake at its terminating dispatcher, whatever transport
its frames crossed (an SSH link confers no call authority by itself).

The external listener defends itself: TLS handshakes are rate-limited
per source IP (10/minute, sliding window), capped at 128 concurrent, and
timed out after 10 seconds (`resource_limits.rs`, `dispatcher.rs`).

## Service surface map

| Service | Where it lives | Who may call it |
|---|---|---|
| `ProfileService` | Installation front door | Local installation owners over the front-door socket / in-process |
| `InstallationService` | Installation front door | Local installation owners over the front-door socket / in-process |
| `ClientService` | Profile's Trusted Server | Local clients over its Unix socket / in-process; paired peers over tunnels, with the same API |
| `AgentService` | Trusted Server | Local clients and paired peers (this is what remote sessions ride) |
| `LinkService.Connect` | Trusted Server, and the cloud relay | Adjacent nodes establishing a link, over any link transport |
| `PairingService.Pair` | Pairing Server | Anonymous-TLS callers during an open pairing window |

`ProfileService` lists and watches profiles, manages their lifecycle and
binding, and carries all pairing-window and trust administration, including
peer inspection, revocation, two-phase pairing confirmation, device identity,
pairing candidates and profile diagnostics. Each
profile-specific request names its UUID. `InstallationService` provides
installation info and diagnostics, shutdown, and suspend/resume across profiles
for update. Neither service is registered on profile sockets, LAN connections
or tunnels: a paired peer cannot shut down, suspend or resume the installation
or administer trust.

`ClientService` is the client API: host and agent inventory and subscriptions,
agent CRUD, message delivery and work status, session attach/input, repository enumeration, artifact
put/get and diff, hooks and debug. Pairing grants authority to operate agents,
including creating and deleting them. It grants no installation administration.

Host inventory contains the profile's local host and trusted peers, identically
for local and remote callers. Untrusted-but-online cloud hosts appear only in
`ProfileService.ListPairingCandidates` on the front door. Each `HostEntry` carries
`online` (routing-derived presence) and `last_dial_error` (the most
recent failed dial, cleared when a route comes up); nothing probes, so
"unknown" is simply `!online` with no recorded error.

`AgentService` is what tunnels exist for: a peer lists another daemon's
agents, creates or deletes them, delivers daemon-authored message envelopes,
updates work status, attaches to a session, round-trips terminal I/O, and
serves repository discovery, artifact put/get and diff requests on the owning
host — through the cloud relay if that is the only shared path, with the relay
seeing ciphertext. Parent edges and the provider-specific carriers are
described in [`A2A.md`](./A2A.md).

## Project discovery

`Client::list_repositories` uses the `ListRepositories` RPC on both services.
It selects a host by identity and sends an optional
case-insensitive path/name query and a total result limit. `ClientService` routes
it to that host's `AgentService` over the same authenticated direct connection or
relay tunnel used for agent operations. The host owns the search roots; callers
cannot supply a directory to scan.

Configure `repository_roots` as a list of directories in the installation's YAML
config; all profiles inherit those roots, while recent projects remain per profile.
The default is empty. The host canonicalizes and deduplicates existing roots,
searches directories in path order, recognizes both `.git` directories and
worktree `.git` files, and stops descending at each repository. Directory
symlinks are not followed; a symlink deliberately configured as a root resolves
to its canonical directory. Missing or unreadable directories are skipped.
Only paths representable by the text protocol are offered.

The response separates recent projects, repositories and canonical roots.
Recent projects come first, newest creation first, with each directory appearing
once across both lists. The requested limit caps their combined count, up to
200; zero returns only roots. Each project has a path, display name and optional
last-use time (the newest successful agent registration in that directory).
Recent directories may be outside search roots: creating an agent with a typed
path adds that project to the history. This does not restrict which typed paths
agent creation accepts.

The host retains its latest 200 recent directories in
`data_dir/recent-projects.json`, written atomically with private permissions.
Deleting agents and restarting the daemon preserve history; directories that no
longer exist are omitted from results. Resuming an older session cannot move a
project's timestamp backward. A history write failure is logged without failing
an otherwise successful agent creation.

## Attachment storage and routing

An agent's daemon is the sole owner of that agent's artifacts. It opens one
`amux_artifacts::Owner` at
`<data_dir>/agents/<agent-id>/artifacts`, loads the index once, and keeps it in
memory. Content starts ephemeral, is pinned when a sent message explicitly
names its id, is swept after one hour if still ephemeral, and is deleted with
the agent if pinned. A five-minute background pass visits loaded owners only.

`PutArtifact`, `GetArtifact`, and `Diff` exist on both trusted services.
`ClientService` resolves the agent and forwards a remote call through the
ordinary tunnel to `AgentService`; artifact bytes never pass through a session
subscription. `SendInput` carries only a pin list. After validating and pinning
that list, the owning daemon writes an `amux.attachments` metadata row before
the provider input; it replays all pinned refs when a session subscription
opens. Diff computation also happens there, in the agent's working directory,
and stores the returned patch as a Diff artifact.

Every viewing profile uses one `amux_artifacts::Cache` shared across its agents.
It fetches through `GetArtifact`, verifies content identities, persists recency,
and uses only byte-bounded LRU eviction. Its root is
`<data_dir>/cache/artifacts`; the shared `ui.artifact_cache_mib` preference sets
the bound and defaults to 256. [`ATTACHMENTS.md`](./ATTACHMENTS.md) owns the element syntax,
provider materialisation, complete lifetime rules, and deferred attachment
surfaces.

## The cloud deployment

The cloud relay is multi-tenant and minimal. It serves exactly one thing:
`LinkService.Connect` behind a JWT interceptor
(`LinkAuthInterceptor` → `JwtCloudLinkAuthenticator`), on a TCP listener
wrapped in ordinary WebPKI server-auth TLS (certificate and key supplied
via `AMUX_TLS_CERT` / `AMUX_TLS_KEY`). Devices validate the relay's
hostname certificate like any public endpoint and authenticate themselves
with a JWT from the OAuth device flow; the fire-and-forget `Reauth`
refresh keeps a healthy link undisturbed across token expiry.

Tenancy is per-user by construction: `CloudLinkService` holds one
routing-services instance (`RoutingCore` + `TunnelPool` +
`ConnectionManager`) per authenticated `user_id`, created on first link
and shared by that user's devices. Adjacency events fan out only within a
user's instance, so presence is scoped per user, and frames are only ever
forwarded between one user's devices. Two devices logged into different
cloud users cannot see or reach each other through the relay — they can
still pair and connect via LAN or SSH, which never involve the cloud.

What the cloud structurally cannot do follows from what it doesn't have.
It holds no device identity and is pinned in nobody's trust store, so it
can never terminate a tunnel into anyone's trusted services: it cannot
create agents, read session traffic, or impersonate a device. Its own
incoming-tunnel sink discards everything
(`spawn_discard_incoming_tunnels_task`) — there is no service behind the
relay to tunnel into. Pairing traffic crossing the relay is opaque
ciphertext like everything else; the cloud is never told pairing is
happening. What it *does* see is metadata: `host_id`s, JWT-derived user
ids, names and capabilities from handshakes it relays, online status, and
traffic volume/timing.

A device daemon bounds the blast radius of a compromised relay. Inbound
tunnel opens arriving over the cloud link are rate-limited (30/minute,
sliding window — excess frames are dropped while the link stays up), and
the routing table caps untrusted hosts at 1000 with oldest-inactive
eviction; trusted peers are exempt and never evicted
(`resource_limits.rs`, `routing/core.rs`). A self-hosted relay needs none
of this machinery explained separately: relaying is something every node
can do, so an always-on paired peer is a relay with a pinned key.

## Internal layering

Each profile owns four networking components under its services, each with
one job. No profile shares their link, route or tunnel state with another:

**`LinkRegistry`** (`routing/link_registry.rs`) — the daemon's live links:
`LinkId → writer` for every established link, each writer feeding frames
into that link's `Connect` stream. It is the single source of truth for
*wire* adjacency, and the adjacency-only advertising rule is structural
here: every `NeighborUp`/`NeighborDown` a peer ever receives from us is
emitted by the registry, under one lock, in registration order — there is
no API for broadcasting anything else. Registering a link also reconciles
the handshake's neighbor snapshot atomically, closing the gap between
composing a snapshot and the link going live.

**`RoutingCore`** (`routing/core.rs`) — the routing table: our own direct
adjacency (`directs`) and our neighbors' adjacency claims (`claims`).
Presence falls out as a derivation — a host is online if we are adjacent
to it or some neighbor claims to be — which is why presence reaches
exactly two hops. `best_route` answers `Direct(link)` or `Via(relay)` and
nothing longer. The untrusted-host cap lives here.

**`TunnelPool`** (`tunnel/pool.rs`) — endpoint state for tunnels this
daemon initiates or hosts, plus the relay forwarding rule. Only a
`TunnelOpen` allocates state; data for an unknown id is dropped without
allocation; closes are sent proactively on teardown. Forwarding consults
only the `LinkRegistry` — a frame for `dst` is forwarded iff a direct
link to `dst` exists — and keeps no per-tunnel state for relayed traffic.
Terminating tunnels are surfaced as byte streams to the dispatcher, which
runs the pinned mTLS handshake inside them.

**`ConnectionPool` / `ConnectionManager`** (`connection.rs`) — outbound
channel selection over the two route shapes. There is exactly one
materialization path: every peer call rides a tunnel, opened over a
direct link (`dst = peer`, zero relays) or over a relay link. The pool
caches one tonic channel per `(peer, route)`; the manager subscribes to
routing events, keeps one active route per peer, prefers the link itself
over any relay path, and swaps make-then-break — a cached channel is only
as alive as the link under it, and a broken stream is the caller's signal
to reconnect over whatever is now best. One learned guard: a route whose
target advertises itself as a cloud relay is recorded but never eagerly
tunneled into, because relays discard inbound tunnels and the handshake
could only time out.

The **dispatcher** ties the inbound half together, and `services/startup/`
wires all of it: routing services first, then the two servers, the
listeners, the reachability connector, and (for cloud-attached daemons)
the cloud link connector.

## What is deliberately deferred

Key rotation and identity recovery (today: re-pair from a surviving
peer), finer per-peer or per-method authorization for agent operations,
LAN auto-discovery, and OS-keychain storage for the device key. Pairing
remains the trust boundary for all of them.
