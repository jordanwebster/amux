# Flight notes

What later iterations need to know: how to build and test, what surprised you, what to avoid. Keep it short; prune what is no longer true.

## Repository rules (AGENTS.md)

- Wrap every test invocation in `timeout` (e.g. `timeout 600 cargo test ...`). A firing timeout is a hang to diagnose.
- Update `DEVLOG.md` in the same commit as each chunk of work. No Co-Authored-By trailers.
- Committed docs live in `docs/` (index at `docs/README.md`); `notes/` is gitignored.
- CI and close-out test runs set `AMUX_INVARIANT_FATAL=1`. Spec suite: `timeout 600 cargo test -p amux --features testnet --test spec`.
- amux is unreleased: break old dump formats, config keys and bindings freely. Do not touch amuxapp.
- Each worktree has its own amux instance via `.wt.toml` (`AMUX_CONFIG`, data dir under `.wt/amux/`); `target/debug` is first on PATH there.

## Implemented substrate (chunk 1, verified 2026-09-02)

- `Config::reports_dir()`; `amux_ui::report` owns schema v1 headers, bundle files, listing,
  `log_tail`, and per-kind retention (20 automatic; Bug/Tweak survive).
- `msgs.jsonl` = header line + one Msg per line (`Recorder::snapshot()` / `replay_msgs()`).
- `Runtime::report`, `install_panic_report`, `write_panic_report` write bundles; a log that will
  not read becomes a log-specific `Absent` reason, never a failed write. Release absent reasons
  say `unavailable in release build`.
- `amux-cli/build.rs` resolves the Git ref without invoking git and exposes `GIT_SHA`.

## Implemented substrate (chunk 2, COMPLETE, verified 2026-09-02)

- Renderer state is serde: `ViewState`, `ChatView`, `FeedViewport`, the Claude/Codex/unsupported
  views, `Composer`, `QuitGuard`, `Theme`/`Tokens`/`ColorMode`/`ThemeName`. ratatui's `serde`
  feature is now on workspace-wide (`Token.ansi` is a ratatui `Color`), and
  `unsupported::View::protocol` is a `String`, not `&'static str`.
- **`ChatView::feed_metrics` and `paint_cache` are `#[serde(skip)]`.** A view restored from bytes
  MUST be drawn before its keys are handled — chat key handling reads the metrics cache.
- `crates/amux-tui/src/chrome.rs` (unconditional, NOT debug-gated) owns `TraceEvent`, the
  `InputEvent`/`KeyRecord`/`MouseRecord` crossterm mirrors, `Chrome`, `ChromeConfig`,
  `ShellEffect`. `Chrome::step(&Model, &TraceEvent) -> Vec<ShellEffect>` is the only place view
  state mutates. `Draw` builds lines into the chrome; the caller paints them via `take_frame()`.
  Repaint debt lives on the chrome: `take_dirty()` / `mark_dirty()`.
- `run.rs` holds a `Chrome`, converts terminal events with `InputEvent::from_terminal`, and
  performs effects in `perform()`. A dispatch's op id comes back in as `TraceEvent::Dispatched`.
- Deviations from the plan's interface, decided in chunk 2: `ShellEffect::Report` was added to
  keep the existing `UiAction::DebugDump` (C-g) working — delete it when the capture key
  replaces DebugDump. `step` keeps the plan's return type; dirtiness moved onto the chrome.
- `Chrome::step` is now the ONLY writer of view state, with no exceptions:
  `TraceEvent::Tick { now }` carries the quit-guard expiry (`Chrome::expire` is private;
  `Chrome::quit_guard_armed()` is the shell's gate — run.rs steps a Tick only while a guard is
  armed and records it only when the step disarmed one, so quiet sessions do not fill the ring),
  and `TraceEvent::Notice(Option<String>)` carries every notice the shell sets (attach return,
  clipboard, report) through `run.rs::set_notice`.
- `crates/amux-tui/src/trace.rs`: `TraceRing` (two segments, `SEGMENT_LEN` = 5000), `Snapshot`,
  `TraceWindow` (events stay unparsed; `event(i)` parses one), `SharedTrace`, `record_shared`.
  `window()` returns `Option` — before the first draw there is no snapshot to fold from.
  ORDER: `roll_if_due` runs BEFORE the `Draw` event is recorded.
- `crates/amux-tui/src/replay.rs`: `capture_frame` (the goldens' serializer, moved here),
  `Replay::{load,step_to,step_to_end,draw_indices,frame,header,position}`, `verify`, `verdict`,
  `frame_diff`, `FrameDiff`. Stepping backwards resets to the snapshot and re-folds.
  A cell differs when its SYMBOL OR STYLE CLASS differs.
- `RuntimeOptions::msg_tap: Option<MsgTap>` (amux-ui) fires in `process()`, in fold order.
  `amux-cli/src/ui.rs` installs both ring and tap only under `cfg!(debug_assertions)`.
- `amux_ui::report::read_frame(&Path) -> io::Result<Option<FrameCapture>>` was added.
- Tests: `--lib serde_roundtrip`, `--lib chrome::`, `--lib trace::`, `--lib replay::`
  (`replay::divergence::` for the tamper/no-trace cases). The `fixtures` feature is on for
  `--lib` tests via the self dev-dependency; `tempfile` is now an amux-tui dev-dep.
- `crates/amux-tui/src/replay.rs`'s `mod tests::Session` is the reusable "record a live
  session the way run.rs does" harness — reuse it for the capture-flow tests. It can
  `draw`, `press`/`press_with`, `fold(Msg)` + `drained()`, `notice(&str)`, `advance(secs)`,
  `tick()` and `write_report(dir)`.
- Feeding a chat fixture new stream entries: the seq must CONTINUE the fixture's (ClaudeIdle
  ends at 5, so use 6, 7) and the payload's `sessionId` must be the fixtures' constant — a seq
  gap or a foreign session id is silently dropped and never reaches the feed.

## Implemented substrate (chunk 3, tasks 11-14 done, verified 2026-09-03)

- `amux-cli` now has a **lib target** (`[lib] name = amux_cli`) holding only
  `pub mod diagnostics;`. The bin keeps its own `mod` declarations and reaches it as
  `amux_cli::diagnostics::…`. Do NOT move the bin's modules into it — they would compile twice.
- `amux_tui::DiagnosticsSource` lives in `crates/amux-tui/src/diagnostics.rs` (UNCONDITIONAL — the
  `TuiConfig.diagnostics` field must exist in every build). `amux_cli::diagnostics::source(config,
  git_sha, debug_build, daemon_dump)` builds it; `resolved_log_path()` (AMUX_LOG or
  `amux::default_log_path`) is the single resolver, used by `RuntimeOptions.log_path` too.
- **The debug-vs-release gate is a `debug_build: bool` parameter**, called with
  `cfg!(debug_assertions)` (precedent: `validate_pair_qr_link_usage`). Tests always build with
  debug assertions on, so a `cfg!` inside the function would be untestable. Same shape for
  `bindings::report_key_row_for(bool)`.
- `crates/amux-tui/src/report_flow.rs` is `#[cfg(any(debug_assertions, test))]` and owns
  `CAPTURE_KEY` (C-g), `Frozen`, `ReportFlow`, `Stage`, `FlowStep` and the pure painter
  `paint(frame, frozen_buffer, theme, marks, prompt)`.
  - `Frozen::take` reads frame + trace window + recorder snapshot + log tail synchronously and
    spawns the daemon fetch; `Frozen.trace` is `Option<TraceWindow>` (the ring has no window
    before the first draw) and `Frozen.log_absent_reason` records a log that would not read.
  - `ReportFlow` owns `note` and `marks`; `Stage::Marks` carries only cursor/anchor/drag.
  - `ReportFlow::begin(frozen, theme)`. Marks paint `theme.mark()` (style class `A`), the prompt
    row `theme.report_prompt()` (class `P`).
- `bindings::report_key_row()` lives in bindings.rs (NOT report_flow.rs — a fn that must return
  `None` in release cannot live in a debug-gated module) and feeds the fleet and both chat
  sections. `UiAction::DebugDump` / `ShellEffect::Report` are gone.
- `amux_ui::report::LOG_TAIL_BYTES` is now public; every capture path shares the 64 KiB budget.
- **run.rs is fully wired**: `capture()` returns the `Frozen`; `report_flow()` then owns the
  terminal and the `EventStream` until the prompt is answered — it never calls `record()` or
  `chrome.step()`, which is what keeps the flow out of the trace. Cancel sets no notice; both
  paths `chrome.mark_dirty()` so the next turn paints live state over the frozen frame.
  `last_frame: Option<Buffer>` is cloned from `terminal.draw()`'s `CompletedFrame`.
  **Release builds need the `report_flow` stub taking `Infallible`** (mirroring `capture()`'s);
  without it `cargo build --release -p amux-cli` fails. Check the release build after touching
  either.
- `ReportFlow::finish(draft, &writer)` consumes the flow: await the daemon (2 s bound, the task
  is aborted on timeout) → `ReportWriter::write` → `replay::verify` the written bundle →
  `amux_ui::report::set_verdict` rewrites `report.json`. Verify cannot run before the write —
  it reads a report *directory*. A replay error other than `NoTrace`/`NoCapturedFrame` is
  stamped `Diverges`, not `Unchecked`.
- `ReportParts` now has `daemon_absent_reason` beside `log_absent_reason`; the shared
  `absent_reason` only ever speaks for a trace that could not be taken.
  `report::set_verdict(path, verdict)` is public — chunk 5's `amux debug report replay` should
  reuse it rather than rewriting the header itself.
- `crates/amux-tui/src/replay.rs`'s test harness is crate-wide now: `cfg(test) pub(crate) mod
  tests`, with `Session`/`VIEWPORT`/`open`/`draw`/`press`/`fold`/`drained`/`capture` `pub(crate)`
  and `Session::into_capture() -> (SharedTrace, Buffer)`. Use it to freeze a REAL recorded
  session (see `report_flow::written`); a hand-built buffer will not replay to `Reproduces`.
- Adding the C-g row moved 7 goldens; regenerate with
  `UPDATE_GOLDENS=1 cargo test -p amux-tui --all-targets`. The chat overlay is height-limited and
  already prints "⋮ more", so an added row pushes the last one out of view by design.
- The overlay's own golden is `report_overlay_dark{,_styles}`, produced by a test in
  `tests/component_golden.rs` that renders a fixture, freezes the buffer and calls `paint`.

## Remaining substrate observations

- Render is pure: `render(&Model, &ViewState, &FrameContext, frame)`. `crates/amux-tui/tests/golden.rs` renders into ratatui `TestBackend` and serializes a frame as text plus a per-cell style-class map via `theme.classify(style)`; goldens regenerate with `UPDATE_GOLDENS=1`.
- `crates/amux-shot` (workspace member, outside default members, vendors fonts) exposes `rasterize(&Buffer, Theme)` and `write_png`; it depends on `amux-tui` with the `fixtures` feature. Keep it out of the shipped binary. Shared-target-dir caveat in its README.
- `crates/amux-tui/src/fixtures/mod.rs` builds `NamedState`s through Msgs and drives real key/mouse handlers; a natural home for a report-backed fixture loader.
- Mouse capture is enabled with the chrome (`terminal.rs`); chat already handles wheel events, and left-button down/drag/up events reach the loop with cell coordinates.
- Daemon: `ClientService.Debug` RPC → `crates/amux/src/debug/server.rs` `dump_server_debug_info`; `host_count`, `route_count`, `remote_agent_count` are literal zeros. `amux debug --format yaml|json` exists in `crates/amux-cli/src/main.rs`.
- `debug_assertions` gating precedent: `open_in_process_protocol_plane` and friends in `crates/amux/src/services/agent/mod.rs`.
- TUI and daemon both log to `amux::default_log_path()` unless `AMUX_LOG` is set.
- Session byte tails: `ByteReplayQuery::Tail` / `SequencedReplayQuery` in `crates/amux/src/agents/buffer.rs`.
- Redaction: `replay-support` owns the personal-identifier field rule used by the capture tools (`AMUX_REDACT_*`).
- `Model` is `Serialize + Deserialize`; `Msg` is fully serde (`crates/amux-ui/src/msg.rs`).

## QA capture (2026-09-03)

- Launch the worktree daemon with `AMUX_CONFIG=$PWD/.wt/amux/config.yaml target/debug/amux server start`, create `target/debug/amux new test-agent --name live-qa` in tmux, then open `target/debug/amux ui` in another pane.
- C-g works chrome-wide from the fleet and writes a self-verified tweak report under `.wt/amux/data/reports/`; the captured run used keyboard marks and produced two notes.
- `lsof -a -p <tui-pid> -i` is the correct macOS process-scoped socket check; it returned no TCP/UDP rows for the TUI.
- The test-agent fallback did not enter structured chat through fleet `o` in this run; real Claude was not launched because its shim may transmit workspace data. See `.autopilot/qa/capture-key-and-report-flow.md`.
