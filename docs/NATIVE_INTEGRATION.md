# Integrating the native app branch

This is the procedure for bringing the `nativeapp` branch (the iPhone app,
its Rust bridge, and the daemon features written alongside it) into `main`,
and the shape the code must have when that is done. It is written for the
person or agent doing the merge, who has not seen the history of either
branch.

## The end state

The app layer is three small crates that any rich client reuses: the iPhone
app today, Mac and Windows desktop apps later, whether they embed a node or
attach to a daemon that is already running.

| Crate | Owns | May depend on | Must not depend on |
| --- | --- | --- | --- |
| `app-runtime` | Account-scoped sessions, the projection from reducer state to typed presentation values, the fleet cache, and the frame-coalesced event queue | `model`, `settings`, `client`, `ui-state`, `ui-runtime`, `artifacts` | `node`, anything provider-specific, any platform SDK |
| `app-embedded` | Starting, credentialing and stopping a provider-free `node::Installation`, holding the relay link, and handing clients to `app-runtime` | `node`, `client`, `app-runtime` | provider crates, test infrastructure (except `testnet` behind the `debug-tools` feature) |
| `app-ffi` | The C ABI: exported symbols, JSON in and out, callbacks, opaque handles, cancellation, foreign lifetime rules; the `staticlib`/`cdylib` crate type; the `cbindgen` build script | `app-runtime`, `app-embedded` | anything else |

The one rule that matters: nothing in `app-runtime` imports `node`. A desktop
app that attaches to a running daemon uses `app-runtime` and `app-ffi`
without `app-embedded`. Presentation values are plain Rust and JSON; no
SwiftUI, UIKit, AppKit or C pointer types appear outside `app-ffi`.

Ownership rules the code must keep:

- Every operation carries its account. A reply cannot be credited to the
  wrong account.
- Sessions, views and subscriptions are independently owned handles. Closing
  a view cancels its own subscriptions and pending work, nothing else.
- Closing every view never stops a daemon the app attached to. Only
  `app-embedded` stops an installation, and only the one it created.
- Rust never talks to the identity service on behalf of a rich client. The
  platform's own account client obtains connect tokens; `app-embedded`
  receives tokens through a callback and connects to the relay it is told
  to use.
- The exported symbol prefix is `amux_app_`. The bridge is not phone-shaped.

The harness the phone is tested against is the `testnet` crate: a declared
topology of real daemons, one fake relay, one fake identity service, and
scripted providers, driven in-process by Rust specs and out-of-process
through `testnet serve`. Nothing test-shaped is compiled into a product
crate under a `cfg`.

## Before you start

`main` must already contain the finished restructure: the `testnet` crate is
the harness (not a private module of `node`); no Cargo feature exists only to
expose test helpers; production files contain no profile-selected harness
branches; tasks run through `just`; CI does not install `wt`. Check with:

```sh
just dependency-policy
just --list
```

## Step 1: merge, do not rebase

On the `nativeapp` branch, merge `main` once. The branch has several hundred
commits; rebasing or cherry-picking is not an option. A dry run before the
restructure finished showed about 75 conflicts, and git follows most files
across the crate moves by rename detection. Resolve in this order, because
each layer's names feed the next: `Cargo.toml` and `Cargo.lock`, then `node`,
then `ui-state` and `ui-runtime`, then `tui`, then `agent-runtime`, then
tests.

Old name to new name:

| On `nativeapp` | On `main` |
| --- | --- |
| `amux::` (the library) | `node::` for identity, trust, routing, services, installation; `model::` for shared values; `wire::` for protobuf types and codecs; `client::` for RPC clients; `settings::` for persisted configuration |
| `amux_ui::{Model, Msg, update, ...}` (pure state) | `ui_state::` |
| `amux_ui::{Runtime, RuntimeOptions, ...}` (effects, connections) | `ui_runtime::` |
| `amux_tui::` | `tui::` |
| `amux_artifacts::` | `artifacts::` |
| `amux-cli` (binary crate) | `amux` |
| `amux-shot` | `shot` |
| `amux::testnet::` | `testnet::` |
| `crates/amux/proto`, `crates/amux/src/protocol/generated` | `crates/wire/proto`, `crates/wire/src/generated`; regenerate with `just protobuf` |
| `local-agents` feature | gone; the CLI composes `agent-runtime` explicitly, embedded nodes pass `host_factory: None` |

Placement of the daemon features written on `nativeapp`:

- The embedded relay, `RelayEndpoint`, `RelayConnection`, `DisconnectReason`:
  `node`'s transport layer.
- Repositories, revocation, pairing changes, the admin client: the `node`
  module of the same name.
- Claude and Codex proto additions: `wire`, then `just protobuf`.
- UI queue, provider, report and runtime changes: pure state and reducer
  logic in `ui-state`; anything that owns a connection, task, file or timer
  in `ui-runtime`.

## Step 2: port the harness extensions onto seams

`nativeapp` compiled the harness into the product through a build-script flag
selected by the debug profile, reaching it through about a hundred conditional
sites in fifteen production files. `main` replaced that pattern with injection
points: an installation accepts a `host_factory`; a provider session is built
`from_sources`; `agent-runtime` exposes narrow, always compiled, hidden adapters
over its backends.

Do:

1. Delete `crates/amux/build.rs` (now `crates/node`) and every conditional site
   controlled by its harness flag. Production structs carry no
   `Option<script::Provider>` field.
2. Scripted Claude PTY and SDK providers become a `LocalAgentHostFactory`
   implementation in `testnet`. The factory's sessions are built through
   `claude::pty::Session::from_sources` and the SDK session's transport
   seam, fed by the script. `register_scripted_claude`,
   `register_scripted_provider` and `end_scripted_session` on the agent host
   disappear; the topology declares which daemon uses the scripted factory.
3. Move `script.rs`, `sdk.rs`, `latency.rs`, `client.rs` and the added
   operator verbs into `crates/testnet/src`. Verb names do not change.
4. Move the served control door (`Topology`, `Control`, `Reply`,
   `Readiness`, and `ScriptFromReport`) from `e2e-runner` into a `serve`
   binary in `testnet`. `e2e-runner` goes back to running PTY scripts.
   Readiness reports the relay address and the fake identity service's
   address separately; phone journeys keep handing the app a static token
   because the phone's identity client is Swift and is tested with a
   scripted Swift adapter.
5. One invariant, written in `testnet`'s crate documentation: every control
   verb of the door is a method on the harness with the same name, so an
   in-process spec and a phone journey are the same sentence.
6. Fold the echo test agent into a trivial script if it is still used
   anywhere the scripted providers are available.

## Step 3: split the bridge

`crates/amux-mobile` becomes the three crates above.

- `projection.rs`, `cache.rs`, the queue and account-session bookkeeping
  from `runtime.rs`: `app-runtime`.
- Installation startup, credentials, relay link, shutdown from
  `runtime.rs`, and the embedded relay glue: `app-embedded`.
- `lib.rs`: `app-ffi`, symbols renamed from `amux_mobile_` to `amux_app_`,
  and the Swift `Bridge.swift`/`BridgeClient.swift` updated in the same
  commit. The generated header is a build output, not a committed file.
- `debug-tools` stays as a feature of `app-embedded`, and is the only place a
  product crate may depend on `testnet`. It is never a default feature.
- Tests that need only the projection and queue run under `cargo test -p
  app-runtime` with no node, no cbindgen and no static library build.

## Step 4: build recipes

iOS recipes join the root `justfile` as a module (`mod ios` in the justfile,
`ios/justfile` for the recipes), so `just --list` shows them and `just ios
build` runs them. Ownership stays with the iOS code; the recipe names from
the branch are kept.

Replace the unconditional bridge recipe that built three variants, recreated
frameworks and ran linkage smoke before ordinary work with:

- `just ios rust`: builds one slice, the active simulator architecture with
  `debug-tools`, under the `dev` profile, into one Cargo target directory for
  that triple. It packages the framework only when the Rust library or the
  generated header changed.
- `just ios build`, `just ios unit`: depend on `ios rust`. A Swift-only edit
  runs no cargo, generates no header and repackages nothing.
- `just ios package`: builds every required simulator and device slice under
  the `mobile` profile, assembles the shipping XCFramework, and runs the
  linkage check. Release recipes depend on this one.

The `mobile` profile (fat LTO, one codegen unit, size optimisation, abort) is
used only by `ios package` and release. Do not export `SDKROOT` for the
whole recipe environment: host-side build scripts are fingerprinted with it,
so alternating simulator and device builds invalidate each other. `cc`
selects the SDK from the target triple on its own; if a crate still needs
the variable, set it for that one cross-compiling cargo invocation only.

Output ownership is simple: each worktree owns its Cargo output, its
`DerivedData`, its packaged frameworks under `target/ios`, and any simulator
it created. Removing the worktree removes all of it. Nothing else needs to
discover or sweep these paths.

## Step 5: acceptance

Run and record these after the merge builds cleanly. Each is a test or a
recipe that exists in the tree, not a claim.

1. `just ci` and every `just ios` verification recipe pass on macOS; the
   platform CI matrix passes.
2. Start and stop an embedded installation repeatedly, including a failure
   during startup and a stop while callbacks are in flight. Exactly one
   terminal callback per owned handle, every time.
3. Attach to an external daemon, close every view, and confirm the daemon
   still serves another client.
4. Two accounts, two independently owned views: closing one or changing its
   selection neither cancels the other nor routes an operation to the wrong
   account.
5. Measure an initial simulator build, a no-op rebuild, a Swift-only edit and
   a Rust edit. The Swift-only path shows no cargo invocation.
6. `cargo tree -p app-ffi -e normal` contains no provider crate, no
   `agent-runtime`, no `pty-host`, and no test infrastructure unless
   `debug-tools` is enabled.

## Explicitly not required

Manifest and digest handshakes between the app and the framework; audits of
linked symbols or archive members; proving two native worktrees can run
concurrently; teaching `wt` to discover nested Cargo roots; any second build
system. If one of these turns out to be needed, it is a separate decision,
not part of this integration.
