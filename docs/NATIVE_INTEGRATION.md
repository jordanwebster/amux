# The app layer

The app layer is three small crates that any rich client reuses: the iPhone
app today, Mac and Windows desktop apps later, whether they embed a node or
attach to a daemon that is already running.

| Crate | Owns | May depend on | Must not depend on |
| --- | --- | --- | --- |
| `app-runtime` | The chats and fleet over the local runtime: session and fleet drivers, views asked for by row key, and changes gathered until the host's next turn | `model`, `client`, `ui-state`, `ui-view`, `ui-runtime`, `wire` | `node`, anything provider-specific, any platform SDK |
| `app-embedded` | The daemon's profile runtime hosted in process: its store and replica rows, identity, pairing, relay link and switchable source policy | `node`, `client`, `app-runtime`, `wire` | provider crates, test infrastructure |
| `app-ffi` | The C ABI: exported symbols, JSON in and out, callbacks, opaque handles; the `staticlib` crate type; the `cbindgen` build script | `app-runtime`, `app-embedded` and the value crates | anything else |

The one rule that matters: nothing in `app-runtime` imports `node`. A desktop
app that attaches to a running daemon uses `app-runtime` over a gRPC client
without `app-embedded`. Values are plain Rust serialized as JSON; their Swift
mirrors are generated from the Rust definitions (`cargo run -p xtask --
swift-types`, with `--check` failing on drift), and no SwiftUI, UIKit, AppKit
or C pointer types appear outside `app-ffi`. `just dependency-policy`
enforces the edges in the table.

Ownership rules the code keeps:

- Chats are independently owned handles. Closing one cancels its own stream
  and wake, nothing else.
- Only `app-embedded` stops an installation, and only the one it started.
- Rust never talks to the identity service on behalf of a rich client. The
  platform's own account client obtains a refresh token; `app-embedded`
  binds the profile with it and the relay link comes up from there.
- No client opens the store: the chats read it through the client service,
  called in process.

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
- `app-embedded` takes edge overrides (where the relay link dials, discovery,
  where direct links listen) that a test fills in; the phone build passes
  none. `app-ffi`'s `debug-tools` feature, never a default, lets a driving
  build keep direct links on loopback.

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
2. The C ABI pairs with a served desk by PIN, lists its agents from this
   device's own rows, reads a chat's rows by key, answers a permission by
   choice position and a question with picks, pages older rows below the
   oldest held, and writes a dump (`app-ffi`: `the_phone_pairs_opens_a_chat_answers_its_asks_and_pages_through_the_c_abi`).
3. However many updates land between the host's turns, it is woken once and
   takes every changed key together (`app-runtime`: `tests/runtime.rs`).
4. A signed-in phone pairs with a host through the relay and reads its
   agents there (`app-embedded`: `tests/embedded.rs`).
5. Build times on an Apple silicon development Mac, measured after the
   merge, are recorded in `DEVLOG.md`; the Swift-only path shows no cargo
   invocation.
6. `just ios graph-check` proves `cargo tree -p app-ffi -e normal` contains
   no provider crate, no agent host, no `pty-host` and no test
   infrastructure, with and without `debug-tools`.

## Explicitly not required

Manifest and digest handshakes between the app and the framework; audits of
linked symbols or archive members; proving two native worktrees can run
concurrently; teaching `wt` to discover nested Cargo roots; any second build
system. If one of these turns out to be needed, it is a separate decision.
