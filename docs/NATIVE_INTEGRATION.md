# Native application integration

Nativeapp adapts to the Rust foundations as a separate integration change. It
extracts three packages from its working bridge and runtime code; this branch
does not provide empty package shells. The combined code is mergeable only
after the native implementation and evidence below exist.

## Package responsibilities

`app-runtime` owns reusable rich-client sessions, account-scoped cache and
projection, operation routing and typed presentation state. It accepts supplied
client connections and administration capabilities. It may depend on `model`,
`settings`, `client`, `ui-state`, `ui-runtime` and `artifacts`; it must not
depend on `node`, launch an installation, choose a device credential store or
assume a phone lifecycle. Presentation values remain independent of SwiftUI,
UIKit, AppKit, C pointers and JSON.

`embedded-client` owns embedded node startup, credentials and orderly shutdown.
It constructs a provider-free `node::Installation`, obtains explicit channels
or endpoints from that owner and supplies them to clients. It stops only the
resources it created. Attaching app-runtime to an external desktop daemon does
not give it shutdown authority over that daemon.

`client-ffi` is the language boundary. It owns serialization, exported symbols,
callbacks, opaque handles, cancellation and foreign-language lifetime rules.
It translates typed app-runtime values at the edge. Node internals and provider
objects do not cross the ABI.

Every operation carries its account/profile identity. Session, view and
subscription handles have independent ownership. Multiple windows or screens
may observe different accounts, and several views may share one account
session. Closing a view cancels only its subscriptions and pending view work;
it does not stop other views, the shared session or an attached daemon.
Platform shells own navigation, visibility/background policy, refresh cadence,
file selection, notifications and credential-store adapters.

## Build composition

Each application configuration selects exactly one bridge implementation and
links it once. Development simulator iteration builds only the needed simulator
slice with an incremental development profile. It does not use the shipping
profile's fat LTO or single codegen unit.

The current unconditional native recipe must be replaced. It builds shipping
simulator, shipping device and debug-tools simulator variants, recreates
frameworks and runs linkage smoke before ordinary app and unit-test work. The
replacement separates these paths:

- A simulator development task builds one selected bridge and only the active
  architecture, then packages it only when its Rust library or public header
  changed.
- Swift-only edits do no Rust compilation, unchanged-header generation or
  framework repackaging.
- An explicit verification task builds all required simulator/device slices,
  packages the shipping XCFramework or framework set and runs linkage checks.
- Release tasks use the shipping optimization profile and repeat multi-slice
  packaging and final linkage/architecture validation.

Each packaged artifact has a manifest containing its Rust revision, target,
profile, features, exported-header digest and library digest. A consumer rejects
a mismatched manifest rather than relying on framework search order or force
loading one of several bridges.

## Output and resource ownership

Every native Cargo root must be visible through wt's supported output-root
mechanism. A `.rustc_info.json` and Cargo profile directories nested somewhere
under `target/ios/...` are not automatically discoverable. Either simplify the
layout into declared roots or extend wt's adapter/configuration, then verify the
effective roots with wt diagnostics and a real post-task sweep. The outstanding
wt capability is described in [wt output requirements](WT_OUTPUT_REQUIREMENTS.md).

Cargo output, DerivedData, packaged frameworks, test results and simulators are
different resource classes:

- Each worktree owns mutable Cargo and DerivedData roots. They are never shared
  writable across worktrees.
- Generated frameworks are derived outputs with manifests; tasks replace only
  the variant they own and remove it with its owning worktree.
- Test results have a named producer and retention policy. Diagnostic captures
  are retained only with an explicit owner and purpose.
- Simulator names, devices, installed bundle state, Keychain state and captures
  are worktree-scoped. Teardown removes only a simulator the same worktree
  created.
- Cargo fingerprint sweeping applies only to Cargo roots. Native artifacts use
  their native lifecycle and must not be deleted because a Cargo unit appears
  stale.

## Shipping exclusions

The shipping Rust dependency closure and packaged artifacts must exclude
`agent-runtime`, `claude`, `codex`, `pty-host`, `replay-support`, `testnet`,
`test-agent`, `claude-specs`, `codex-specs`, `tui-fixtures`, `shot` and desktop
provider binaries. Verify both Cargo metadata and final linked symbols/archive
members. The embedded graph includes only the provider-free node and the client,
state, runtime and bridge layers required by the app.

## Combined-code acceptance

Run these checks in the native branch after integrating the foundation commit:

1. Start and shut down an embedded installation repeatedly, including failure
   during startup and cancellation while callbacks are in flight. Prove
   lifecycle ordering and one terminal callback per owned handle.
2. Attach to an external daemon, close all views and confirm the daemon remains
   running and usable by another client.
3. Exercise at least two accounts and two independently owned views. Closing or
   changing selection in one must not cancel the other or route an operation to
   the wrong account.
4. Verify callback serialization, queueing, cancellation and handle release
   across the language boundary, including late callbacks after a view closes.
5. For every app configuration, inspect linkage and prove exactly one bridge is
   selected with the expected architecture and manifest.
6. Measure an initial simulator build, a no-op rebuild, a Swift-only edit and a
   representative Rust edit. The Swift-only path must show no Cargo invocation,
   header generation or framework repackaging.
7. Run the explicit shipping multi-slice packaging/linkage task and verify the
   provider and test-infrastructure exclusions above.
8. Run two concurrent native worktrees with independent Cargo roots,
   DerivedData, simulators, bundle state and result paths. Exercise startup,
   callbacks and teardown in both, then show wt discovers and sweeps every Cargo
   root without touching the other worktree.
9. Run supported device and simulator tests, plus platform CI for architectures
   unavailable locally. Record unavailable checks as gaps rather than passes.

Bazel is deferred. It is neither a foundation completion criterion nor a
nativeapp merge requirement.
