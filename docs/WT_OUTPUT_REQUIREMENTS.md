# Wt Cargo-output requirements

Amux relies on wt to snapshot a warm canonical Cargo target into new APFS
worktrees, discover every declared Cargo output root, sweep unreachable build
objects after tasks, and remove a tree's private output with the tree. Wt 0.4.0
provides those foundations, with one retention defect exposed by amux's focused
and workspace-wide test recipes.

## Preserve dependency-distinct workspace roots

Cargo can produce two units for the same workspace crate whose package,
features, target, profile, compile kind, rustflags, crate root and output kind
match while their resolved dependency fingerprints differ. This occurs when:

```text
cargo test --locked -p model
cargo test --locked --workspace --lib --tests --no-run
```

select different workspace roots and therefore unify dependency features in
different graphs. Both outputs remain useful because developers alternate
focused tests with full verification.

Wt 0.4.0's sweep selects the newest workspace unit for each visible identity
and follows dependencies from that unit. It does not include the unit's direct
dependency fingerprints in the root slot. In the measured amux workload, the
full test build removed 31 units after a focused test. After the next source
edit, the focused test rebuilt 19 unchanged third-party crates. Running the
same focused test immediately again compiled zero units. Timestamp ties can
also retain both variants temporarily, which makes the result timing-sensitive.

Wt should treat dependency-distinct workspace roots as simultaneously live
configurations when Cargo retains and can select both. The liveness key can
include the ordered dependency fingerprint set, or use an equivalent Cargo
unit-graph identity. Sweeping an older root remains valid when its complete
configuration is unreachable because a lockfile, target, feature selection or
declared task no longer produces it.

Acceptance should cover this exact sequence with compile-event assertions:

1. Warm a canonical with product and workspace test builds.
2. Snapshot a worktree.
3. Run the package-focused test, then the full test build.
4. Edit only the focused package and rerun its test.
5. Assert that unchanged third-party crates do not compile.
6. Repeat the full/focused alternation and prove the target inventory reaches a
   stable range after both configurations exist.

This belongs in wt because the repository cannot safely infer Cargo unit
liveness from files, ages or per-task byte budgets. Giving each focused task a
separate target directory would multiply snapshots and configuration output,
and broadening focused tests to the full workspace would discard the compile
boundary the package split was built to provide.

## Native output discovery

Nativeapp will produce Cargo roots for simulator, device and shipping
configurations. Wt must expose a supported way to declare or discover each
actual Cargo root. Scanning an arbitrary parent called `target` is insufficient
when `.rustc_info.json`, profile directories and Cargo locks are nested more
deeply. The adapter should report the roots it will snapshot and sweep, and
`wt doctor` should identify configured roots it cannot observe.

DerivedData, packaged frameworks, test results and simulators are not Cargo
roots. Their native owners need separate lifecycle rules; wt should coordinate
them only through declared resources or native-specific adapters, without
applying Cargo fingerprint reclamation to them.
