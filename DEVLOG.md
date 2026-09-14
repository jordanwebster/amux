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
