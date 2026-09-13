# The app layer

The app layer is three small crates that any rich client reuses: the iPhone
app today, Mac and Windows desktop apps later, whether they embed a node or
attach to a daemon that is already running.

| Crate | Owns | May depend on | Must not depend on |
| --- | --- | --- | --- |
| `app-runtime` | Account-scoped sessions, the projection from reducer state to typed presentation values, the fleet cache, and the frame-coalesced event queue | `model`, `settings`, `client`, `ui-state`, `ui-runtime`, `artifacts` | `node`, anything provider-specific, any platform SDK |
| `app-embedded` | Starting, credentialing and stopping a provider-free `node::Installation`, holding the relay link, and handing clients to `app-runtime` | `node`, `client`, `app-runtime` | provider crates, test infrastructure |
| `app-ffi` | The C ABI: exported symbols, JSON in and out, callbacks, opaque handles, cancellation, foreign lifetime rules; the `staticlib` crate type; the `cbindgen` build script | `app-runtime`, `app-embedded` | anything else |

The one rule that matters: nothing in `app-runtime` imports `node`. A desktop
app that attaches to a running daemon uses `app-runtime` and `app-ffi`
without `app-embedded`. Presentation values are plain Rust and JSON; no
SwiftUI, UIKit, AppKit or C pointer types appear outside `app-ffi`.
`just dependency-policy` enforces the edges in the table.

Ownership rules the code keeps:

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

## Seams instead of test builds

Nothing test-shaped is compiled into a product crate under a `cfg` or a
feature. Where a harness needs to reach inside, production code exposes an
injection point that is always compiled and ordinary to call:

- An installation accepts a `host_factory`; a `node::Installation` with none
  starts no provider runtime, which is what a phone embeds.
- A daemon under test receives its listeners, relay link and host factory
  through `node`'s profile-runtime fixtures.
- A provider session is built `from_sources`; `agent-runtime` exposes hidden,
  always-compiled adapters that let a harness supply a scripted Claude PTY or
  SDK session, or a recorded Codex session, in place of a real process.
- The relay's presentation values, `model::RelayConnection` and
  `model::DisconnectReason`, live in `model`, so the projection reads them
  without a daemon dependency. A plaintext loopback relay is an ordinary
  `node::RelayEndpoint` constructor; `app-embedded` allows it only behind its
  `debug-tools` feature, which is never a default and is the only place a
  driving affordance is switched on.

The harness the phone is tested against is the `testnet` crate: a declared
topology of real daemons, one fake relay, one fake identity service and
scripted providers, driven in-process by Rust specs and out-of-process by
`target/debug/testnet serve`. Every control verb of the served door is a
method on the harness with the same name, so an in-process spec and a phone
journey are the same sentence. [TESTNET.md](TESTNET.md) owns the topology
format and the control protocol.

## Build recipes

iOS recipes are a module of the root `justfile`: `just --list ios` shows
them and `just ios build` runs one. The bridge is built one slice at a time
for development and in full for shipping:

- `just ios rust` builds the active simulator slice with `debug-tools` under
  the `dev` profile into one Cargo target directory for that triple, and
  repackages the framework only when the Rust library or the generated header
  changed. When the Rust sources are unchanged it runs no cargo at all.
- `just ios build` and `just ios unit` depend on `ios rust`. A Swift-only
  edit runs no cargo, generates no header and repackages nothing.
- `just ios package` builds every simulator and device slice under the
  `mobile` profile, assembles the shipping XCFramework and runs the linkage
  check. Release recipes depend on it.

The `mobile` profile (fat LTO, one codegen unit, size optimisation, abort) is
used only by `ios package` and release. `SDKROOT` is not exported for the
recipe environment: host-side build scripts are fingerprinted with it, so
alternating simulator and device builds would invalidate each other.

Each worktree owns its Cargo output, its `DerivedData`, its packaged
frameworks under `target/ios`, and any simulator it created. Removing the
worktree removes all of it. Nothing else needs to discover or sweep these
paths.

## What holds

Each item is a test or a recipe in the tree, not a claim.

1. `just ci` and every `just ios` verification recipe pass on macOS; the
   platform CI matrix passes.
2. Starting and stopping an embedded installation repeatedly, including a
   failure during startup and a stop while callbacks are in flight, yields
   exactly one terminal callback per owned handle
   (`app-ffi`: `every_handle_ends_exactly_once_however_it_is_stopped`).
3. Attaching to an external daemon and closing every view leaves the daemon
   serving another client (`app-embedded`: `tests/attach.rs`).
4. Two accounts with two independently owned views: closing one or changing
   its selection neither cancels the other nor routes an operation to the
   wrong account (`app-embedded`: `tests/views.rs`).
5. Build times on an Apple silicon development Mac, measured after the
   merge, are recorded in `DEVLOG.md`; the Swift-only path shows no cargo
   invocation.
6. `just ios graph-check` proves `cargo tree -p app-ffi -e normal` contains
   no provider crate, no `agent-runtime`, no `pty-host` and no test
   infrastructure, with and without `debug-tools`.

## Explicitly not required

Manifest and digest handshakes between the app and the framework; audits of
linked symbols or archive members; proving two native worktrees can run
concurrently; teaching `wt` to discover nested Cargo roots; any second build
system. If one of these turns out to be needed, it is a separate decision.
