# P1 report — `terminal_v1`: the agent-independent raw byte plane

Date: 2026-08-12.

## Baseline

- Branch: `main`
- Phase-start ref: `fe7fc41c894400963e66e01fd24e5d038012a2ab`
- Phase-start tree: clean

## Implemented

The raw PTY byte plane is now named `terminal_v1` and is owned by the core.
There is no alias, compatibility shim, or remaining `claude_raw_v1` path.

- `crates/amux/proto/amux/v1/amux.proto` owns `TerminalV1Args`,
  `TerminalV1ReplayQuery`, and `TerminalV1Control`, with the old shapes and
  field numbers unchanged. The three old messages were deleted from
  `claude.proto`.
- `crates/amux/src/agents/terminal_io.rs` owns the Rust domain types and codec:
  `TERMINAL_V1`, `TerminalV1Args`, `TerminalV1ReplayQuery`,
  `TerminalV1Control`, `encode_terminal_v1_args`,
  `decode_terminal_v1_args`, `encode_terminal_v1_control`, and
  `decode_terminal_v1_control`.
- `amux::terminal_io` publicly exports the protocol constant, domain types,
  and client-side encoders. `amux::claude_io` now exports only the structured
  `claude_pty_transcript_v1` codec.
- Every PTY-backed session advertises `terminal_v1`. Subscribe, replay-tail,
  input, and resize/control dispatch use the core codec; the CLI raw attach and
  send paths use the same public surface.
- Stale protocol literals in core/UI/TUI unit-test builders, capture-harness
  support, and golden-test input models were renamed. No assertions, capture
  fixtures, or golden snapshots changed.

## Deviations and surprises

No implementation deviation from the brief. `TerminalSize` remains declared
in `claude.proto` because `amux.proto` already imports that file for
`ClaudeCreateConfig`; moving the shared message into `amux.proto` would create
an import cycle. This was pre-existing proto-file placement debt and does not
leave any raw-terminal message in the Claude namespace. It can be reconsidered
when agent creation messages are reorganized, but is not worth a new
abstraction in this rename phase.

The workspace clippy/test builds continue to print two pre-existing dead-code
warnings for test-only tracked-listener helpers. P1 introduced no warnings.

One workspace-test attempt reached the 600-second wrapper while entering
doctests, after every executable test suite had passed. There was no stuck test
or surviving test process; direct test-binary startup checks were immediate.
An exact rerun from the completed build finished normally with exit status 0,
so this was transient gate wall-clock/tooling overhead rather than a protocol
or test hang.

## Verification

- `cargo fmt --all` — pass
- `timeout 600 cargo clippy --workspace --all-targets` — pass
- `timeout 600 cargo test --workspace` — pass
- `timeout 600 cargo test -p amux --features testnet --test spec` — pass,
  44 tests
- `grep -rn "claude_raw_v1\|ClaudeRawV1" crates/` — no matches
- `git diff --check` — pass

## Suggested `docs/UI.md` wording

Replace the raw-protocol prerequisite wording with:

> A client that does not know an agent type degrades to the `AgentCard` and
> can still attach to its raw terminal when the session advertises the
> agent-independent core protocol `terminal_v1`; every PTY-backed session
> currently advertises it.

The deferred typed-agent-identity bullet can replace its final clause with:

> Raw terminal attachment is already agent-independent through `terminal_v1`;
> only the typed known/unknown agent descriptor remains deferred.
