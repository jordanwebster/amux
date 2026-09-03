# Flight notes

- Chunk 1 is `d341887` plus repair `43f4848`: the reader uses Document
  vocabulary and `AskDocument`'s serde tag is `kind`. Its exact chunk gate
  passes. Regenerate fixed-width TUI frames with `timeout 900 env
  UPDATE_GOLDENS=1 cargo test -p amux-tui`; rows intentionally trail spaces.
- Chunk 2 is `4fabde5`, `f132464`, `e86eea1`. Owner and Cache share
  `blobs/<hex>` and atomic `index.json`; recovery, lifetime, LRU, size and hash
  checks pass. Evidence is `.autopilot/evidence/artifacts-unit.txt`.
- Chunk 3's RPC/codegen is `0c086bc`, diff is `c47476e`, daemon ownership and
  routing are `aed0a07`, followed by materialisation, canonical-path repair,
  and testnet coverage. Put/get/diff work locally and remotely; explicit pins
  emit `amux.attachments`, materialise PTY/Codex paths and SDK/Codex images,
  replay refs, and survive restart/lifetime tests.
- Any `cargo test -p amux <filter>` must add `--lib` or `--test NAME`: three
  harness=false live binaries otherwise treat the filter as a scenario.
- Cargo writes to `/Users/jlw/.wt/cache/cargo-build/amux/`; sandboxed workers
  may need test-command escalation. Git fsmonitor IPC warnings are benign.
- Chunk 3's corrected exact gate passes. Recreate its evidence with the Task 10
  check at `.autopilot/evidence/spec-attachments.txt`.
- Task 11 is `5495632`: attachment mention parsing/formatting, draft hashing,
  path preservation and malformed-candidate recovery are complete. Task 12 is
  `5bdbbc8`: review parsing keeps files/hunks/numbered rows, Old/New anchors,
  normalized ranges, comments and round-trippable bodies. Bodies begin with
  `blobs: <JSON pairs>`; bases are `working-tree` or `branch:<base>`.
- Task 39 length-frames every review comment after its quoted rows with
  `text-bytes: <UTF-8 length>`, so quote-prefixed, heading-shaped, empty, blank-
  line, and Unicode text round-trip; ambiguous old unframed bodies are rejected.
  The exact chunk-4 gate and strict amux-ui Clippy pass. The Proof table assigns
  no standalone evidence capture to these low-level stage-4 tasks.
- Task 13 adds a per-Claude/Codex `AttachmentIndex`, authoritative metadata
  overlay through `AttachmentIndex::segments`, `describe`, and an eight-patch
  FIFO through `insert_diff`/`diff`. Prompt and assistant entries retain raw
  provider text and also expose typed `content` segments. Claude's internal
  session-id epoch reset preserves refs replayed just before the first new
  transcript row; full stream reopens rebuild the index. The exact 4-test
  attachment spec, full 248-test spec suite, and all-target Clippy pass.
- Task 14 adds provider-neutral attachment send/fetch/open/diff commands and
  typed effects/outcomes. Attachment sends reuse native gates/encoders but
  emit exactly one `PutThenSend`; only that live effect holds bytes. Pending,
  finished and recorded commands redact bytes while keeping id/size, including
  synchronous refusal. `OpError` is now a typed enum with accessors used by
  existing TUI surfaces. The exact 8-test task check, full 252-test spec suite,
  amux-ui Clippy, and all-target amux-tui compile pass.
- Chunk 5 so far: `031ab62` (composer tokens) and `d6ea6dc` (paste/Ctrl+V
  routing). A token is one private-use char in the draft, so cursor, delete and
  kill rules cover it whole; `Composer::export(review)` yields canonical
  elements plus the artifacts to put and pin (a review's diff rides bytes-less,
  only to be pinned). `Composer.tokens` is boxed: growing Composer inline trips
  `clippy::large_enum_variant` on `chat::View`. Clipboard reading lives in
  `src/clipboard.rs` (arboard + png) and `chat/attach.rs` turns content into a
  token; key tests inject a `ClipboardContent` — never a real clipboard. Test
  helpers in `chat::claude::keys::tests` are `pub(super)`, and the sibling
  `attachments` module must stay a sibling or the `keys::attachments` filter
  misses it. Adding a binding row changes six help-overlay goldens.
- Task 15 executes all four effects. `execute_put_then_send` uses the narrow
  `AttachmentClient` boundary so the runtime test proves ordered puts, one send,
  and fail-fast behavior. `RuntimeOptions` owns one flat persistent Cache and an
  injectable path opener; both open and review fetch use its verified bytes.
  amux-cli uses `<default cache dir>/artifacts` and converts
  `ui.artifact_cache_mib` (default 256) to bytes. The three exact runtime tests,
  the config parse test, and strict amux-ui/amux-cli Clippy pass. Embedded-server
  tests in `tests/runtime.rs` share an async guard because parallel embedded
  teardown hangs; isolated cases and the serialized exact filter finish fast.
  The complete chunk-4 verification command passes.
- Chunk 5 is complete for the ui-developer: `e907cef` (feed painting) and
  `ef0a2b8` (draft chapter, shot states). Its exact chunk gate passes. Only the
  qa-tester's task 21 is left in chunk 5.
- Attachments in the feed: each one is its OWN `PaintedBlock`, keyed by
  `chat::attachments::attachment_key(owner, index)`, so `<leader> j/k` focuses a
  single attachment and `<leader> o` opens exactly it (image/file → the runtime's
  `Command::OpenAttachment`; text → Claude's reader, `ReaderSource::Text`, which
  carries the body instead of resolving it; review → chunk 6). Codex has no
  reader, so a text row there is not openable. Prompts and messages now paint
  `chat::attachments::prose(&entry.content)` — elements never reach markdown.
- Golden viewport policy: every chat golden must be 120x40 unless it is listed in
  `component_golden.rs::WIDTH_DECLARING_GOLDENS`. Use `render_frame` (which
  ignores its width/height args and uses the standard viewport), not
  `render_frame_at`, or `viewport_policy_…` fails.
- `amux-ui` cannot see `amux-tui`, so `tests/spec/draft.rs` proves only the
  Model-side of a draft; the composer's own rules stay in amux-tui. No committed
  Claude PTY fixture ends idle after a permission ask — `question_mixed` does.
- New shot states: `chat-attachment-blocks`, `chat-mixed-draft`, in the
  `attachments` set. Adding a set needs four edits in `amux-shot/src/main.rs`:
  `SET_NAMES`, the const, `set_members`, and the `all` loop.
- Task 21 live QA was blocked on 2026-09-03: after `target/debug/amux server
  start`, both `target/debug/amux new claude --name ...` (PTY) and `--driver sdk`
  fail before opening the chat with `managed Claude MCP launch route is no
  longer valid` under Claude Code 2.1.259. Evidence is in
  `.autopilot/evidence/live/attachments-qa/`; Task 40 tracks the launch defect.
