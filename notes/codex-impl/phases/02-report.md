# P2 report — `StructuredLogSource` split

Date: 2026-08-12.

## Baseline

- Branch: `main`
- Phase-start ref: `42a6217cff150da3532c8f0a0b41ce9ea39c848e`
- Pre-existing worktree change preserved and excluded from P2:
  `notes/codex-impl/phases/01-report.md`

## Implemented

`StructuredLogSource` is now an agent-agnostic retained, sequenced sink. It
contains only a `MultiplexStructuredBuffer`; it has no Claude, transcript,
tailer, or path knowledge. Retention is selected by each caller. Claude and
the test agent both select 1000 entries, preserving the former
`MAX_LOG_ENTRIES` policy.

Claude now owns `TranscriptIngest` in
`agents/claude/transcript_ingest.rs`. It wraps a sink and owns the
`TranscriptTailer`/`JoinHandle` slot, current transcript path, same-path no-op,
stop-clear-relink lifecycle, close ordering, and `DebugView` serialization.
The tailer writes through `StructuredLogSource::write` rather than receiving
the underlying multiplex buffer. Truncation handling remains in
`transcript.rs`, and that file remains the sole production emitter of the
literal `amux.transcript_ready` row.

Claude sessions now store the ingest and derive their subscription sink from
it through the unchanged `ClaudeSession::log_source()` surface. Hook writes
use the wrapped sink; transcript links and close calls use the ingest. The
same tailing/relink assertions moved beside the ingest, while the direct-write
test remains with the sink.

## API actually built

`StructuredLogSource` (`pub(crate)`):

```rust
fn new(retention: usize) -> Self
async fn write(&self, payload: serde_json::Value)
async fn subscribe(&self) -> Option<MultiplexStructuredReader>
async fn subscribe_with_query(
    &self,
    query: Option<SequencedReplayQuery>,
) -> Option<(MultiplexStructuredReader, u64)>
async fn current_seq(&self) -> u64
async fn clear(&self)
async fn close(&self)
```

`TranscriptIngest` (Claude-private):

```rust
fn new(source: StructuredLogSource) -> Self
fn log_source(&self) -> &StructuredLogSource
async fn link_transcript(&self, path: PathBuf)
async fn close(&self)
```

`DebugView<'_, TranscriptIngest>` serializes the same optional
`current_path` map as before.

`spawn_pty_agent` now returns
`Result<(PtyHandle, tokio::task::JoinHandle<()>)>`; structured sink creation
and closure are concrete-session policy rather than generic PTY policy.

## Deviations and rationale

- The sink exposes `clear()` rather than its underlying buffer. This is the
  narrowest ingest surface that preserves clear-on-relink with sequence
  continuity.
- `ClaudeSession` stores only `Option<TranscriptIngest>`, not parallel ingest
  and sink fields. `log_source()` clones the ingest's sink for subscription
  consumers. This makes it impossible for the two fields to diverge.
- Generic PTY spawning no longer constructs a structured sink. Claude and
  test-agent `start()` methods wrap the PTY exit handle and close their own
  structured state after process exit. For Claude, this calls ingest close so
  the tailer stops before the sink closes, matching the old composite close
  ordering without teaching the PTY layer about Claude.

## Surprises and tech debt

No behavioral or fixture drift was found. The two pre-existing dead-code
warnings for test-only tracked-listener helpers still appear in workspace
clippy/test builds; P2 introduced no warnings.

The test-agent debug view still emits the historical `"transcript": {}` field
when it has a structured sink. P2 preserves that observable debug shape even
though the sink itself no longer implements transcript-oriented debug
serialization. Renaming that field is adjacent cleanup, not part of a
zero-behavior split.

## What P3 should know

- `AgentSession::log_source()` still returns `Option<StructuredLogSource>`;
  its enum shape is untouched.
- Claude backend lifecycle state is now `transcript_ingest`, and Claude close
  must continue to go through `TranscriptIngest::close()`.
- Structured retention is a backend/protocol decision at construction time;
  Claude currently passes 1000.
- `spawn_pty_agent` is now strictly PTY-oriented and does not return or close
  structured output.

## Verification

- `cargo fmt --all` — pass
- `timeout 600 cargo clippy --workspace --all-targets` — pass
- `timeout 600 cargo test --workspace` — pass
- `timeout 600 cargo test -p amux --features testnet --test spec` — pass,
  44 tests
- Affected `agents::` unit-test subset — pass, 80 tests during implementation;
  the final workspace run passed after removing two redundant new sink tests
- `git diff --check` — pass
- `rg "transcript|tailer|Path|Claude|current_path"
  crates/amux/src/agents/log_source.rs` — no matches
- Existing fixtures, goldens, and `crates/amux/tests/spec/**` — untouched
