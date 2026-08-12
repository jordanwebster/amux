# amux Development Log

This file tracks significant development work, decisions made, and current state. Update this file after completing a chunk of work.

---

## How to Maintain This Log

1. **Add new entries at the top** (reverse chronological order)
2. **Use the standard entry format** shown below
3. **Be concise but complete** - future you will thank present you
4. **Include verification results** - what was tested and how
5. **Document decisions and rationale** - not just what, but why

### Entry Template

```markdown
## YYYY-MM-DD: Brief Title

### Summary
One paragraph describing what was done.

### Changes
- List of files created/modified
- Key structural changes

### Decisions Made
- Decision 1: rationale
- Decision 2: rationale

### Verification
- What was tested
- Results

### Next Steps
- What remains to be done
```

---

## 2026-08-12: Chat V1 Phase 6 — default-open-mode setting (A1)

### Summary
The chat entry mode setting lands in the usual amux config
(`docs/CHAT.md` A1, owner directive "Config home"): `ui.
default_open_mode: raw | chat` in `config.yaml`, following the
`keybinds.leader` idiom — a serde-defaulted section struct with
`deny_unknown_fields`. The shipped default is `raw`; `chat` flips what
the fleet's Enter opens (the non-default mode stays reachable via
Ctrl+Enter/`o`, wired in the next chunks). Mobile clients are always
structured and read no such setting.

### Changes
- `crates/amux/src/config.rs`: `OpenMode` (raw|chat, Default=Raw),
  `UiSettings { default_open_mode }`, `Config.ui` field; tests for the
  shipped default, the YAML round-trip, and unknown-variant rejection.
- `crates/amux/src/lib.rs`: export `OpenMode`, `UiSettings`.

### Verification
- `timeout 600 cargo test -p amux --lib config::` — 25 passed.

---

## 2026-08-12: Phase 5 — simplification pass

### Summary
Behavior-preserving cleanup of the Phase 5 surfaces (orchestration step
8); all goldens byte-identical, no test assertion touched. The main
altitude fix: the ask panels' text fields and the main composer now
share ONE readline dispatch — `composer::readline_key` (motion, kills,
yank, printable insertion) — instead of two parallel key matches;
`composer_key` and `ask_ui::field_key` keep only their own layers
(multiline row motion / send / scroll vs. the consume-everything
one-line frame). Repeated idioms got one home each: `ReaderView::ask()`
for the four open-on-ask literals, `ChatView::ask_reader_open` for the
five source-match checks, `AskUi::menu_cursor` for the three cursor
extractions, and `render::push_right` for the three right-aligned
annotation computations (`(1 of N)`, the reader's position indicator).
`sync_ask` restructured from the `head_exists` boolean block into a
let-else early return so the no-head / new-head / in-flight rules read
in order; `reader_tail` filters the panel state by ask id once, killing
the `expect("checked above")`. Premature abstraction removed: panel.rs's
single-caller `StyleKind` enum folded into `answer_summary(theme)`
returning styles directly. Dead pub surface trimmed: `diff::
magnitude_text` and `diff::new_file_rows` drop to `pub(crate)`
(`reader_rows` stays pub — the golden suite drives it directly). Net
-91 lines.

### Verification
- `cargo fmt`; workspace clippy `--all-targets --features amux/testnet
  -D warnings` clean.
- `timeout 600 cargo test -p amux-tui` (goldens unchanged on disk),
  `-p amux-ui`, `-p amux --lib`, `-p amux --features testnet --test
  spec`, `-p amux-cli` — all green.

---

## 2026-08-12: Chat V1 Phase 5 — codex review fixes

### Summary
Five accepted findings from the phase's codex review, each fixed with a
covering test. (1) The ask-time diff no longer discards `similar`'s
missing-newline fact: a ± row lacking its final newline is followed by
the jsdiff/git `\ No newline at end of file` marker row (rendered dim
verbatim via the existing unknown-prefix path, no line number), so an
edit differing only by a final newline no longer shows visually
identical -/+ rows; Equal rows are exempt — an unchanged EOF missing
its newline on both sides states no difference the approval needs.
(2) Reader scroll metrics now compute bounds from the SAME tail
derivation the frame renders (`reader_tail`, extracted; one layout, two
consumers) instead of assuming the writable action-row tail — End/PgDn
in a resolved-plan or read-only reader land exactly on the render clamp
and the next Up moves immediately. (3) A synchronous refusal of an
answer submitted from the reader now closes the reader to the docked
panel, where the stated failure renders — the same drop an async
SendFailed takes, so no lost outcome hides behind the overlay.
(4) Paste routes on the model/focus state like keys do: read-only chats
drop it (no composer surface exists to retain it invisibly), and the
pre-reconciliation pending-ask window is closed by the same defensive
`sync_ask` — a docked menu stage drops the paste instead of feeding the
hidden composer. (5) The Other field uses the encoder's trimmed
emptiness rule everywhere (`ask_ui::other_present`): whitespace-only
text neither counts as answered, nor checks a box, nor rides a
response — no avoidable synchronous refusal, no lying review tab.

### Changes
- `crates/amux-ui/src/claude/artifact.rs`: missing-newline marker rows;
  test `a_newline_only_edit_states_the_missing_newline` (both
  directions).
- `crates/amux-tui/src/chat/reader.rs`: `reader_tail` extraction;
  `scroll_metrics` uses it (+ readonly derivation).
- `crates/amux-tui/src/chat/mod.rs`: reconcile closes an Ask-source
  reader when a sync refusal is stated.
- `crates/amux-tui/src/chat/keys.rs`: `handle_chat_paste(chat, model,
  text)` — read-only drop + sync-first routing; tests
  `paste_in_a_readonly_chat_retains_nothing`,
  `paste_with_a_pending_ask_before_reconcile_retains_nothing`,
  `end_then_up_moves_immediately_in_a_resolved_reader`,
  `a_refused_answer_from_the_reader_surfaces_in_the_docked_panel`
  (frame-asserted), `whitespace_only_other_stays_unanswered`.
- `crates/amux-tui/src/chat/{ask_ui,panel}.rs`: `other_present`
  (trimmed) replaces raw emptiness in answered/commit/toggle/display.
- `crates/amux-tui/src/run.rs`: paste call site passes the model.
- Golden: `chat_ask_permission_newline` (new); the unverified-shape
  fixture gained trailing newlines so its refusal stays the subject;
  every other golden byte-identical.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean.
- amux-tui 83 lib + 49 chat golden + 19 fleet golden; amux-ui 30 lib
  + 1 runtime + 123 spec; amux --lib 401; amux spec (testnet) 44;
  amux-cli 53 — all under `timeout 600`.

---

## 2026-08-12: Chat V1 Phase 5 — docked ask panels, the reader, read-only chat

### Summary
The chat's interactive surface: every ask form, the fullscreen reader,
and the read-only variant. A pending ask takes over the composer area
behind a dim rule (C1) — head-of-queue with an honest `(1 of N)`, the
draft untouched beneath (D1), not dismissible while pending: Esc steps
back stages and floors at the menu. The permission panel (C2) renders
tool identity + magnitude, the ask-time numberless mini-diff (≤8
screen rows, wraps counted, leading context dropped to one when tight,
`⋮ +K more lines · f full diff`), the Write `+` block with `(N
lines)`, `$ command` for Bash, a compact fallback otherwise; option
2's label derives from the hook's suggestion facts; unverified menu
shapes render read-only-style with the encoder's typed refusal stated;
deny opens the optional one-line feedback stage (empty = plain deny).
The question form (C4): digit/↑↓ select + Enter confirm (never
instant-submit), tab row + mandatory submit/review step when
multi-question or any multi-select, `[x]` Space toggles, inline
`Other…` editor, unanswered review items in error color. Plan review
(C3) opens the READER directly — full plan, position indicator, the
three-way action row; Esc docks to the truncated panel form, `f`
returns; request-changes swaps the action row for the mandatory
feedback field (q types there — P2). The reader is ONE overlay over
typed artifacts: Plan (Phase 4's markdown renderer), Diff (chat::diff
— absolute numbers walked from `structuredPatch`-shaped hunks with the
repeat-number convention, numberless ask-time, blank-gutter wrap
continuations, `⋮` hunk gaps, four fg-only diff.* tokens), NewFile
(numbered `+` block); pager keys j/k g/G PgUp/PgDn Home/End, q/Esc
close; Ctrl+T reopens accepted plans with ←/→ stepping (the feed's
plan entry now carries `· ctrl+t to read`). The read-only chat (F1):
same feed, asks as fact panels with the identical preview and
`waiting for a writable client · f read the diff`, `⊘ read-only`
where the composer would be, header `chat · read-only · needs owner`,
bare-letter pager keys, `q` back to the fleet — write affordances
absent, not disabled (no interrupt, no composer, no actions).
AnsweredOptimistic collapses the panel to the pending marker;
SendFailed resurfaces with the failure verbatim; remote resolution
dismisses panel and reader on reconcile; synchronous answer refusals
are watched and stated in the panel.

### Changes
- `crates/amux-tui/src/chat/diff.rs` (new, pub): the §4 diff renderer
  — number walk, gutters, wraps, preview budget/remainder, new-file
  blocks; unit-locked layout tables.
- `crates/amux-tui/src/chat/ask_ui.rs` (new): the C2/C3/C4 stage
  machines (AskUi/AskStage/QuestionUi), panel text-field readline.
- `crates/amux-tui/src/chat/panel.rs` (new): docked panel renderer,
  all forms + read-only fact panels.
- `crates/amux-tui/src/chat/reader.rs` (new): ReaderView + the
  fullscreen frame, artifact resolution from the Model, plans nav.
- `chat/{mod,keys,render}.rs`: focus routing (reader → panel →
  composer; read-only pager), the Esc chain's stages 1–2, reconcile
  syncs panel/reader to the ask head (plan reader-first), the bottom
  block replacing FIXED_ROWS (panel/read-only/composer variants, tail
  kept on short viewports), read-only header/working-line, plan
  affordance; `render.rs`: the four diff.* Theme tokens.
- `view.rs`/`run.rs`: `UiAction::CloseChat` (read-only `q`).
- Goldens: 27 new frames/style-maps (permission edit incl. CJK wrap +
  truncation arithmetic, write, bash, fallback, unverified refusal,
  deny feedback, pending marker, send-failed resurfacing, question
  single/tabs/other/review, plan reader/docked/feedback/resolved,
  ask diff reader, new-file reader, post-hoc numbered multi-hunk +
  both-theme style maps, read-only ×3, panel style maps ×2);
  chat_needs_you and chat_tools_edge regenerated (the two surfaces
  Phase 4 explicitly deferred to this phase); every other Phase 4 and
  fleet golden byte-identical. The never-panic sweep grew panel,
  question, plan-reader, docked-plan, and read-only states. 17 new
  key/state-machine unit tests.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean.
- amux-tui 78 lib + 48 chat golden + 19 fleet golden; amux-ui 29 lib
  + 1 runtime + 123 spec; amux --lib 401; amux spec (testnet) 44;
  amux-cli 53 — all under `timeout 600`.

### Next Steps
- Phase 5 report; orchestrator review gate + simplification pass.
- Phase 6 wires entry (fleet bindings, UserAttached on chat open),
  the chrome-wide Ctrl+C guard, and the `?` overlay.

---

## 2026-08-12: Chat V1 Phase 5 — ask artifacts in the layer, menu-shape refusal, readonly attach subscription

### Summary
The Model half of Phase 5 (ask panels/reader/read-only). Permission asks
now carry a typed body artifact computed once in the fold at ask
creation: `claude::artifact` defines the jsdiff-shaped hunk model
(`DiffArtifact`/`DiffHunk`, `DiffNumbering::{Absolute,None}`,
`DiffMagnitude::{Fact,Estimated,ReplacesEveryOccurrence}`) and
`ask_time_diff` — the ONE computed diff in the design (diff-rendering
§1.4: at ask time the transcript states no diff; everything post-hoc
restates `structuredPatch`). Edit asks get a numberless,
estimated-magnitude mini-diff via `similar` (line-level, context 3,
the jsdiff convention); `replace_all` states "replaces every
occurrence" instead of counts; Write asks retain their proposed
`content` (`AskArtifact::NewFile` — create-vs-overwrite unknowable
pre-run, the artifact claims neither); other tools carry `None`. The
artifact lives ON the ask (evict bytes, never obligations) so panel
and reader render it and never compute. `encoding::menu_shape_refusal`
exports the panel's read-only gate — the same one-suggestion check the
permission program table enforces, so a panel can never offer an
action the encoder would refuse. And `Msg::UserAttached` now
subscribes readonly agents' streams too (`StreamWanted::UserRequested`
vs the eager `InventoryPolicy` skip): opening a read-only chat (F1) IS
the interaction the subscription policy widens for; inventory alone
still opens nothing for them.

### Changes
- `crates/amux-ui/src/claude/artifact.rs` (new): hunk model + the
  ask-time producer, unit tests (grouping, magnitude, replace_all).
- `crates/amux-ui/src/claude/{mod,fold}.rs`: `Ask.artifact` field,
  computed in `fold_permission_request`; transcript-only fallback asks
  (question/plan) carry none.
- `crates/amux-ui/src/claude/encoding.rs`: `menu_shape_refusal` +
  `unverified_permission_menu` shared with `permission_program`.
- `crates/amux-ui/src/update.rs`: `ensure_stream` takes
  `StreamWanted`; readonly skip applies to inventory policy only.
- Workspace/amux-ui manifests: `similar = "2.7"`.
- Spec: asks chapter grows the artifact cases (+ differential sequence
  `asks::edit_artifact`); attention chapter grows the readonly-attach
  case (+ sequence `attention::readonly_attach`); inventory's
  readonly comment updated (its assertion unchanged).

### Verification
- `timeout 600 cargo test -p amux-ui`: 28 lib + 1 runtime + 123 spec.
- `cargo clippy -p amux-ui --all-targets -- -D warnings` clean; fmt.

### Next Steps
- The TUI half: docked panels, the reader, read-only chat (this
  phase's remaining chunks).

---

## 2026-08-12: Chat V1 Phase 4 — simplification pass

### Summary
The Phase 4 diff re-read for altitude and dead surface; every change is
behavior-preserving (all goldens byte-identical, no regeneration). The
chat layout's clamp arithmetic now reads as one rule: `FIXED_ROWS`
(3 above the feed + 4 below) and a free `extra_rows(working, paused)`
feed both the composer budget in `layout()` and a single
`feed_height_for(paused)` — previously the same sums lived in three
places as `3 + … + 4`, `7 + extra`, and a duplicated inline `extra`.
`composer_width` was `text_width` under a second name (composer and
feed text share TEXT_COL) — merged. The chat's `finished_blank`
duplicated the chrome's `blank_line` that this phase had already made
pub(crate) — the chrome helper now serves both screens. In markdown,
the two copies of the close-row-and-hang-indent step in `wrap_runs`
collapsed into one nested `break_row`; `find` lost its slice-of-one-
char generality; `wrap_runs` went private (no callers outside the
module). `kill_to_line_end` had hand-expanded `kill_range`'s body
(minus a no-op cursor self-assign) — routed through the one kill
primitive. Thin wrappers `plain_text_rows`/`message_lines` inlined to
match the direct `markdown::plain_rows` idiom of every other call
site; the unexercised root re-export of `handle_chat_key` dropped
(`chat::handle_chat_key` remains the path). Deliberately untouched:
`ViewState::open_chat`/`close_chat` (the documented Phase 6 seam), the
chat/fleet renderer split (their line-building needs genuinely
differ), and every test and golden.

### Changes
- `crates/amux-tui/src/chat/render.rs`: `FIXED_ROWS` + `extra_rows` +
  `feed_height_for`; `composer_width`→`text_width`;
  `finished_blank`→`crate::render::blank_line`; inlined
  `plain_text_rows`/`message_lines`.
- `crates/amux-tui/src/chat/markdown.rs`: `break_row` extraction;
  single-char `find`; `wrap_runs` private.
- `crates/amux-tui/src/chat/composer.rs`: `kill_to_line_end` via
  `kill_range`.
- `crates/amux-tui/src/lib.rs`: root re-export trimmed to `ChatView`.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean; amux-tui 51 lib + 21 chat golden + 19 fleet
  golden with every golden byte-identical; amux-ui, amux --lib, amux
  spec (testnet), and amux-cli suites green.

---

## 2026-08-12: Chat V1 Phase 4 — codex review fixes

### Summary
The four codex findings on the Phase 4 diff, all accepted. (1) A
command the reducer refuses SYNCHRONOUSLY (whitespace-only prompt,
disconnected fail-fast) finished before the run loop's reconcile ever
ran — the cleared draft could never resurface; the loop now reconciles
immediately after dispatch. (2) The paste deferral was overridden by
review: with bracketed paste disabled, a pasted CR arrived as Enter and
submitted a partial prompt — actively harmful. Bracketed paste joins
the terminal-hygiene set (enabled with the chrome, restored on every
exit path incl. the signal handler's locked byte string), and
`Event::Paste` inserts literally into the draft: CRLF/CR→LF, tabs
expand to four spaces at insertion (mirroring the reader's
tabs-expand-before-width-math policy, keeping the draft sendable past
the C6 control-byte validator), all other control bytes stripped
(invisible AND unsendable — a trap in both directions). (3) Composer
growth is clamped to the viewport's static-row budget, so the footer
and bottom border survive every height ≥ the (raised) minimum; the
never-panics sweep now asserts both at every size. (4) Wrapping and
border math measure display cells, not codepoints: `str_width`/
`clip_to_width` (unicode-width 0.2 + unicode-segmentation — the exact
versions ratatui renders with) drive markdown wrap, hard wrap, composer
rows, grouped-join fits, and `finish_line`'s clip/pad, so CJK/emoji
never displace the right border and combining marks never widen a row.

### Changes
- `crates/amux-tui/src/run.rs`: reconcile after dispatch;
  `Event::Paste` routes into the chat composer.
- `crates/amux-tui/src/terminal.rs`: Enable/DisableBracketedPaste in
  `write_enter_chrome`/`write_restore`; `RESTORE_BYTES` extended (the
  lockstep unit test still guards it).
- `crates/amux-tui/src/chat/composer.rs`: `paste()` with
  normalization/expansion/stripping + five tests.
- `crates/amux-tui/src/chat/keys.rs`: `handle_chat_paste`; tests for
  the synchronous-refusal resurface and paste dismissal.
- `crates/amux-tui/src/render.rs`: `str_width`/`clip_to_width`;
  `line_len`/`finish_line` measure and clip by cells (re-padding after
  a wide-grapheme clip so the border cannot drift).
- `crates/amux-tui/src/chat/{markdown,render}.rs`: width-based wrap
  paths; composer budget clamp; MIN_HEIGHT 10.
- `crates/amux-tui/tests/chat_golden.rs` + `tests/golden/
  chat_unicode.txt`: CJK/emoji/combining-marks frame with per-row
  border-cell assertions; the viewport sweep asserts frame height,
  footer, and bottom border at every viable size.

### Verification
- amux-tui 51 lib + 21 chat golden + 19 fleet golden; every
  pre-existing golden byte-identical. fmt clean; workspace clippy
  `-D warnings` (`--features amux/testnet`) clean; amux-ui, amux
  --lib, amux spec (testnet), amux-cli suites green.

---

## 2026-08-12: Chat V1 Phase 4 — the chat screen: feed + composer

### Summary
The chat screen lands in amux-tui as a screen within the existing
chrome (`docs/CHAT.md` §Wireframes normative): a new `chat` module with
the feed renderer, the multiline readline composer, scroll/follow
ViewState, and the D5 working line — all wired to Phase 1–3's Model
surface (`ClaudeLayer` entries/echoes/session facts,
`Model::claude_phase`, `claude_send_gate`, `claude_mode_cycle_gate`,
`Command::{SendPrompt, Interrupt, CyclePermissionMode}`). Rendering
stays a pure function of (Model, ViewState, FrameContext); every
derivation (phase, gate, magnitudes, counts) comes from the Model and
the views format only. The screen is reachable in-code via
`ViewState::open_chat` — the seam Phase 6's fleet bindings will invoke;
no fleet binding changes here.

### Changes
- `crates/amux-tui/src/chat/{mod,composer,markdown,render,keys}.rs`:
  the chat screen. Feed rendering for every Phase 1 entry kind
  (terminal markdown per B2 — headings bold keeping `#`, fenced code,
  lists, inline emphasis/code, preformatted tables, URL-aware wrap;
  tool lines with FACT magnitudes and dim `└` continuations; grouped
  read/search one-liners; thinking/turn/compaction markers; the
  truncation boundary; loading-vs-empty; optimistic echo with
  `sending…`; stated send failures), sticky-bottom scroll with the
  paused rule (`↓ following paused · N new entries · pgdn to resume` +
  position %), the 1–6 row auto-growing composer with the full
  readline set and single-slot kill buffer, Enter gated by
  `claude_send_gate` (draft kept, footer states the gate), Ctrl+X
  interrupt in every state, Shift+Tab mode cycle, the Esc chain with
  Phase 5's stages slotted, and the working line driven by the one
  1 Hz tick.
- `crates/amux-tui/src/render.rs`: `Theme` grew Dark/Light variants
  with semantic tokens (text/muted/emphasis/code/ok/warn/error); the
  fleet keeps its fixed styles (byte-identical goldens); line helpers
  opened pub(crate) for the chat module.
- `crates/amux-tui/src/{view,run,lib}.rs`: `ViewState.chat` +
  `open_chat`/`close_chat` seam; run-loop routing (chat owns keys when
  open — its Ctrl+C is clear-as-kill, never quit), dispatch op
  feedback for C5 failure resurfacing, tick gating extended to the
  chat working line.
- `crates/amux-tui/tests/chat_golden.rs` + `tests/golden/chat_*`: 20
  Tier-3 tests — idle/working/scrolled-back/truncated/loading/empty/
  echo/failure/needs-you/markdown/entry-edges/tool-edges frames, style
  maps for both themes, layout-equality across themes, stability, and
  a never-panics viewport sweep.

### Decisions Made
- Working-line token count omitted: the layer folds no usage facts
  yet (D5 words it "when cheaply available"); recorded for a later
  phase rather than half-derived in the view.
- Grouped tool entries join with ` · ` only when they fit the line;
  otherwise they fall back to their own row — grouping compresses,
  never clips.
- Footer hints tell the truth (P10): `? help` and `end newest` from
  the wireframe footers are absent until Phase 6 lands the overlay and
  the ext-tier jumps; the scrolled hint says `pgup/pgdn scroll · esc
  newest` (empty draft only).
- Ctrl+C on an empty draft is a deliberate no-op with a comment naming
  Phase 6's chrome-wide two-press guard; a single ^C never quits.

### Verification
- `cargo fmt`; amux-tui: 44 lib + 19 fleet-golden + 20 chat-golden
  tests green; fleet goldens byte-identical; workspace clippy clean.
- Full gate (amux-ui suite, amux lib, amux spec under testnet) run at
  phase close.

### Next Steps
- Phase 5: ask panels docked at the composer, the reader, read-only
  chats — the Esc chain's stages 1–2 slots are marked in
  `chat/keys.rs::esc_chain`.
- Phase 6: fleet entry bindings via `ViewState::open_chat`, chrome-wide
  guarded Ctrl+C, `?` overlay, kitty detection (Shift+Enter sugar).

---

## 2026-08-12: Phase 3 simplification pass

### Summary
The phase-gate simplification sweep over the Phase 3 diff. Three small
cuts, no behavior change: (1) dead surface — `STALE_INPUT_ERROR` was
re-exported from `amux_ui` but nothing outside `runtime.rs` uses it;
the re-export is gone and the const is module-private. (2) noise — six
`command.clone()` calls on `update.rs` refusal paths were redundant
(NLL lets the command move on every early-return path); refusals now
consume the command plainly. (3) copy-paste drift — the three new
question capture scenarios (`question_tabs`, `question_other_single`,
`question_mixed`) carried verbatim copies of the wait-for-answers /
press-Enter-for-a-lingering-submit-step loop; it is now one
`confirm_question_submit` helper returning the recorded
`extra_submit_steps`, leaving each scenario's documented keystroke
program in-line. Reviewed and deliberately left alone: the refusal
plumbing (already ONE mechanism — `refuse` plus typed `EncodingError`
rendered to stated messages, not parallel copies), the `InputDispatch`
parameter object (four callers; keeps `retry_stale` named at call
sites), `PromptEcho.at` / `SuggestionFact.directories` (fold facts with
a stated Phase 4 consumer), the encoding fns' `pub` visibility (the
module is the documented seam), and every spec chapter.

### Changes
- `crates/amux-ui/src/lib.rs`, `runtime.rs`: drop the unused
  `STALE_INPUT_ERROR` export; const is now private to the shell.
- `crates/amux-ui/src/update.rs`: refusal paths move `command` instead
  of cloning it.
- `crates/amux/tests/capture/main.rs`: shared
  `confirm_question_submit` for the three Phase 3 question scenarios
  (identical messages and retry bounds preserved).

### Verification
- fmt; workspace clippy `--all-targets --features amux/testnet`
  `-D warnings` clean; `timeout 600 cargo test -p amux-ui` (25 lib +
  119 spec + 1), `-p amux --lib` (401), `-p amux --features testnet
  --test spec` (44), `-p amux-tui` (22) all green. No spec assertion
  changed; no fixture touched.

---

## 2026-08-12: Chat V1 Phase 3 — codex review fixes

### Summary
Two accepted findings, both with locking spec cases. [P1] `AnswerAsk`
addressed the whole queue while claude's remote menu only ever displays
the HEAD: an answer for a later queued ask would encode that ask's
digits and apply them to the head's menu — potentially approving the
wrong tool — while marking the later ask optimistic. The reducer now
requires the target to equal `ask_head()`; a non-head target refuses
with a typed outcome ("ask is queued behind the current menu — answer
the head ask first"), no bytes, no state touched. [P2] Free text wrapped
in bracketed paste could carry a literal `ESC[201~` terminator: the
paste would close early and the remainder run as LIVE keystrokes in the
remote session (injection; also an echo desync wedging SendInFlight).
Decision: **rejection over neutralization** — the verified transcript-
transparency claim is printable + `\n` exactly (`prompt_multiline`), so
stripping/splitting ESC would assert reassembly knowledge no capture
confirms (the same honesty rule as unverified menu shapes). Every
control character except `\n` now refuses with the new typed
`EncodingError::ControlBytesUnsupported`, on EVERY free-text path — the
sweep covered the prompt paste, the deny-feedback paste (shared
`paste_block` helper), and the raw menu fields (Other text single- and
multi-select, plan request-changes feedback — all now through
`menu_text`, where an ESC would navigate the menu instead of typing).

### Changes
- crates/amux-ui/src/update.rs — the head-only answer guard.
- crates/amux-ui/src/msg.rs — AnswerAsk doc states the head rule.
- crates/amux-ui/src/claude/encoding.rs — `ControlBytesUnsupported`,
  `reject_control` + `paste_block`, menu_text sweep, plan feedback
  routed through menu_text; table test
  `control_bytes_in_free_text_refuse_on_every_path`.
- crates/amux-ui/tests/spec/write.rs — locking cases
  `an_answer_addressed_past_the_head_refuses_without_bytes` (two queued
  asks: non-head refusal, head answers normally; differential sequence
  `write::head_guard`) and
  `a_prompt_carrying_the_paste_terminator_is_refused` (no effect, no
  echo, gate stays Ready).

### Verification
- fmt clean; workspace clippy `-D warnings` clean.
- amux-ui 25 lib + 1 runtime + 119 spec; amux --lib 401; amux spec 44
  (testnet); amux-tui 3 + 19 — all green.

## 2026-08-12: Chat V1 Phase 3 — the write path (C6 module, Commands, C5 lifecycle)

### Summary
The chat write path lands: typed intents become keystroke programs
injected under the seq guard, with the C5 optimistic lifecycle in the
Model. One encoding module (`amux-ui/src/claude/encoding.rs`) owns every
Claude key byte in the workspace — typed intent in, `KeyStep` program
out, each table documented with its capture provenance (claude 2.1.228),
and any menu shape no capture confirmed returns a typed
`UnverifiedMenuShape` error instead of guessed bytes (the permission
menu is generated from the hook's `permission_suggestions`, so its digit
table is verified for the one-suggestion shape only). Four new Commands
(`SendPrompt`, `AnswerAsk`, `Interrupt`, `CyclePermissionMode`) dispatch
with OpIds and finish as state; the reducer encodes, gates (D2 via
`Model::claude_send_gate`, D4 via the mode-cycle gate, D3 ungated),
flips the optimistic state (prompt echoes with content-equality
reconciliation; `AskState::AnsweredOptimistic{op, answer}` carrying the
pending-marker data), and emits `Effect::SendInput` with the layer's
stream cursor as `expected_seq`. The shell maps steps onto the
`claude_pty_transcript_v1` actions and always answers — a stale-seq
interrupt (position-independent by design) retries mechanically with
the seq the refusal reported; everything else fails fast and resurfaces
with the failure stated. Remote resolution wins over local pending
state everywhere; readonly rejection is the server's and the model
states it. Hook payloads' `permission_mode` now folds into the session
facts (the D4 verdict made code).

### Changes
- crates/amux-ui/src/claude/encoding.rs — the C6 module + table tests.
- crates/amux-ui/src/claude/{mod,fold}.rs — ask `suggestions` facts,
  prompt echoes, seq cursor, hook permission_mode fold, ask-state
  mutators, echo invariant.
- crates/amux-ui/src/{msg,effect,update,model,runtime,lib}.rs — the
  Command/OpOutcome/Effect vocabulary, gates, dispatch, op-result
  resurfacing, `SendGate`, the shell executor.
- crates/amux-ui/tests/spec/write.rs — Chapter 13 (13 cases, 5
  differential sequences); wire_free serde variants extended; asks
  chapter asserts the suggestion facts.
- crates/amux-tui/src/render.rs — status-line verbs for the new
  commands (no chat rendering — that is Phase 4).

### Decisions Made
- Prompts always inject as bracketed paste: literal text (no `/`/`!`
  grammar triggers), newline-safe, and the transcript row lands
  byte-identical — which makes content equality the echo reconciliation
  key (verified).
- One optimistic echo in flight at a time (`SendGate::SendInFlight`):
  reconciliation stays unambiguous; claude queues raced sends anyway.
- Deny-with-feedback is ONE program (digit, settle, pasted feedback as
  a follow-up prompt) — the permission menu has no feedback field
  (verified; the plan menu's request-changes does and keeps its field
  encoding).
- `Effect::SendInput.retry_stale` carries the reducer's policy; the
  shell retries mechanically (bounded) with the server-reported seq —
  interrupt only.
- Stale-seq refusals are stated, not jargoned: the shell maps an
  unretried/exhausted `SequenceNumberMismatch` to `STALE_INPUT_ERROR`
  ("input raced the session — it moved on before the keys landed")
  with the technical detail in parentheses; `EncodingError` carries no
  serde — a refusal is a finished-op message, never Msg traffic.

### Verification
- fmt; workspace clippy `-D warnings` clean.
- amux-ui: 24 lib + 1 runtime + 117 spec (differential wraps the new
  sequences; fold==live after every Msg). amux --lib 401; amux spec 44
  (testnet); amux-tui 3+19 green.

## 2026-08-12: Chat V1 Phase 3 — live keystroke verification (C6 research)

### Summary
The write path's core risk retired first: every keystroke encoding the C6
module will state was driven against a REAL claude (2.1.228, haiku) via
six new capture-harness scenarios, and the open D4 question is answered.
Verdicts: a mid-session Shift+Tab cycle emits NO `permission-mode` row
(three presses, two turns, zero rows — the hook payloads' 
`permission_mode` is the live mode source; cycle order default →
acceptEdits → plan → default); plan approve-AUTO (digit 1, the owed H.5
sub-capture) does not flip the row either while the effective mode
becomes acceptEdits and edits land ask-free; the permission menu is
generated from the hook's `permission_suggestions` (1 Yes / one digit per
suggestion / last No) and its deny digit denies IMMEDIATELY with the
interrupt artifacts — no feedback field, so deny-with-feedback composes
as digit + follow-up prompt; question digits SELECT-and-advance (a
single single-select form submits on selection; multi-question/multi-
select forms end on a review step where one Enter submits — claude's own
form implements C4's mandatory-submit rule); the single-select Other is
digit + type + Enter; mixed forms compose with Tab advancing off the
multi-select question; and multiline prompts inject via bracketed paste
with the transcript row byte-identical to the sent text (B1's echo
correlation is content equality). Eight redacted fixtures graduated
(leak sweep clean); semantics recorded in transcript-semantics.md §18d.

### Changes
- crates/amux/tests/capture/main.rs — scenarios permission_session,
  permission_deny_feedback, question_tabs, question_other_single,
  question_mixed, plan_auto, mode_cycle, prompt_multiline.
- crates/amux/tests/fixtures/chat-v1/ — 8 new fixture pairs + README.
- notes/chat-v1/transcript-semantics.md — §18d; §10 permission-mode row
  entry resolved.

### Decisions Made
- Deny encoding: the last menu digit, never Esc — Esc-deny and digit-deny
  differ (both interrupt-close the turn on the plain permission menu, but
  the digit is the labeled option C2 requires; the plan menu's digit 3
  keeps the turn alive with a feedback field).
- The question_tabs pre-run that CANCELLED the form (digit+Enter surplus
  keys walked the review onto Cancel) is kept as negative evidence in the
  phase report — digits auto-advance, so per-question Enters are wrong.

### Verification
- All 8 scenarios green against claude 2.1.228 on haiku (no sonnet
  needed); every run under the auto-update guards on a poisoned scratch
  daemon (the spawn-seam scrub live-proved again).
- Fixture leak sweep: username/home/hostname/scratch/mcp/credential
  patterns × 17 fixture pairs = 0 hits.

## 2026-08-12: Phase 2 gate — simplification pass

### Summary
The Phase 2 simplification pass over the ask/phase build. Three small
cuts, no behavior change: `structured_type()` in the core hook seam was
a single-caller helper whose `None` arm was unreachable at its call
site (the internal-only kinds are matched above it), leaving a dead
`else { return }` — the emitted kinds now name their `type` tag
directly in `handle_hook`'s one flat match; `fold.rs` carried the
FNV-1a loop twice (`content_key`, `ask_key`), now one `fnv1a` helper
with identical hash output (the constants stay explicit because ask
keys are serialized model state); two doc comments in `model.rs` still
said "summarizer" after the E2 deletion. Reviewed and deliberately
left: the phase()/attention() parallel derivations (attention is not a
function of ChatPhase — Authority-close and the stop pre-signal map to
NeedsYou(Finished) on the badge but Idle on the phase, so the mirrored
precedence IS the one story), `observe_exit`'s field clearing beside
the `exited` early-return (accessors like `ask_count()` are
spec-asserted post-exit), the `AskState` variants only Phase 3
constructs (documented shape-ahead), and the capture harness's
`env_map`/assert split (the guard's point is checking exactly what the
daemon receives).

### Changes
- crates/amux/src/agents/claude/session/hooks.rs — helper inlined.
- crates/amux-ui/src/claude/fold.rs — one FNV-1a implementation.
- crates/amux-ui/src/model.rs — stale summarizer wording in two docs.

### Verification
- cargo fmt; workspace clippy `-D warnings` clean.
- amux-ui 104-spec suite, amux --lib (401), amux spec (44, testnet),
  amux-tui (19 + goldens): all green, no assertion touched.

---

## 2026-08-12: Phase 3 gate — spec corrections from the write path

### Summary
docs/CHAT.md absorbs the Phase 3 live-verified corrections: D4
resolved (mid-session cycling emits NO `permission-mode` row; hook
payloads' `permission_mode` is the live source; cycle order default →
acceptEdits → plan → default), H.5 resolved (plan menu 1/2/3;
approve-auto never flips the row either — effective mode via hook
facts), C2 gains the suggestion-generated menu reality (option
labels derive from `permission_suggestions`; unverified menu shapes
refuse typed; deny is immediate with feedback as a composed
follow-up prompt), B5's `command_permissions` claim narrowed to
command-rule grants, C6 cites the encoding module and its refusal
rule, C4 notes claude's appended "Chat about this" option. Gate
context: codex review returned a P1 (answers could bind past the
queue head and approve the wrong permission — now head-only with a
typed refusal) and a P2 (bracketed-paste terminator injection —
resolved by rejecting control bytes on every free-text path,
rejection chosen over neutralization per the honesty rule); both
fixed with locking spec cases (`a637fa8`).

### Changes
- docs/CHAT.md — six corrections, each evidence-tagged "Phase 3".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Phase 2 gate — spec corrections from the ask/phase build

### Summary
docs/CHAT.md absorbs the Phase 2 fixture-grounded corrections:
plan request-changes spawns a NEW ask without ending the turn
(rejection carries `toolDenialKind:"user-rejected"`); permission-ask
correlation is content identity (hooks carry no tool_use id;
`tool_name` + byte-equal `tool_input` is the join); the phase decay/
staleness thresholds are named constants (60 s idle decay, 600 s
working cap) so E2E can assert them; hook delivery is documented
at-least-once with core-side dedupe; the fleet's errored→Unknown
mapping is design, not omission. Gate context: codex review returned
one P1 (truncated live windows stuck Replaying forever for
late-attaching clients) and two P2s — all fixed with locking spec
cases (`9d1df25`) — plus capture-env hardening after the Phase 0
installer incident (the owner's `~/.local/bin/claude` symlink was
repointed into a capture temp dir; restored to the durable install,
and the harness now hard-locks claude's updater in the spawn env).

### Changes
- docs/CHAT.md — five corrections, each evidence-tagged "Phase 2".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Phase 1 gate — simplification pass

### Summary
The Phase 1 simplification pass over the Claude layer. One real seam:
the review fixes had left two copies of the message-closure machinery
(`close_open_messages` with a hidden targeted-fallback mode, plus the
bolted-on `close_others_as_abandoned`), each duplicating the
close-and-retag loop. Both collapse into one `close_messages(layer,
closure, selects)` whose slot-selection rule now reads at each call
site — the interrupt's pair-by-`interruptedMessageId`-else-all-open
fallback moved to `fold_interrupt`, where §17 states it. Two
micro-cleanups: the thinking arm binds its already-matched block type
instead of re-reading the discriminator, and `retain_plan`'s fallback
flattens to `find` + `?`. Behavior frozen: no spec chapter changed.
Left alone deliberately: `prompt_at()`/`session_id()` (documented
D5/header read surfaces), the `with_claude_summarizer`/
`with_claude_layer` gate duplication (the summarizer is deleted by
Phase 2's unification), and `entry_kind`/`entry_kind_mut` (mut/non-mut
pair documenting the eviction-tolerant read).

### Changes
- crates/amux-ui/src/claude/fold.rs — closure machinery unified; net −8
  lines with the rules stated at the call sites.

### Verification
- cargo fmt + clippy `-D warnings` clean; `timeout 600 cargo test -p
  amux-ui` (69 spec + unit tests) and `timeout 600 cargo test -p amux
  --features testnet --test spec` (44) all green, assertions untouched.

---

## 2026-08-12: Chat V1 Phase 2 — codex review fixes + capture hardening

### Summary
Three accepted codex findings plus one incident hardening item.

[P1] **Truncated live windows never unlocked.** A long-running session
writes past the bounded source tail, so a late attacher's truncated
window no longer CONTAINS the `amux.transcript_ready` marker — the layer
reported Replaying (and attention Unknown) forever, suppressing live
prompts and permission hooks on the most common real-world attach.
`StreamMsg::ReplayComplete` is now the out-of-band unlock: the layer
records `replay_complete`, and liveness is `ready-marker OR
replay-complete`. Truncation honesty (B9's boundary) is unchanged; a
relink resets the latch and is unlocked by the new file's own fresh
marker (B10's loading band survives). Locked by
`phase::a_truncated_live_window_unlocks_after_replay_complete`
(mid-replay Replaying → unlock → tail's Working surfaces → live
permission hook surfaces on phase AND badge, history_truncated stays).

[P2] **Stale badge beside a stale-free label.** After the 600 s cap,
`effective_attention` degraded to Unknown while `status_label_for`
still rendered the cached "working". One derivation now: the status
word derives from the same effective attention as the badge
(`AgentCard::status_label` takes the effective attention and is
crate-private; `Model::status_label_for` is the only entry). Locked by
`attention::stale_working_degrades_the_fleet_badge_and_label_together`.

[P2] **Exit on a truncated window fell to Unknown.** `observe_exit`
cleared activity but left `truncated_start`, so the phase degraded
instead of settling. Orderly termination is now a recorded FACT
(`exited`) that overrides truncation and staleness: phase settles to
`Idle{Fact}` (choice recorded — the vocabulary has no exited phase; the
card's `AgentPhase::Exited` carries the exit itself), attention to
Idle. Locked by the extended
`phase::agent_exit_closes_asks_and_settles_the_phase` (plus a
truncated-window variant).

[Hardening] **The capture harness can no longer let claude's installer
touch the owner's real launcher.** During Phase 0 captures the
auto-updater downloaded into the scratch XDG_DATA_HOME and REPOINTED
`~/.local/bin/claude` at the temp dir (the launcher path is
`join(homedir(), ".local/bin/claude")` in the 2.1.228 binary — not
env-overridable — and the harness must keep the real HOME for keychain
auth). The harness env now carries `DISABLE_AUTOUPDATER=1`,
`DISABLE_UPDATES=1` (the hard lock — `claude update` itself refuses),
and `DISABLE_INSTALLATION_CHECKS=1`, all three verified against the
2.1.228 binary's env registry; env construction moved to a pure map
with the guard asserted on every capture run (the capture binary has no
test harness; Phase 7's scheduled capture validates live). No new
captures were run.

### Changes
- crates/amux-ui/src/claude/mod.rs — `replay_complete` + `exited`
  facts, `observe_replay_complete`, `live()` gate in phase/attention.
- crates/amux-ui/src/update.rs — ReplayComplete reaches the layer.
- crates/amux-ui/src/model.rs — label derives from effective attention.
- crates/amux-ui/src/claude/fold.rs — the relink reset re-stamps the
  arrival clock the triggering row set.
- crates/amux-ui/tests/spec/{phase,attention,feed_replay,inventory,
  sessions}.rs — locking cases; the mid-replay and relink sequences
  restated so ReplayComplete sits where reality puts it (after replay
  batches).
- crates/amux/tests/capture/harness.rs — update guards + assertion.

### Verification
- fmt + workspace clippy `-D warnings` clean; `timeout 600 cargo test
  -p amux-ui` (11 + 1 + 104 spec), `-p amux --lib` (401), amux spec
  suite (44), `-p amux-tui` (3 + 19 goldens) all green.

---

## 2026-08-12: Chat V1 Phase 2 — one fold: the summarizer unification (E2)

### Summary
The duplicate interpretation dies. `summarizers/claude.rs` — the
separate attention fold with its KNOWN-FRAGILE notification-wording
split (which the plan-approval notification defeats: "needs your
approval", no "permission" substring) — is deleted; kernel attention is
now a pure projection of the SAME Claude-layer state the chat phase
derives from: `ClaudeLayer::attention()`, cached on the card by the one
`with_claude_layer` gate (the `with_claude_summarizer` duplication dies
with it). The kernel Attention vocabulary is unchanged; the
AttentionMismatch invariant now checks card-vs-layer. Fleet gains: the
badge routes asks on `hook.permission_request.tool_name` (plan review
is `!`, never `?`), `turn_duration` now closes Working → Finished (the
old fold left tool-denial turns stuck Working forever — it treated
system rows as weak and the deny turn has no hook.stop), an interrupt
settles to Idle instead of Working, an API error degrades to Unknown
instead of lying Working, and the E1 staleness cap applies to the badge
at read time (`effective_attention`), keeping fleet and chat in
agreement (E3). Chapter 5 is rewritten onto the chat fixtures — its
three authored summarizer-era fixture JSONs are deleted — and closes
with the unification property: on every fixture, folded row by row,
fleet needs-you equals chat-phase needs-you.

### Changes
- crates/amux-ui/src/summarizers/ — deleted (claude.rs + mod.rs).
- crates/amux-ui/src/update.rs — one gate; attention refreshed from the
  layer after every fold step.
- crates/amux-ui/src/model.rs — `AgentCard.summarizer` removed;
  AttentionMismatch retargeted (card vs layer-derived);
  `effective_attention` staleness degrade.
- crates/amux-ui/src/lib.rs — `SummarizerState` unexported.
- crates/amux-ui/tests/spec/attention.rs — rewritten for the unified
  fold; authored fixtures claude_permission_flow/stop_and_notification/
  truncated_tail.json deleted.
- crates/amux-tui/tests/golden.rs — fixture rows restated in the
  unified fold's facts (ready marker, human-prompt turn starts, the
  question as an AskUserQuestion permission_request). Every golden
  FRAME is byte-identical — the badges' meaning survived the fold swap,
  which is exactly the E3 guarantee.

### Decisions Made
- Attention stays TIME-FREE on the card (cache-vs-fold invariant holds
  between Ticks); read-time policy (host offline, working staleness)
  lives in `Model::effective_attention` — computed once for every
  renderer.
- Errored maps to Attention::Unknown: the kernel vocabulary cannot say
  "errored", retries run invisibly, and Working/Idle would be wrong
  badges. The chat phase carries the errored FACT.
- Interrupt-closed turns map to Idle (the user closed it deliberately);
  authority/presignal-closed turns keep NeedsYou(Finished).

### Verification
- `timeout 600 cargo test -p amux-ui`: 11 lib + 1 runtime + 103 spec
  green (equivalence property sweeps all 9 fixtures row-by-row);
  workspace clippy `-D warnings` clean; amux-tui, amux lib, and the
  amux spec suite green (phase gate).

---

## 2026-08-12: Chat V1 Phase 2 — the ask model and the phase derivation

### Summary
The Claude layer grows the ASK model and the E1 phase derivation
(docs/CHAT.md §Asks, §Phase and attention). An `Ask` is `{id, kind,
payload}` queued in arrival order with an honest head + count: the
pending signal is the `hook.permission_request` row routed on
`tool_name` (AskUserQuestion → question kind; ExitPlanMode → the
plan-review permission variant carrying the plan; anything else → plain
permission with the SAME typed per-tool payload tool entries use — one
extraction, both consumers), with the unpaired
AskUserQuestion/ExitPlanMode in a final message as the transcript-only
fallback. Hook payloads carry NO tool_use id (fixture-verified), so
correlation rides content identity — the hook's `tool_input` equals the
transcript `tool_use.input` byte-for-byte on every fixture. Resolution
is the B5 facts (non-error result / typed denial / answers / plan
approval), plus interrupt-closes-ask, turn-end closers for lagging
resolutions, and human-turn-start as the stale guard. The C5
answered-optimistic and send-failed states exist structurally; Phase 3
drives them with Commands. Phase: `ClaudeLayer::phase(now)` +
`Model::claude_phase` implement the E1 table row by row — replaying /
working / idle / needs-you(permission|question) / errored / unknown,
each value tagged fact vs inferred, working staleness-capped into
Unknown (600 s), idle decaying FACT→INFERRED (60 s), truncated blind
windows Unknown, transport loss Unknown-with-obligations-kept.

Hook rows now dedupe by bounded content hash in the fold (the same
window unknown rows use): historical streams and replays carry every
hook row twice (§18b), and without the dedupe a shrink re-replay would
resurrect a long-resolved ask as pending — the re-replay spec case locks
this on the permission fixture. Recorded trade-off: a byte-identical
genuine re-request within the window folds once (payloads carry
prompt_id and tool_input, so distinct events collide only when truly
identical).

### Changes
- crates/amux-ui/src/claude/mod.rs — Ask/AskKind/AskState/AskWhy,
  ChatPhase/PhaseTag, TurnState activity signals, asks queue +
  invariants (`claude-ask-order` + asks retention bound, firing tests).
- crates/amux-ui/src/claude/fold.rs — ask extraction, correlation,
  resolution and closers; hook-row content dedupe; error_live; turn
  open/close bookkeeping; `observe` gains the batch arrival time (the
  staleness clock enters through Msgs).
- crates/amux-ui/src/model.rs — `Model::claude_phase`, the
  `ClaudeAskOrder` violation class.
- crates/amux-ui/src/update.rs — arrival time threaded; stream close
  wires `observe_exit` (asks die with the process) and `invalidate`
  (stale → Unknown, obligations kept).
- crates/amux-ui/tests/spec/asks.rs (Chapter 11) and phase.rs (Chapter
  12); harness `chat_feed_prefix`; feed_replay's re-replay case
  extended to the hook-carrying permission fixture. 99 spec tests, all
  sequences registered into the differential.

### Decisions Made
- No new Msg kinds (recorder/flow-class coverage by construction).
- One extraction for ask payloads and tool entries: `AskKind` wraps
  `ToolInvocation`, so plan review is literally a permission whose
  invocation is the Plan variant (CHAT.md: not a third kind).
- Ask identity is per request content, not per row or per prompt_id —
  plan_reject proves a revised plan re-ask shares prompt_id.
- Turn-end signals (`turn_duration`, non-active `hook.stop`) close
  pending asks: an ended turn is incompatible with a blocking ask; this
  is the catch-up path when another client answered and the resolving
  rows lag.
- The transcript-only plain-permission inference (E1's read-only
  failure mode: unpaired non-question tool in a final message) is
  deliberately NOT implemented — unpaired = running; only
  question/plan tools are self-evident asks. Recorded for Phase 5.
- Staleness caps are policy constants (600 s working, 60 s idle-fact
  decay), applied at read time so the folded state stays time-free.

### Verification
- `timeout 600 cargo test -p amux-ui` — 10 lib + 1 runtime + 99 spec
  green; every new sequence wrapped by the wire_free differential.

---

## 2026-08-12: Dedupe duplicate Claude hook deliveries at the daemon seam

### Summary
Root cause of the Phase 1 "every hook row arrives twice" observation,
found in the transcript's own records rather than theorized: Claude Code
runs EVERY registered hook command per event, and this machine carries
two registrations of the amux hook — a legacy user-scope
`~/.claude/settings.json` entry (`amux-dev hooks claude`, from the
owner's pre-plugin dev setup; no current repo code writes settings.json)
beside the plugin's `amux hooks claude`. The capture transcripts'
`stop_hook_summary` rows record it verbatim: `hookCount: 2, hookInfos:
[{amux-dev hooks claude}, {amux hooks claude}]`. Both processes read the
same stdin JSON and deliver byte-identical payloads to the same daemon,
which wrote each — hence duplicate adjacent `hook.*` rows in every
fixture. Hook delivery is at-least-once by construction (user settings,
project settings, and plugins can all legitimately carry the
registration), so the fix is at the seam where deliveries become stream
facts: `ClaudeSession::handle_hook` now fingerprints each emitted
payload and drops a byte-identical re-delivery within a 2 s window. The
three structured-emission arms collapsed into one in passing. Client
folds still tolerate duplicates (historical streams and replays contain
them) — that lands with the Phase 2 layer work.

### Changes
- crates/amux/src/agents/claude/session/hooks.rs — dedupe window +
  fingerprint, consolidated emission arm, `tokio::time`-paused test
  proving collapse/distinct/past-window behavior.
- crates/amux/src/agents/claude/session/core.rs — `last_emitted_hook`
  suppression state on `ClaudeSession`.
- crates/amux/src/agents/hook.rs (+ two call sites) — boxed
  `ExternalHookBootstrap::Register` (the grown session tripped clippy's
  variant-size lint).

### Decisions Made
- Dedupe in core, not only in client folds: two identical deliveries of
  one event are one fact, and the daemon is the transport seam; a
  2-second window keeps a genuinely identical future event (same
  notification re-fired minutes later) honest. This is transport
  hygiene, not interpretation.
- The legacy settings.json registration itself is owner-machine state,
  not repo code; flagged in the phase report rather than auto-edited
  (amux does not rewrite user Claude config).

### Verification
- `timeout 600 cargo test -p amux --lib` (401), `timeout 600 cargo test
  -p amux --features testnet --test spec` (44), clippy
  `--all-targets --features testnet` clean, fmt clean.

---

## 2026-08-12: Phase 1 gate — spec corrections from the layer build

### Summary
docs/CHAT.md absorbs the Phase 1 fixture-grounded corrections: B3
(tool-denial interrupts ARE followed by `turn_duration`; the inferred
marker reconciles in place), B4 (Write-create carries an empty
`structuredPatch`; magnitude = created line count), B1 (bare
local-command rows render with unstated source, never start a turn),
B7 (task notifications are their own entry — no agent-id key to
correlate), and E2 (notification-wording heuristics are forbidden;
interpretation routes on `hook.permission_request.tool_name` — the
plan-approval notification says "needs your approval", no
"permission" substring). Gate context: codex review returned seven
P2 fold findings — every one an edge-path violation of a stated spec
rule — all fixed with a locking spec-chapter case each (`2df0e8b`).

### Changes
- docs/CHAT.md — five corrections, each evidence-tagged "Phase 1".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Chat V1 Phase 1 — codex review fixes

### Summary
All seven codex findings on the Phase 1 diff accepted and fixed in the
Claude layer fold — each one was the fold breaking a rule docs/CHAT.md
or its own module docs state, and each fix now carries a Tier-1 spec
case documenting the rule: (1) feed-producing rows WITHOUT uuids (the
retained unknown shapes) now dedupe by content hash in the same bounded
window, so a source-shrink re-replay stays idempotent (B10); (2) the
top-level `isMeta` discriminator is filtered before any interrupt/tool/
closure handling, so an array-form meta row can no longer abandon a
message or emit unrecognized entries — meta rows are fully inert in
either content form; (3) `origin.kind:"human"` is the definitive
turn-start discriminator — an unknown `promptSource` label degrades
gracefully instead of silently killing the turn clock; (4) an API-error
row closes any open null-stop message as abandoned before recording the
error (B2); (5) a `turn_duration` row without a readable `durationMs`
degrades to an unrecognized entry — zero is not a fact, and it can no
longer overwrite a better inferred marker; (6) a compaction boundary
invalidates the elapsed prompt base and pending marker reconciliation,
so no duration state crosses it (B3); (7) unknown uuid rows advance the
thinking-duration timestamp chain like every other row.

### Changes
- crates/amux-ui/src/claude/fold.rs — the seven fixes (`remember` /
  `content_key` dedupe helpers, hoisted isMeta filter, turn-start rule,
  API-error closure, malformed-authority degrade, boundary
  invalidation, chain touch in the unknown arm).
- crates/amux-ui/tests/spec/{feed_replay,feed_turns,feed_edges}.rs —
  one spec case per finding (7 new tests, 62 → 69), plus four new
  differential sequences.

### Decisions Made
- No-uuid dedupe strategy: content hash (FNV-1a over canonical JSON,
  domain-tagged) within the SAME bounded seen-window as row uuids —
  identical unknown rows fold once per window, which mirrors what a
  uuid would give them; a distinct-content repeat still folds.
- Malformed `turn_duration` degrades to unrecognized (uninterpreted —
  no turn-state changes) rather than "measured unknown": zero facts
  are never invented, and the row stays visible.
- The isMeta hoist also stops string-form meta rows from closing open
  messages (previously they did): machine-injected rows are not user
  actions, so B2's user-row closure does not apply to them.

### Verification
- fmt + CI clippy clean; `timeout 600 cargo test -p amux-ui` (10 lib +
  1 runtime + 69 spec) green; amux spec suite 44 green; differential
  wraps the new sequences.

---

## 2026-08-12: Chat V1 Phase 1 — the Claude feed-facts layer

### Summary
The typed Claude chat layer (docs/CHAT.md §The feed, B1–B10 + G1) landed
in amux-ui: `claude::ClaudeLayer`, a per-agent child model folding native
`claude_pty_transcript_v1` rows into typed feed entries — prompts,
messages (upsert by `message.id`, dedupe by row `uuid`, finality on
non-null `stop_reason`, abandoned/interrupted closure), thinking/turn/
compaction markers with the FACT-vs-INFERRED tags, tool entries paired by
`tool_use.id` with typed sidecar facts (Edit magnitude, question answers
keyed by question text, subagent launch/completion, plan approval),
status entries (API errors, interruptions), and explicit unrecognized
entries for unknown shapes. Retention is bounded and honest: the feed
evicts from the front counted (`history_truncated`), while the pairing
index, accepted plans, and session facts live outside the window —
the structure Phase 2's obligations-outlive-eviction rule builds on.
Replay/live rides `amux.transcript_ready`; a differing `sessionId` is the
relink fact and opens a fresh epoch; re-replay after source shrink is
idempotent by row uuid. Research-first against the Phase 0 fixtures
found real drift — recorded in transcript-semantics.md §18b (agent-name
rows returned, plan_mode attachments, bare `/compact` user rows,
Write-create's empty structuredPatch, same-message rows resuming after a
tool_result, turn_duration closing denial-interrupt turns, duplicated
hook rows, the plan-approval notification wording that defeats the
summarizer's substring split).

### Changes
- crates/amux-ui/src/claude/{mod.rs,fold.rs} — the layer: entry types,
  bounded state, the fold, `check_invariants` extension (+ firing tests).
- crates/amux-ui/src/model.rs — `AgentCard.claude` + accessors; four new
  Violation classes (retention overflow, feed-order arithmetic,
  index-ahead, dedupe incoherence).
- crates/amux-ui/src/update.rs — `with_claude_layer` routing beside the
  summarizer (unification is Phase 2); layer window resets on `Opened`.
- crates/amux-ui/tests/spec/{feed_replay,feed_turns,feed_tools,
  feed_edges}.rs — Chapters 6–9, fixture-driven against
  crates/amux/tests/fixtures/chat-v1 (referenced via `include_str!`, so
  the suite stays IO-free); sequences registered so the wire_free
  differential wraps them.
- crates/amux/tests/fixtures/chat-v1/README.md — corrected the pong row
  claim (the fixture holds no assistant rows; the capture closed at the
  arrival-ordered hook.stop).

### Decisions Made
- No new Msg kinds: the layer folds from the existing Stream Msgs, so
  the recorder, flow classes, and the differential property cover it by
  construction (fold-from-recording == live proven over the new
  sequences).
- The layer lives beside `ClaudeSummarizer`, not unified with it — E2's
  one-fold unification is Phase 2's brief; duplicating interpretation
  now was rejected in favor of leaving the summarizer untouched.
- Session-epoch detection keys on transcript `sessionId` only (never
  hook `session_id`, which is arrival-ordered and could false-trigger).
- Meta user rows, local-command records, attachments, file-history and
  queue rows fold to no entry (known bookkeeping); the bare `/compact`
  row renders as an unstated-source prompt.

### Verification
- fmt + CI clippy (`--workspace --all-targets --features amux/testnet
  -D warnings`) clean; `timeout 600 cargo test -p amux-ui` (10 lib + 1
  runtime + 62 spec, all green); amux spec suite 44 green.

---

## 2026-08-12: Phase 0 simplification pass

### Summary
Simplification pass over the Phase 0 diff (`41243f9..0b7f0d4`), scoped
to the capture harness. Main finding: `CaptureSession::close` returned
the keystroke log but every scenario discarded it, cloning the same
data out of the pub `keys_log` field via a `keys_json` helper one line
earlier — `close` now returns the keys JSON directly, the helper is
gone, and `agent_name`/`rows`/`raw_screen`/`keys_log` went private.
Also: dropped three `let index = …; let _ = index;` dances in
scenarios, made `CONFIG_ATTACHMENT_TYPES` private (redact.rs-internal),
and fixed the garbled `target_debug_dir` comment. Deliberately left
alone: `DaemonEnv` (named-field call-site clarity beats a bare bool),
`claude_spawn_env`/`apply_env` (exist so the unit tests exercise the
exact production seam), `wait_for_transcript_ready` (named domain
boundary), and the `Row` predicate duplication (merging `is_tool_use`
into `tool_use_id().is_some()` would change edge behavior on id-less
blocks). Altitude check: src/testnet is in-process only — no existing
utility covers a real subprocess daemon, which the env-inheritance
seam requires. No fixture row content touched; no recapture needed.

### Changes
- crates/amux/tests/capture/harness.rs, main.rs, redact.rs — above.

### Verification
- `cargo fmt`, clippy (`-p amux --all-targets --features testnet`),
  `cargo test -p amux --lib` (400 ok), spec suite (44 ok), and the
  capture binary's opt-in no-op path — all green.

---

## 2026-08-12: Phase 0 gate — spec corrections from fixtures

### Summary
docs/CHAT.md absorbs the Phase 0 fixture-grounded corrections: plan
review resolution rules (manual approval emits no `permission-mode`
change; approval fact is the canonical tool_result, rejection is
`is_error:true`; approve-auto still owed to H.5), lazy transcript
creation on fresh sessions (empty-chat state, `transcript_ready`
arrives with the first turn), and ask extraction routing on the hook
payload's `tool_name` (`hook.permission_request` also fires for
AskUserQuestion/ExitPlanMode). Gate context: codex review
(gpt-5.6-sol, high) of the phase diff returned five findings — one P1
fixture-privacy leak, four P2 harness defects — all five fixed and
the phase recommitted clean (`0b7f0d4`) before anything was pushed;
leak grep over committed fixtures independently verified zero.

### Changes
- docs/CHAT.md — three corrections, each evidence-tagged "Phase 0".

### Verification
- Prose only; wireframes remain exactly 80 columns (machine-checked).

---

## 2026-08-11: Chat V1 Phase 0 — transcript persistence fix + capture harness

### Summary
Fixed the transcript-persistence bug that starved the structured
stream, and built the real-Claude capture harness that produced the
first baseline fixture set (including the previously-UNOBSERVED
ExitPlanMode rows). Empirical root cause: an amux daemon whose ancestry
includes a Claude session inherits Claude Code's child-session marker
set (`CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDECODE=1`, `CLAUDE_PID`,
`CLAUDE_CODE_SESSION_ID`, …) and leaks it to every claude it spawns;
the child sees itself as a nested session and turns transcript saving
off. Confirmed by `ps eww` on daemon + spawned claude, and by
disassembling the claude 2.1.228 binary's env-builder and suppression
check.

### Changes
- `crates/amux/src/agents/pty.rs`: `spawn_pty_agent` grew an
  `env_remove` param; new `apply_env` helper (env_remove then env).
- `crates/amux/src/agents/claude/session/core.rs`: at the Claude spawn
  seam, scrub `CLAUDE_CHILD_SESSION_ENV_SCRUB` (explicit list, not a
  `CLAUDE_*` wipe — `CLAUDE_CONFIG_DIR` must survive) and set
  `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1`. Unit-tested.
- `crates/amux/src/agents/test_agent.rs`: pass `&[]` for env_remove.
- `crates/amux/tests/capture/`: opt-in real-Claude capture harness
  (`harness=false`), driving keystrokes through the structured input
  seq-guarded path; scenario redaction with a fail-loud verify pass.
- `crates/amux/tests/fixtures/chat-v1/`: 9 redacted, provenance-stamped
  baseline fixtures (pong, tools, permission, question single/multi,
  interrupt, plan approve/reject, compact) + README.

### Decisions Made
- Scrub AND force (not force alone): force fixes persistence, but the
  leaked SESSION_ID/PID/MESSAGING_SOCKET point the child at the parent
  session's identity/IPC — scrubbing is the correct hygiene regardless.
- Poisoned-daemon-by-default in the harness: every capture run doubles
  as a live regression test of the scrub (empty capture ⇒ scrub broke).
- Explicit scrub list, not a prefix wipe: user config like
  `CLAUDE_CONFIG_DIR` must survive.

### Verification
- Unit: env-scrub + force assertions (core.rs), apply_env (pty.rs).
- Live: `ps eww` of the spawned claude showed the markers gone and
  `FORCE=1` set; all 9 poisoned-daemon capture runs produced rows and
  `amux.transcript_ready`.
- Gate: fmt clean, clippy (`--all-targets --features testnet`) clean,
  `cargo test -p amux --lib` 400 passed, spec suite 44 passed.
- Codex review (5 findings, all accepted & fixed before this commit):
  redaction now drops config-bearing attachment rows
  (skills/agents/MCP-tool inventory) that had leaked the owner's Claude
  config — profile isolation breaks auth on macOS (keychain is config-dir
  gated, verified), so the fix strips at capture time + fails-loud verify;
  question_multi asserts every selection + the Other value (encoding solved:
  Space commits the Other checkbox); tools asserts recorded Edit+Bash rows;
  harness holds the daemon child in a kill-on-drop guard across startup;
  plan_approve stops at the ExitPlanMode resolution (264s→26s). All 9
  fixtures recaptured through the new redaction; leak sweep = 0.

### Spec drift recorded (notes/chat-v1/transcript-semantics.md §18a)
- Plan approve (manual) does NOT emit a `permission-mode` change —
  contradicts the docs-sourced C3/§18 rule; approval FACT is the
  tool_result success + canonical content. Rejection = `is_error:true`.
- AskUserQuestion `answers` keyed by question TEXT, not header;
  options are `{label,description}` objects.
- Transcript file is created lazily on the first turn, not at
  SessionStart. docs/CHAT.md deltas queued for the orchestrator in
  notes/chat-v1/phases/00-report.md.

### Next Steps
- Phase 1 (Claude layer fold) can read these fixtures raw.
- Phase 3 owns the multi-select joined-selection encoding (captured
  structurally here but the exact keystroke table is unconfirmed) and a
  mode-cycle capture for D4.

## 2026-08-11: Chat V1 spec (docs/CHAT.md)

### Summary
docs/CHAT.md lands: the normative V1 spec for the structured chat
view over `claude_pty_transcript_v1` — vocabulary (feed / ask /
composer / phase / reader), modes and entry, feed semantics grounded
in a transcript evidence survey with fact-vs-inferred discipline
throughout, the ask lifecycle, principle-derived keybindings (guarded
Ctrl+C, no Alt ever, readline in text fields), diff/artifact
rendering, seven 80-col wireframes, and the real-Claude E2E suite.
Designed from first principles with UX studies of Codex CLI and
opencode plus a transcript semantics survey (working material in
notes/chat-v1/, gitignored per convention). Implementation is phased
in notes/chat-v1/orchestration.md.

### Changes
- docs/CHAT.md — new; companion to docs/UI.md.

### Decisions Made
- The chat is a typed Claude layer over native transcript rows (no
  IR). Asks have two kinds — permission and question; plan review is
  a permission variant with a fullscreen reader.
- Shipped default open mode: raw attach; chat is one setting away in
  the standard config; mobile clients are chat-only.
- Keybindings derive from ten recorded principles: Esc never answers
  or interrupts; interrupt is Ctrl+X alone; Ctrl+C is the guarded
  abandon key (clears a non-empty field as a yankable kill; two-press
  rendered quit when empty), one rule chrome-wide.
- UI.md's deferred content-windowing decision is resolved for the
  chat milestone: window = bounded source tail, relink = epoch.
- Diffs render precomputed `structuredPatch` facts; the only diff
  ever computed client-side is the ask-time preview (`similar`, in
  the fold), numberless and estimated — ask-time hunks do not exist.

### Verification
- Prose spec only; wireframes machine-checked at exactly 80 columns;
  all 34 requirement IDs traceable to their sections. The executable
  half arrives with the phased implementation.

### Next Steps
- Phase 0 (notes/chat-v1/orchestration.md): transcript-persistence
  bug fix + baseline fixture capture; overnight autonomous run.

---

## 2026-08-10: Panic-hook recorder dump

### Summary
Panics now leave a Msg recording. The Runtime's recorder became
`Arc<StdMutex<Recorder>>` (single-threaded fold — contention nil; the
mutex exists for the hook, and a poisoned lock is entered via
`PoisonError::into_inner` so a panic mid-record cannot block the
hook). `Runtime::install_panic_dump()` registers (recorder, dump dir,
BUILD) in a process-global OnceLock; the free function
`amux_ui::write_panic_dump(detail)` reads it and writes a
`DumpReason::Panic` bundle, returning quietly on any error. The
amux-tui panic hook calls it AFTER `restore_now()` (a dump is
worthless if writing it destroys the panic report), and `amux ui`
installs it right after `Runtime::start`. This completes the
dump-trigger set from the spec: tripwire, overflow-reserved
(`ChannelOverflow`), panic, and user-requested (`C-g`).

### Changes
- `crates/amux-ui/src/runtime.rs` — shared recorder; `lock_recorder`
  (poison-tolerant) + `dump_stamp` helpers; `install_panic_dump`;
  `write_panic_dump`; unit test.
- `crates/amux-ui/src/lib.rs` — export `write_panic_dump`.
- `crates/amux-tui/src/terminal.rs` — panic hook: restore, then
  `write_panic_dump(&info.to_string())`, then the previous hook.
- `crates/amux-cli/src/ui.rs` — `runtime.install_panic_dump()`.

### Decisions Made
- A process-global OnceLock, not a hook re-registration: terminal.rs
  owns hook installation order (restore FIRST); amux-ui only exposes
  the write. One Runtime per client process makes the single global
  slot honest.
- The test exercises the hook's dump path without panicking (build a
  Runtime with a tempdir dump dir, record Msgs, install, call
  `write_panic_dump`, assert the file and its header) — actually
  panicking in tests buys nothing and poisons test output.

### Verification
- New unit test: `runtime::tests::write_panic_dump_writes_a_dump_after_install`.
- fmt + CI clippy clean; amux-ui/amux-tui/amux-cli suites, the 44-test
  spec suite, and e2e-runner 14/14.

### Next Steps
- `amux debug ui-dump` (IPC-triggered dump of a running TUI) remains
  deferred.

---

## 2026-08-10: Model invariants checked at the fold seam

### Summary
`Model::check_invariants()` reports structural incoherence as typed,
entity-addressed `Violation` values, and the Runtime enforces it after
every fold: panic in debug builds, dump-once-per-violation-kind
(`DumpReason::Tripwire`, detail `invariant: …`) plus `tracing::warn`
in release — never a release panic, folding continues. This is the
third assertion class, distinct from input tripwires (refuse + dump at
the receiving reducer arm) and renderer staleness (tolerance/clamps,
never assertions); docs/UI.md's Testing section now draws that
three-way line.

### Changes
- `crates/amux-ui/src/model.rs` — `Violation` (kind() throttle key +
  Display) and `check_invariants()`: streams ⊆ agents; card/host
  epochs never ahead of the model epoch; when synchronized, all card/
  host epochs equal it; `finished_ops` within retention; cached
  `card.attention` equals its summarizer's derived attention. Doc
  comment carries the discipline rule: invariants range over the
  structural index (ids, counts, phases, epochs), never content —
  O(entities) forever.
- `crates/amux-ui/src/runtime.rs` — `enforce_invariants()` runs in
  `process()` after `update` AND after the effects loop;
  `reported_violations: HashSet<&'static str>` throttles release
  dumps. Debug-only shell companion (post-effects, where a CloseStream
  decided by the same fold has already executed): every live stream
  task key exists in `model.streams`.
- `crates/amux-ui/src/lib.rs` — export `Violation`.
- `crates/amux-ui/tests/spec/wire_free.rs` — the differential wrapper
  additionally asserts `check_invariants().is_empty()` after every Msg
  of every registered sequence.
- `docs/UI.md` — Testing: the three-way assertion line.

### Decisions Made
- Checked at the seam (shell), not inside `update`: the reducer stays
  a pure fold; coherence enforcement is shell policy, like dumping.
- Reused `DumpReason::Tripwire` rather than adding a variant — the
  meaning ("state the protocol says cannot happen") is the same; the
  detail string distinguishes fold-seam invariants.
- Both check points matter: after `update` catches the fold that broke
  the Model; after the effects loop catches shell bookkeeping drift
  and is where the task-map companion is race-free.

### Verification
- New unit tests (model.rs): `detects_stream_without_agent`,
  `detects_epochs_ahead_of_the_model`,
  `detects_stale_epochs_while_synchronized`,
  `detects_finished_ops_over_retention`,
  `detects_attention_disagreeing_with_the_summarizer` — each corrupts
  a fold-built Model directly and asserts its class fires.
- fmt + CI clippy clean; amux-ui/amux-tui/amux-cli suites and the
  44-test spec suite pass.

### Next Steps
- None; the invariant list grows only with new structural state.

---

## 2026-08-10: Restore the terminal on SIGINT/SIGTERM/SIGHUP

### Summary
The chrome now restores the terminal when killed by SIGINT/SIGTERM/
SIGHUP, closing the gap `docs/UI.md` already scoped ("nothing survives
SIGKILL" — but the catchable signals should not leave a raw-mode alt
screen either). A `signal_restore` module in `amux-tui/src/terminal.rs`
installs an async-signal-safe low-level handler (via `signal-hook`)
that restores the cooked termios saved before raw mode first engaged
(`tcsetattr`), writes the leave-alt-screen/show-cursor bytes with a raw
`write`, then re-raises the default disposition so the process still
dies exactly as expected. Installed once from `install_panic_hook`, so
every chrome entry point gets it.

### Changes
- `Cargo.toml` — workspace dep `signal-hook = "0.3"`.
- `crates/amux-tui/Cargo.toml` — `[target.'cfg(unix)'.dependencies]`
  libc + signal-hook.
- `crates/amux-tui/src/terminal.rs` — `signal_restore` module
  (cfg(unix)); `install_panic_hook` calls its `install()`; unit test
  `signal_restore_bytes_match_write_restore` locks the handler's
  hardcoded `RESTORE_BYTES` to what `write_restore` (crossterm)
  actually emits.

### Decisions Made
- Not a tokio signal arm in the chrome loop: the handler must cover
  the mid-attach passthrough phase, where nothing polls the event
  stream — a stream-based handler would leave the process deaf to
  signals exactly when the terminal is rawest. The low-level handler
  works process-wide regardless of what the main thread is doing.
- The restore is deliberately unconditional (no CHROME_OWNS_TERMINAL
  gate): on a sane terminal it is a no-op, and that is what lets one
  handler cover both chrome mode and mid-attach.
- Signals covered: SIGINT, SIGTERM, SIGHUP. SIGQUIT is left at its
  default (core dump semantics stay untouched); SIGKILL is
  uncatchable by definition.

### Verification
- `signal_restore_bytes_match_write_restore` passes; fmt + CI clippy
  clean; full per-crate suites and the 44-test spec suite pass;
  e2e-runner 14/14.
- Automated signal-delivery testing is deliberately absent: kill-based
  tests of terminal state are flaky by construction. One manual
  `kill -TERM` verification on a real terminal is still pending.
- Known remaining gap (pre-existing, milder): CLI-only `amux attach`
  (never entered the TUI) does not install the handler, so a signal
  mid-passthrough there still leaves the terminal raw.

### Next Steps
- Manual `kill -TERM` check during chrome and mid-attach on a real
  terminal.

---

## 2026-08-10: TUI V1 review fixes — resize guard, stream lifecycle, scroll clamp

### Summary
Fixed the code-review findings on the V1 TUI. P1: the fleet renderer
underflowed `width - RIGHT_INFO_FROM_EDGE` below ~13 columns (debug
panic / huge `pad_to` allocation in release) — the below-minimum guard
now covers it and a sweep test renders every viewport 1..=200 ×
1..=60. P2s: late stream events for removed agents no longer
re-materialize `Model::streams` ghosts; `prune_if_synchronized` emits
`Effect::CloseStream` for every stream it drops so reconnect pruning
no longer orphans shell tasks; and render clamps stale scroll/selection
against the rows it is actually drawing, so a subscription-driven fleet
shrink cannot draw an empty list until the next keypress. Minors:
finished stream `JoinHandle`s are dropped when their `Closed` Msg is
observed, the dead `Effect::ScheduleTick` vocabulary is deleted, and
the Claude notification permission/question string-match is marked as
a known-fragile seam.

### Changes
- `crates/amux-tui/src/render.rs` — `MIN_FRAME_WIDTH` guard
  (too-small notice below 13 cols); render-time clamp of stale
  `view.scroll`/`view.selected` (formatting a stale ViewState against
  the Model, not deciding — render stays pure).
- `crates/amux-ui/src/update.rs` — `update_stream` discards events for
  agents absent from `model.agents` (all arms);
  `prune_if_synchronized` returns `CloseStream` effects, propagated
  from both synchronized arms (skipping already-Closed streams,
  matching the `AgentRemoved` arm).
- `crates/amux-ui/src/runtime.rs` — `process` drops an agent's stream
  `JoinHandle` on an observed `Closed` Msg when the task is finished
  (shell resource bookkeeping; `is_finished` guards against a stale
  Closed racing a newer OpenStream); `ScheduleTick` executor arm gone.
- `crates/amux-ui/src/effect.rs` — `Effect::ScheduleTick` deleted
  (nothing emitted it; the TUI drives ticks via its own interval +
  `observe_now`).
- `crates/amux-tui/src/run.rs` — ticker comment: the interval always
  fires, only the repaint is gated (deliberate V1 simplification).
- `crates/amux-ui/src/summarizers/claude.rs` — KNOWN-FRAGILE SEAM
  comment on the notification-text permission/question split.
- Tests: `sessions::late_stream_events_after_removal_leave_no_ghost_state`,
  `connection::epoch_prune_emits_close_stream_for_dropped_streams`
  (both sequences registered, so the wire_free differential wraps
  them), golden `fleet_too_narrow` (new frame; existing frames
  untouched), `rendering_never_panics_at_any_viewport_size`,
  `stale_scroll_after_fleet_shrink_clamps_at_render`.

### Decisions Made
- Below-minimum widths reuse the existing "amux: terminal too small"
  state rather than growing a second degraded layout; the new
  `fleet_too_narrow` golden locks it as a byte contract.
- Discarding late stream events keys on `model.agents` membership,
  which restores the invariant `streams ⊆ agents` everywhere (Opened
  inserts only for known agents; removal and prune delete both sides).
- Handle cleanup on `Closed` only removes finished tasks: best-effort
  cleanup that can never discard a live task's handle.

### Verification
- `cargo +nightly fmt --all`; `timeout 600 cargo clippy --workspace
  --all-targets --features amux/testnet -- -D warnings` clean.
- `timeout 600 cargo test -p amux-ui` (28 passed), `-p amux-tui`
  (19 passed), `-p amux-cli` (52 passed), `-p amux --features testnet
  --test spec` (44 passed).

### Next Steps
- The stale-Closed-vs-new-OpenStream reducer race (stream Msgs carry
  no generation id, so a queued Closed can overwrite a reopened
  stream's phase) is noted but out of scope here.

---

## 2026-08-09: TUI V1 M4 — attach round-trip (the spine)

### Summary
`enter` on a fleet row now runs the real passthrough: the chrome leaves
the alternate screen and restores termios (RAII guard from M3), the
existing `session_client` passthrough runs in-process on the real
terminal, and on detach the chrome re-enters and repaints from the
Model. `<leader>d` and `<leader>s` both detach (the fleet IS `s`'s
target in V1). The passthrough was reused, not rewritten: the leader-
scanning stdin reader and select loop moved intact into
`spawn_stdin_reader`/`attach_loop`, now parameterized over injected
input events and an output writer so the tier-2 suite drives them
without a terminal; the CLI `amux attach`/`amux new` paths keep their
exact print-and-exit behavior via `finish_cli_attach`.

### Changes
- `crates/amux-cli/src/session_client.rs` — `AttachOutcome`,
  `subscribe_raw`, `spawn_stdin_reader` (+`s` chord, +stdin reclaim),
  `attach_loop` (generic writer), `attach_terminal`, `attach_for_ui`;
  tier-2 `mod attach` test suite (embedded daemon + pty `cat` agent).
- `crates/amux-tui/src/run.rs` — attach callback now returns an optional
  status-line notice; outcomes surface in the fleet status line.
- `crates/amux-cli/src/ui.rs` — wires `attach_for_ui` as the handoff.

### Decisions Made
- Stdin reclaim: the blocking stdin reader cannot be killed portably
  mid-read, so when a session ends WITHOUT a detach chord (agent exited,
  killed, daemon shutdown) the TUI path prints
  `[<label> — press any key to return to the fleet]` and the next
  keypress is consumed to hand stdin back exclusively before the
  chrome's event stream reads again. Detach chords need no prompt (the
  reader exits as part of the chord). CLI paths never reclaim — the
  process exits, as today.
- Tier-2 tests use the embedded in-process daemon (same pattern as
  amux-ui's runtime test) rather than the multi-daemon testnet: the
  testnet's client accessors are pub(crate) and attach is single-daemon
  work; a real daemon plus a real pty test-agent is the tier-2 contract.
  Test names live under `session_client::attach::*` because amux-cli is
  a binary crate (no lib target for integration tests to import).
- Named tests: `attach::round_trip_repaints_fleet` (attach → echoed
  output → detach → fleet repaints from Model → attach again → 100
  scripted attach/detach cycles), `attach::detach_leaves_terminal_sane`
  (vt100 over the real enter/restore byte sequences: alternate screen
  entered and left, cursor re-shown),
  `attach::kill_during_attach_still_restores_the_terminal` (delete the
  agent mid-attach; the loop reports the close instead of hanging, and
  the restore sequence still yields a sane vt100 screen),
  `attach::offline_host_shows_dial_error_instead_of_attaching` (enter on
  an offline host surfaces `last_dial_error` in the status line, no
  attach).
- Late attach relies on the existing buffer replay unchanged
  (`replay_query: None`); how well real agent TUIs reconstruct was not
  visually verified in this environment — the SIGWINCH-wiggle fallback
  remains the known remedy, out of V1 scope.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-cli` — 52 passed (incl. the four
  attach tests; the 100-cycle loop runs in ~0.1s against the embedded
  daemon). `timeout 600 cargo test -p amux-ui` — 26 passed;
  `-p amux-tui` — 16 passed; protocol spec suite 44 passed.
- Non-TTY smoke: `amux ui` without a terminal fails gracefully
  ("Device not configured"), no panic, no partial terminal state.
- Interactive feel ("the loop feels instant") not verifiable headless;
  the tier-2 round trip plus terminal-hygiene bytes are the executable
  stand-in. Real-terminal pass recommended at review.

### Next Steps
- M5 (naming translation cleanup) deliberately not picked up.
- Backlog: recorded claude-code fixture to supplement the authored M2
  ones; `amux debug ui-dump` IPC; Windows CI leg for amux-tui.

---

## 2026-08-09: TUI V1 M3 — amux-tui fleet screen + golden frames

### Summary
New library crate `crates/amux-tui` (ratatui + crossterm), consumed by
`amux-cli`: bare `amux` (and `amux ui`) opens the fleet. The renderer is
a pure function of (Model, ViewState, FrameContext) building the whole
chrome as styled text lines, so the tier-3 golden frames control every
cell; the checked-in frames reproduce the spec mockups verbatim
(byte-compared in this session). Event loop is event-driven — drain,
fold, draw once, dirty-flag gated, ticks only while relative ages are on
screen. Alt-screen only, RAII terminal guard with a chained panic hook
that restores before reporting.

### Changes
- `crates/amux-tui/src/{lib,render,view,keys,run,terminal}.rs` — new.
- `crates/amux-tui/tests/golden.rs` + `tests/golden/*.txt` — all named
  frames: fleet_ranked, fleet_attention_badges, fleet_offline_host_rows,
  fleet_cloud_auth_banner, fleet_empty_no_agents, fleet_daemon_starting,
  fleet_daemon_unreachable (extra), picker_filtered, row_rename_inline,
  delete_confirm_statusline, op_pending_and_failed, help_overlay,
  fleet_ranked_80col/60col (column-collapse rule), plus a run-stability
  test and style assertions (badge colors, offline dim) that text
  goldens cannot see.
- `crates/amux-cli/src/ui.rs` + `main.rs` dispatch — bare `amux` runs
  init first on an uninitialized machine (CLI-owned auth; TUI stays
  auth-passive), then opens the fleet; the connector wraps `get_client`
  so daemon spawn shows as the "Starting daemon…" state. `amux ui` is
  the explicit alias.
- `crates/amux/src/{setup,identity}.rs` — minimal read-only plumbing:
  `setup::local_host_id[_in]()` reads the stored identity's host id
  (the wire does not mark the local host); with test.
- `crates/amux-ui` — Model grew `effective_attention`/`status_label_for`
  (offline hosts degrade display to Unknown, computed once for every
  renderer) and fleet ranking now uses effective attention; summarizer
  routing keys on the advertised `claude_pty_transcript_v1` protocol
  fact instead of the `agent_type` string; op seq is 1-based; kernel
  entity re-exports widened (Capabilities, HostTrustStatus).

### Decisions Made
- Whole-frame text rendering (one Paragraph of spans) instead of nested
  ratatui widgets: the mockups are the contract, and byte-level control
  is what makes "verbatim" testable.
- Spec-mockup inconsistency flagged (NOT silently deviated): the rename
  and pending-row mockup lines place age/status at columns 46/52 while
  every fleet-frame row uses 48/54. The renderer uses the fleet grid
  consistently; those two golden lines differ from the mockup by that
  2-column shift only. All other mockup lines match byte-for-byte.
- The attach handoff seam is in place (`run_fleet` takes an async attach
  callback; chrome restores the terminal before calling it and resumes
  after) but amux-cli passes a no-op until M4 wires the passthrough.
- Rename mode hides the `▸` marker on the edited row (the draft cursor
  `▌` marks focus) — matches the mockup.
- Column collapse: the status-word column drops below 68 columns; key
  hints drop when they no longer fit. Locked by the 80/60col frames.
- Debug-dump keybinding is `C-g`; `amux debug ui-dump` (spec-listed)
  needs an IPC surface to reach a running TUI's ring and is deferred
  with a note — the keybinding plus tripwire/panic/overflow triggers
  cover V1.
- Layout math counts chars (fixtures are width-1 glyphs); wide-glyph
  names will mis-pad until a unicode-width pass — accepted for V1.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-ui` — 26 passed; `timeout 600 cargo
  test -p amux-tui` — 16 passed (goldens stable across two runs);
  protocol spec suite 44 passed; `amux --help` lists `ui`.
- `fleet_ranked` golden byte-compared against the spec mockup: verbatim
  match; all other spec frame lines verified present verbatim except the
  two flagged 46/52-column lines.
- Windows leg NOT verifiable from this mac: `cargo check -p amux-tui
  --target x86_64-pc-windows-msvc` fails building `ring`'s C code (no
  Windows-target C toolchain locally). amux-tui itself has no
  platform-specific code; CI's Windows runner builds ring natively.

### Next Steps
- M4: attach round-trip — reuse `session_client` passthrough in-process,
  terminal hygiene tests over the vt100 harness.

---

## 2026-08-09: TUI V1 M2 — attention (pure amux-ui, zero core changes)

### Summary
Attention lands as a pure per-agent fold with zero changes under
`crates/amux` (verified: empty diff). `summarizers/claude.rs` folds the
`claude_pty_transcript_v1` stream — raw transcript rows interleaved with
the three hook rows the claude session emits — into the kernel
`Attention` vocabulary, with the entry/clear table written exhaustively
in the module docs. The kernel subscription policy is in the reducer:
every local agent advertising the structured stream is subscribed (tail
1000, one policy constant), remote agents join on a reified
`Msg::UserAttached`. The shell grew the stream executor: per-agent tasks
that coalesce entries into batched Msgs before the recorder sees them
and derive the truncation fact from the first replayed seq.

### Changes
- `crates/amux-ui/src/summarizers/{mod,claude}.rs` — new; typed
  `SummarizerState` carried on the `AgentCard`.
- `crates/amux-ui/src/{update,model,msg,runtime,lib}.rs` — subscription
  policy (`ensure_stream`), attention folding on stream Msgs,
  `Msg::UserAttached`, `Effect::OpenStream/CloseStream` executor.
- `crates/amux-ui/tests/spec/attention.rs` + `tests/spec/fixtures/
  claude_{permission_flow,stop_and_notification,truncated_tail}.json`.

### Decisions Made
- Clearing rules: a permission answered through raw passthrough is only
  visible as subsequent activity — and a blocked agent emits nothing, so
  any activity row IS the unblock evidence. Only hook rows leave
  `Working`, so streaming output cannot strobe (hysteresis by
  construction rather than by timer).
- `ClaudeHookKind` enumerated: SessionStart/SessionEnd are
  internal-only, Unknown is dropped at the core — the stream carries
  exactly permission_request/stop/notification plus transcript rows.
- Question-vs-permission: hooks alone do not distinguish them; both
  arrive as Notification differing in text. The fold inspects the
  message ("permission" → Permission, else Question) — interpretation at
  observation time, resolving M2's open question in the spec.
- Honest degrade: truncation = first replayed seq > 1 (the source buffer
  is bounded, so this also covers server-side eviction). A truncated
  window with no attention-bearing rows reports `Unknown`; the same
  window over complete history reports `Idle`. Weak rows (`summary`,
  `system`, unknown shapes) carry no signal either way.
- Stream-loss degrade: transport-ish closes invalidate the fold to
  `Unknown`; orderly agent exit resets it (phase carries the exit).
  Retryable closes reopen on the next inventory upsert — bounded, no
  timer loops.
- Fixtures are authored (shapes from `agents/claude/session/hooks.rs` +
  the Claude Code transcript/hook schema) and say so in `capturedWith`.
  No live claude-code capture was available in this environment; the
  spec's intent is recorded-first, so replacing/supplementing these with
  a real redacted capture stays on the backlog.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-ui` — 26 passed (25 tier-1 incl. the
  six named attention tests wrapped by the differential property, 1
  tier-2).
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44
  passed; `git diff crates/amux` empty (zero core changes).

### Next Steps
- M3: `amux-tui` fleet screen + golden frames; M4: attach round-trip.

---

## 2026-08-09: TUI V1 M1 — amux-ui becomes the reducer

### Summary
Replaced the placeholder `amux-ui` crate (1.1k lines, zero consumers)
wholesale with the reducer core from `docs/UI.md`: serializable `Msg`s
in, pure `update(&mut Model, Msg) -> Vec<Effect>`, a `Runtime` shell
that owns `amux::Client` and funnels every stimulus through one ordered
bounded channel, and a `Recorder` ring whose dumps are self-contained
JSONL replay bundles. Tier-1 spec suite (`tests/spec/`) with the
differential fold≡live property wrapping every chapter's sequences, plus
a tier-2 embedded-server integration test replacing the old ignored one.

### Changes
- `crates/amux-ui/src/{msg,model,update,effect,recorder,runtime,lib}.rs`
  — new crate body; old `{agent_cache,cmd,error,inventory,notification,
  runtime,session,types}.rs` deleted, no compat wrapper.
- `crates/amux-ui/tests/spec/{main,harness,connection,inventory,ops,
  sessions,wire_free}.rs` — tier-1 chapters; `tests/runtime.rs` — tier-2.

### Decisions Made
- Reconnect snapshot semantics: entities are epoch-tagged; the swap to
  the new snapshot happens at the synchronized marker (both hosts and
  agents snapshots complete), so stale rows stay visible during catch-up
  and renderers distinguish "loading" from "empty".
- `local_host_id` enters the Model via `ServerMsg::Connected` — the wire
  does not mark the local host, so the embedding client reads the device
  identity and hands it to the shell (wired up in M3; `None` degrades to
  no local-subscription policy, honestly `Unknown` attention).
- OpIds are minted by the shell (`Uuid::new_v4`) and enter via
  `Msg::Command`; epochs are minted by the reducer (increment on
  `Connected`) — both deterministic under replay.
- The recorder checkpoint advances by folding evicted Msgs through the
  same pure `update`, so `checkpoint + ring` always reproduces the live
  Model; dumps are 0600, retention-bounded (20 files), never uploaded.
- `cloud_auth_required` is Model state set by auth-required op/stream
  failures (mapped from `ProtocolError::InvalidCredentials`), cleared on
  reconnect; a connection-level credential failure maps to
  `DisconnectReason::AuthenticationRequired`. Degraded banner, never a
  blocking screen.
- `Effect::OpenStream`/`CloseStream` are defined but the reducer does
  not emit them yet — the subscription policy is M2's first bullet; the
  shell executor lands with it.
- Fleet ranking: `NeedsYou` first (permission, question, finished), then
  recency, id as the deterministic tiebreak; pending creates render as
  optimistic rows at the bottom in dispatch order.
- `AgentPhase::Suspended` was not modeled: nothing on the wire reports
  it (suspended agents leave inventory), and inventing it would violate
  facts-only. `exited(N)` derives from stream-close facts.

### Verification
- `cargo +nightly fmt --all` (repo formats with nightly rustfmt).
- `cargo clippy --workspace --all-targets --features amux/testnet -- -D
  warnings` clean.
- `timeout 600 cargo test -p amux-ui` — 18 passed (17 tier-1 spec, 1
  tier-2 embedded-server integration).
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44
  passed (protocol suite untouched).

### Next Steps
- M2: attention summarizer fold + subscription Effect policy.
- M3: `amux-tui` fleet screen + golden frames; M4: attach round-trip.

---

## 2026-08-10: `q quit` in the hint bar; Ctrl-C quits the chrome

### Summary
UX pass #3. The status-line hints gain `q quit` (HINTS_COL 31 → 25 so
the longer string still fits the 68-col frame), and Ctrl-C now quits
the chrome from ANY mode — the chrome is a stateless viewer and must
never feel like it traps the terminal (`esc` cancels modes). First
intentional run of the golden regeneration workflow: 11 frames
regenerated with UPDATE_GOLDENS=1 and diff-reviewed — every change is
the hint shift/addition plus two help-overlay lines (`q / C-c  quit`;
`C-a d` relabeled "detach to shell" post-chord-split). Verified the
passthrough is untouched: `handle_key`'s only production caller is the
chrome loop (unreachable while attached), the stdin reader intercepts
leader chords only (0x03 forwards to the agent as always), and Claude's
interrupt injection (`session/core.rs` StopPolicy) is server-side.

### Decisions Made
- Ctrl-C is global quit even mid-rename/filter; esc is the cancel key.
- Backlog (noted, not fixed): an external `kill -INT` terminates the
  chrome without unwinding (keyboard cannot produce SIGINT in raw
  mode), skipping terminal restore — a signal handler running the same
  restore would close it; parked, vanishingly rare. Also candidates,
  discussed but NOT committed to: a `DumpReason::Panic` recorder dump
  from the panic hook (cheap, fires exactly when a recording matters
  most — do opportunistically), and a `Model::check_invariants()`
  harness pass (streams ⊆ agents, epoch bounds, attention ==
  summarizer-derived). The invariant checker is an oracle without a
  generator today — hand-authored sequences only reach "normal" states
  — so it waits for whichever arrives first: fuzzed Msg sequences, the
  chat-milestone Model growth, or a second shipped state-lifecycle bug.

### Verification
- `timeout 600 cargo test -p amux-tui` 21 passed (19 + 2 new key
  tests: `ctrl_c_quits_from_any_mode` incl. filter/rename where `c` is
  text input, `q_quits_in_normal_mode`); goldens stable post-regen.
- `-p amux-ui` 29; `-p amux-cli` 53; e2e 14/14; CI clippy clean.

---

## 2026-08-10: Readonly agents hidden from the fleet

### Summary
UX pass #2: readonly agents (externally captured sessions the chrome
cannot drive) are hidden from the fleet until the structured chat view
can render them. Per "views format, never decide" the filter lives in
`Model::fleet()`; a new `fleet_agent_count()` keeps the header, empty
state, and ticker consistent with the visible list (`agent_count()`
stays the honest entity total); and the subscription policy skips
readonly agents — a badge nobody can see is not worth a stream.

### Verification
- `timeout 600 cargo test -p amux-ui` 29 passed incl.
  `inventory::readonly_agents_are_hidden_from_the_fleet` (visibility,
  counts, and no `OpenStream` in one sequence, wrapped by the
  differential property); `-p amux-tui` 19 (goldens untouched);
  `-p amux-cli` 53; e2e 14/14; CI clippy clean.

---

## 2026-08-10: `<leader>d` detaches to the shell; `<leader>s` is the fleet

### Summary
First UX-pass fix: V1 had collapsed both chords into return-to-fleet
(noted in the M4 entry); real usage immediately surfaced the tmux
muscle-memory expectation — `d` means the shell. The chords now split
all the way through: `StdinEvent::{Detach,SwitchToFleet}`,
`AttachOutcome::{Detached,SwitchedToFleet}`, and `attach_for_ui`
returns a typed `amux_tui::AttachReturn::{Fleet(notice),Exit}` that
`run_fleet` honors (`Exit` ends the TUI; the terminal was already
restored by the handoff). CLI `amux attach` treats `s` like detach
(no fleet to return to — opening the TUI from there is a future
nicety). The help overlay already documented these semantics, so no
golden changes: behavior caught up with the documentation.

### Verification
- `timeout 600 cargo test -p amux-cli` 53 passed (round-trip second leg
  now locks `s`→`SwitchedToFleet`; first leg and the 100-cycle loop
  still lock `d`→`Detached`; new `leader_chords_split_detach_from_fleet`).
- `-p amux-tui` 19, `-p amux-ui` 28, e2e 14/14, CI clippy clean.

---

## 2026-08-10: Bare `amux` keeps printing help off-TTY; e2e green again

### Summary
First TUI push turned the e2e leg red on both unix runners: `bare_help`
runs bare `amux` in a context without a usable TTY, where the new
open-the-fleet dispatch errored (ENXIO) instead of printing help. Bare
`amux` now checks `IsTerminal` on stdin+stdout: real terminal → fleet
TUI (init-first unchanged); scripts/pipes → top-level help, exactly the
pre-TUI behavior (`amux ui` still errors honestly off-TTY). The
`bare_help` expected output also needed the new `ui` command line.
Process lesson recorded: the local review battery covered every cargo
suite but not `cargo run -p e2e-runner -- run` — e2e belongs in the
pre-push battery for anything touching the CLI surface.

### Verification
- `timeout 600 cargo run -p e2e-runner -- run`: 14 passed, 0 failed.
- CI run on the fix watched to completion (see push).

---

## 2026-08-10: CI hardening for the TUI suites before first push

### Summary
CI's test job already runs `cargo test --workspace --all-targets` on all
three OSes, so the new amux-ui/amux-tui/amux-cli suites are covered with
zero workflow changes — but two Windows hazards needed closing first.
The three PTY-backed tier-2 tests (`attach::round_trip_repaints_fleet`,
`attach::kill_during_attach_still_restores_the_terminal`,
`runtime_reflects_daemon_state_in_the_model`) get the existing ConPTY
ignore gate (same attribute and reason as the embedded suite; the pure
byte-sequence and no-agent attach tests stay ungated everywhere). New
`.gitattributes` pins LF on the tier-3 golden frames so Windows
checkouts cannot CRLF-translate byte-contract fixtures.

### Verification
- `cargo +nightly fmt --all` clean; `timeout 600 cargo test -p amux-ui`
  28 passed, `-p amux-cli` 52 passed, `-p amux-tui` 19 passed (macOS —
  gates are windows-only, nothing ignored here).

---

## 2026-08-09: Client-layer design — docs/UI.md, external review, V1 TUI spec

### Summary
Designed the client/UI layer and committed it as `docs/UI.md` (fourth
committed doc; CLAUDE.md repointed). Core decisions: `amux-ui` is a
reducer core — serializable Msgs in, pure `update`, Effects out to a
shell — with CQRS edge vocabulary (`Command` in, entity-keyed idempotent
`Delta` out, `Effect` internal, `Ephemeral` reserved/uninhabited); a
kernel of protocol facts plus typed per-agent layers (no generic agent
IR, no capability flags); the core↔UI boundary as three verbs (core
transports and translates facts, never interprets — attention is a pure
per-agent fold at observation time, zero core changes); chrome-first TUI
(fleet around existing raw passthrough, alt-screen only, no scrollback
writes, no split panes ever); four test tiers with a differential
fold≡live property as the load-bearing test, plus a Msg recorder whose
dumps replay as tier-1 regressions.

The draft was reviewed externally before revision: a GPT 5.6 Sol xhigh
design review (which read core source) plus three Opus agents comparing
against openai/codex, sst/opencode, and pingdotgg/t3code. The two
central rejections turned out to be empirical — t3code and opencode
shipped the generic IR and per-client folds and the predicted failure
modes are visible in their trees; t3code independently converged on the
same reducer architecture. Revisions from the review: scoped and
enforced determinism guarantee, authority rule (subscriptions are the
sole entity writer; RPC results resolve ops only), per-Msg flow classes
with loud overflow, retention rule (never evict live obligations),
cloud-auth expiry as a degraded banner rather than a blocking screen,
unknown-agent attach reduced to card-only pending an agent-independent
raw terminal protocol. Full findings: notes/ui-review-findings.md
(gitignored); V1 build spec with milestones, named tier-1 tests, and
aligned golden-frame mockups (flat globally-ranked fleet):
notes/tui-v1-spec.md (gitignored).

### Changes
- `docs/UI.md`: new — normative client-layer design.
- `CLAUDE.md`: points at docs/UI.md.

### Decisions Made
- amux-tui will be a library crate invoked by amux-cli (one shipped
  binary); bare `amux` opens the TUI, running the init flow first on an
  uninitialized machine (CLI dispatch — the TUI stays auth-passive).
- V1 attention subscribes local agents only (+ interacted-with
  remotes); ClientService-side stream dedup/replica is a deferred,
  seam-protected optimization.
- Naming cleanup (`provider_label` translation, provenance deletion) is
  a lightly-held candidate chunk, decided at pickup.

### Verification
- Docs-only. Spec suite green on current toolchain (44 passed, see the
  catch-up entry below).

---

## 2026-08-09: Catch-up — two June commits landed without entries

### Summary
Work paused ~6 weeks after 2026-06-28; two commits predate the break and
have no entries. `2e5f9d8` (2026-06-20) added QR pairing deeplinks
(production `amux://pair` links from `pair --qr`, debug-only `--link`
helper for simulator pairing) and tucked in two behavior changes
documented only in the commit message: untrusted cloud pairing
candidates no longer trigger eager trusted-tunnel activation, and remote
agent-event subscriptions are gated on trusted hosts. `baaebc3`
(2026-06-28) serialized CLI refresh-token rotation
(`DeviceFlowProvider` refresh lock in `amux-cli/src/auth.rs`),
coordinated same-day with amuxapp ("Handle rotating runtime
credentials") and amuxcloud ("Test rotating refresh token clients").

### Verification
- Spec suite re-verified green on the current toolchain 2026-08-09:
  44 passed / 0 failed / 0 ignored, 32.3s (the 44th is the presence
  spec test added by `2e5f9d8`).

---

## 2026-06-17: Cleanup-review pass over the client-only/seam effort

### Summary
Reviewed the full client-only → seam effort (`f90a8de`..`1925197`) for
vestigial code and stale references left by the superseded passes (the
`LocalAgentHost` trait replaced the earlier "Design Y" gating). The
codebase came back clean: no dead code, every suspect (`local_agent_count`,
the dual agent-event subscribe paths, `is_cloud_server`, `DebugAgent`) is
still reached in at least one feature config, and the ~21 residual
`local-agents` cfgs are all module/re-export/construction/capability gates.

One stale reference fixed: the iOS `compile_error!` in `lib.rs` still told
callers to use the `client-only` feature, which was deleted in `40b426b`.
Corrected to "depend on amux with `default-features = false`."

### Verification
- `cargo build -p amux` (default): clean.

---

## 2026-06-17: Self-update poll is a desktop-daemon concern, not embedded

### Summary
Fixes a regression from the 2026-06-16 "decouple daemonish" change: that
commit gated the periodic self-update poll behind a `with_daemon_tasks`
flag and conflated it with reachability, which broke the
`embedded_server_runs_update_checker_when_reporter_is_configured` test and —
more importantly — encoded the wrong model. The active manifest poll checks
for a newer amux *binary* to install; a mobile client can't self-update (the
app store owns its binary), so it must never poll. A too-old mobile client is
instead told to update *over the cloud connection*: a relay rejects
under-version clients (`Config::minimum_client_versions`) and the client
surfaces `UpdateStatus::Required` to its `update_reporter`
(`services/startup/cloud.rs`) — that lib path is already wired; acting on the
signal (forcing an app-store update) is the host app's job and is not yet
implemented in the mobile app.

Split `spawn_local_background_tasks` (cloud connection — every local host)
from a new `spawn_daemon_background_tasks` (reachability links + update poll —
desktop daemon only). The `with_daemon_tasks` flag is gone; the embedded path
simply never calls the daemon-only function.

### Changes
- `server.rs`: split the two task groups; deleted the `with_daemon_tasks`
  flag; documented the poll-vs-enforcement distinction.
- `tests/embedded.rs`: replaced the "embedded polls" test with
  `embedded_server_does_not_poll_for_updates` (reaches a live manifest server,
  asserts the embedded client never hits it).
- `server.rs` unit tests: added `update_checker_is_spawned_with_reporter`.

### Verification
- `cargo build` / `clippy` (default + `--no-default-features`): clean.
- `cargo test --lib`: 394 / 300 passed. `--test embedded`: 10 passed.
- `cargo test --features testnet --test spec`: 43 passed.

---

## 2026-06-16: Make local-agents a real seam (LocalAgentHost trait)

### Summary
Replaced the pervasive, implicit `local-agents` boundary with one explicit
abstraction. The core no longer reaches the agent runtime by concrete type;
it depends on a `LocalAgentHost` trait and holds an
`Option<Arc<dyn LocalAgentHost>>` (`None` = the embedded client). The concrete
runtime — sessions, PTY, hooks, lifecycle, suspend/resume, session I/O,
the registry, and its three event sources — moved behind the seam into the
`PtyAgentHost` impl, gated at its module declaration. Every RPC and shutdown
path that used to carry a `#[cfg]` (and usually a hand-written client-only
twin) is now a uniform delegator whose `None` arm is ordinary control flow.

Derived the trait surface from the existing `#[cfg(not)]` stubs (they were the
de-facto "what the core needs without a runtime"); cross-checking surfaced
that the three `EventSource`s and the `SessionEvent` channel were structurally
"core" but only ever fed by runtime code, so they moved into the host too —
that is what makes the boundary clean rather than merely relocated. The
shutdown *orchestration* stays in `server.rs` (so `notify_routing_peers` keeps
interleaving between local-notify and commit); `prepare_suspend` owns the save
and folds prepare/save failures into `Err`, and `server.rs` bails before
notify/commit on `Err`.

Result: `cfg(feature = "local-agents")` sites 87 → 21, and the 21 are all
module declarations, re-exports, construction wiring, or the `routing/host.rs`
capability statement — **no boundary/business-logic gate and no cfg twin
remains**.

### Changes
- `services/agent/host.rs`: new — `PtyAgentHost` + `impl LocalAgentHost`
  (create/rename/delete/send_input/subscribe_session/agent-events/handle_hook/
  resume/stop_all/prepare_suspend/commit_suspend/notify_shutdown/debug_dump).
- `services/agent/state.rs`: new — `AgentServiceState`/`LocalAgentContext`
  registry (moved from `mod.rs`, gated at the `mod` site).
- `services/agent/mod.rs`: `LocalAgentHost` trait + `DebugAgent` (always
  compiled); `AgentServiceCtx` now `{ Option<host>, host_id, is_cloud }` and
  delegates; removed the create/rename/delete twins and moved helpers to host.
- `services/agent/session_rpc.rs`: retargeted `AgentServiceCtx` → `PtyAgentHost`,
  dropped all per-item gates (module gated) and the client-only stub.
- `services/{mod,client}.rs`, `server.rs`, `debug/server.rs`,
  `user_state.rs`, `services/startup/mod.rs`, `testnet/daemon.rs`: route
  through the host; `ServerState` holds `Option<Arc<dyn LocalAgentHost>>`,
  built lazily in `ensure_local_agent_host` (the host spawns a task, so it
  must be constructed inside the runtime, not in `ServerState::new`).
- `debug/server.rs`: consumes owned `Vec<DebugAgent>` from `host.debug_dump`
  (session detail rendered to JSON inside the host) instead of borrowing the
  registry guard.

### Decisions Made
- Host presence = the feature, not the role: `Some` whenever `local-agents`
  is compiled (cloud relays keep an empty registry); cloud-vs-device stays a
  runtime guard. So the host needs only `host_id` to construct.
- Keep suspend's pieces (`prepare`/`commit`/`notify`) as separate trait
  methods: the shutdown orchestration interleaves `notify_routing_peers`, so a
  merged `suspend()` could not preserve ordering.
- `routing/host.rs` capabilities (3 gates) stay: a compile-time capability
  fact consulted where no host handle exists.

### Verification
- `cargo build`/`clippy` (default + `--no-default-features` + `testnet`): clean
  (pre-existing `serve_*_tracked` / embedded-no-default warnings unrelated).
- `cargo test --lib`: 393 (default) / 299 (client-only) passed — including
  `attach_local_agent_events_populates_client_agent_model` (the event-delivery
  bridge through the host).
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check` (embedded, `default-features = false`): clean.

---

## 2026-06-16: Split agent data types from the runtime (seam prep)

### Summary
First chunk of the `local-agents` seam refactor. Separated the agent *data*
types from the *runtime*, so the runtime gates at one module declaration
instead of per item, and removed gates that `lib.rs`'s client-only
`allow(dead_code)` already makes unnecessary. `AgentRecord`, `SessionEvent`,
and `StopPolicy` (plus the `Agent: From<AgentRecord>` impls) moved to a new
ungated `agents/record.rs`; `agents/session.rs` is now pure runtime
(`AgentSession`/`StructuredInputTarget`) gated at `#[cfg] mod session`, with
its 8 internal `local-agents` gates removed. `suspend.rs` references only
ungated data, so its 7 gates were dropped outright — it simply compiles dead
in client-only builds.

### Changes
- `agents/record.rs`: new, holds `AgentRecord`/`SessionEvent`/`StopPolicy`.
- `agents/session.rs`: data types removed; per-item `local-agents` gates
  dropped (module gated at the `mod` site).
- `agents/mod.rs`: `mod record;`, `#[cfg] mod session;`, re-export data from
  `record`.
- `suspend.rs`: removed all 7 `local-agents` gates.

### Decisions Made
- Lean on the existing client-only `allow(dead_code)`: an item needs `#[cfg]`
  only if it would fail to *compile* (names a gated type), not merely if it is
  unused. This is why `suspend.rs` needs no gates.

### Verification
- `cargo build` / `--no-default-features`: clean, no warnings.
- `cargo test --lib`: 393 (default) / 299 (client-only) passed.

### Next Steps
- Introduce the `LocalAgentHost` trait + `PtyAgentHost`; route
  `AgentServiceCtx` and the core consumers through `Option<dyn LocalAgentHost>`.

---

## 2026-06-16: Gate the agent runtime at the AgentSession seam

### Summary
Collapsed the leaf-level `local-agents` gating into a single module seam.
Previously `AgentSession`, `StructuredInputTarget`, and `SuspendedAgent`
each carried a `Disabled` variant in client-only builds, forcing every
match arm into a three-way `#[cfg]` split ending in `unreachable!()`. Now
the agent-runtime types and their impls live entirely behind
`#[cfg(feature = "local-agents")]`: client-only builds don't compile them
at all, so the `Disabled` variants and ~20 `unreachable!()` arms are gone
and `session.rs` / `suspend.rs` read like the original. The data the
client still needs — `AgentRecord`, `SessionEvent`, `AgentType`,
`claude_io` — stays compiled. `AgentServiceCtx` / `AgentServiceState`
remain as the identity/event carrier; only their session-holding
internals (the `local_agents` map, the `lifecycle` module, the
create/rename/delete RPCs) are gated, with `FailedPrecondition` stubs in
the client build.

### Changes
- `agents/session.rs`, `suspend.rs`: gate `AgentSession`,
  `StructuredInputTarget`, `SuspendedAgent` + impls; remove `Disabled`.
- `services/agent/{mod,lifecycle}.rs`: gate the `lifecycle` module and the
  session-holding `AgentServiceState` methods; client-only `create` /
  `rename` / `delete` stubs; added a build-agnostic `local_agent_count()`.
- `services/mod.rs`, `agents/mod.rs`, `server.rs`, `debug/server.rs`:
  gate the suspend/shutdown re-exports and the debug-dump session details.

### Decisions Made
- Gate the type, not the arm: a `#[cfg]` belongs on a module/type/field,
  never inside a match. The `Disabled`/`unreachable!()` pattern was the
  symptom of gating at the wrong altitude.
- Keep `host_id` on `AgentServiceCtx` (Design Y): gating only the
  session-holding internals avoids rippling into the `ClientService` /
  startup / server construction for no functional gain.

### Verification
- `cargo build` / `clippy` (default + `--no-default-features`): clean.
- `cargo test --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check`: clean against the refactored lib.

---

## 2026-06-16: Decouple daemonish behaviors from local-agents; drop client-only marker

### Summary
Reworked the embedded-build gating along the right axes. The "daemonish"
behaviors (peer reachability links, periodic self-update poll, direct
dispatcher TCP listener, local client unix listener, sleep inhibition,
LAN/direct pairing) had been coupled to the `local-agents` compile
feature via a `runtime_profile` shim. Those are *runtime* concerns — a
cloud relay is directly reachable yet hosts no agents — so they now live
on the builder/config axis instead. `spawn_local_background_tasks` takes
a `with_daemon_tasks` flag (desktop daemon `true`, embedded `false`); the
listener/sleep gates revert to the existing `tcp_port` /
`prevent_idle_sleep` config, which already sat in the daemon-only `run()`
path. Deleted the `runtime_profile` module and the redundant
`client-only` marker (an empty feature — `default-features = false` is
the real signal).

### Changes
- `Cargo.toml`: removed `client-only`; documented that `local-agents`
  gates only PTY hosting, not daemonish behavior.
- `server.rs`: `spawn_local_background_tasks(..., with_daemon_tasks)`;
  reverted the sleep / TCP / unix-listener runtime_profile gates.
- `client/mod.rs`, `services/client.rs`, `services/reachability.rs`:
  removed the `runtime_profile` pairing/reachability guards.
- Deleted `runtime_profile.rs`.

### Decisions Made
- Daemonish = runtime, not a feature: keeps cloud-relay/desktop role
  switching in one binary; only `local-agents` (native `portable-pty`)
  earns a compile-time feature.

### Verification
- `cargo build -p amux` and `--no-default-features`: clean.
- `cargo test -p amux --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test -p amux --features testnet --test spec`: 43 passed.

### Next Steps
- Collapse the remaining leaf-level `local-agents` gating: gate the agent
  runtime at the `AgentSession` seam and remove the `Disabled` enum arms
  / `unreachable!()` noise in `session.rs` and `suspend.rs`.

---

## 2026-06-16: Client-only feature gating for embedded iOS builds (first cut)

### Summary
First cut of compile-time gating so the amux lib can build for iOS and
link into the React Native app without spawning local agents. Adds a
`local-agents` default feature (owns PTY-backed Claude/test-agent
spawning via `portable-pty`) plus a `client-only` marker; embedded
builds use `default-features = false`. iOS gets its own state/data/log
paths under the app container, and a `compile_error!` guard rejects iOS
builds that leave `local-agents` enabled. A `runtime_profile` module
centralizes the "is this a local-agent host" decision for listeners,
reachability, sleep inhibition, and periodic update checks.

### Changes
- `Cargo.toml`: `local-agents` (default) + `client-only` features;
  `portable-pty` now optional.
- Feature-gated the agent runtime (`agents::{session,pty,hook,...}`,
  `suspend`, `services::agent` hosting) with `Disabled` enum arms for
  the client build.
- `paths.rs`: iOS app-container state/data/log paths.
- `runtime_profile.rs`: centralized daemonish gates.
- `lib.rs`: `VERSION`/`PROTOCOL_VERSION` exports, iOS compile guard.
- `services/client.rs`: `CreateAgent` now checks the target host's
  supported agent types precisely (`FailedPrecondition` instead of a
  generic `Unreachable`).

### Verification
- `cargo build -p amux --no-default-features`: clean (client-only).
- The app bridge (`amuxapp/modules/amux-core`) builds against this with
  `default-features = false, features = ["client-only"]`.

### Next Steps
- Simplify: collapse the leaf-level gating to a single module seam, move
  the daemonish behaviors onto the builder/config runtime axis, and
  delete `runtime_profile` and the redundant `client-only` marker.

---

## 2026-06-11: Stop the pre-existing Windows test hang from eating 6-hour runners

### Summary
With the clap failure fixed, the Windows test leg ran further and hit a
hang that PREDATES the protocol rework (the 2026-06-07 run on old main
was killed at the same 6-hour limit): `embedded_shutdown_…` and
`embedded_suspend_…` never return on Windows — agent PTY teardown hangs
under ConPTY, the same class of issue that already keeps the Windows e2e
leg disabled. Both tests are now `#[cfg_attr(windows, ignore)]` with the
reason inline. And the "tests always run with a timeout" rule now applies
to CI itself: every job has `timeout-minutes` (10–30), so a future hang
fails in minutes instead of silently burning a 6-hour runner.

### Verification
- `cargo test -p amux --test embedded`: 10 passed (macOS).
- Windows leg verified by the subsequent CI run.

---

## 2026-06-11: Fix Windows CLI arg conflicts referencing the unix-only --via-ssh

### Summary
CI's windows-latest test leg caught five failing `amux pair` arg-parsing
tests: `qr`/`listen`/`connect` listed `via_ssh` in `conflicts_with_all`,
but `via_ssh` is `#[cfg(unix)]`-only, so on Windows clap's debug
assertion fired for a conflict against a nonexistent argument. The
conflict is now declared via `#[cfg_attr(unix, arg(conflicts_with =
"via_ssh"))]` on each of the three args. macOS-only local verification
could not have caught this; the CI matrix did its job.

### Verification
- `cargo test -p amux-cli`: 43 passed (macOS); Windows leg verified by
  the subsequent CI run.

---

## 2026-06-11: Protocol version resets to 1

### Summary
amux is unreleased and the wire owes nothing to history:
`PROTOCOL_VERSION` resets from the internal working number to **1** —
the protocol as designed is the first one that will ever ship. The
internal version labels scrubbed from code comments, docs, and this log
in favor of plain language ("the protocol rework", "the earlier
route-list routing").

### Verification
- Full local CI parity run (fmt --check, CI clippy, workspace build, lib
  tests, spec suite, e2e suite) before rebasing onto main.

---

## 2026-06-11: Docs housekeeping — three docs, repointed instructions, compacted log

### Summary
docs/ now holds exactly three files, one per audience: PROTOCOL.md (the
wire, for implementers), ARCHITECTURE.md (the system, for developers),
HOW_IT_WORKS.md (the mental model + trust story, for the documentation
website). The investigations and ops material moved to gitignored notes/
(deployment, transcript-redaction ×2, session-tracking research).
CLAUDE.md repointed at the three docs + the spec suite, and now carries
the working conventions (timeout-wrapped tests, DEVLOG-per-chunk, no
trailers, docs-vs-notes). The spec suite's main.rs module doc absorbed
the "how to add a test" guide, retiring notes/SPEC_TESTS_DESIGN.md;
notes/REFACTOR_PROGRESS.md (migration complete) deleted. DEVLOG
compacted: June 2026 entries (spec suite + protocol rework + docs) kept verbatim, the
~100 older entries summarized into era paragraphs — full text remains in
this file's git history.

### Changes
- docs/: transcript-redaction-spec/-feasibility, research-claude-session-
  tracking untracked and moved to notes/ (deployment.md was never
  tracked; moved too).
- CLAUDE.md rewritten; tests/spec/main.rs module doc expanded.
- notes/SPEC_TESTS_DESIGN.md, notes/REFACTOR_PROGRESS.md deleted.
- DEVLOG.md: 3,700 → ~840 lines.

### Verification
- Spec suite recompiled and green after the main.rs edit; CI clippy
  clean.

---

## 2026-06-11: Protocol spec grows its survivors; the website one-pager lands

### Summary
docs/PROTOCOL.md absorbed the three protocol-level survivors of the v5
spec — the SPAKE2 wire crypto (RFC 9382/edwards25519, transcript hash,
HKDF infos, sealed identities, opaque INVALID_PIN), the
versioning-is-equality rule, and the QR payload shape — plus a new "Why
it is shaped this way" section folding the load-bearing rationale from
the protocol-rework decision walkthrough (stateless relays, one proxy hop, only-Open-
allocates, no housekeeping acks, tunnels die with their link, deliberate
double encryption, PAKE over bearer tokens). With that folded, the
protocol-decisions working note is deleted; rejected-alternative history
lives in the June DEVLOG entries. New
docs/HOW_IT_WORKS.md: the human-readable one-pager for the documentation
website — mental model (devices not accounts, pairing as a one-time act,
links carry / tunnels authenticate) and the trust story (what a relay
structurally cannot do, local revocation, missing-by-design, executable
spec).

### Changes
- docs/PROTOCOL.md: pairing crypto paragraph, version rule, rationale
  section, header repointed (now ~150 lines).
- docs/HOW_IT_WORKS.md: new.
- the protocol-decisions working note deleted (gitignored; content folded).

### Verification
- Docs-only change; spec suite green on the prior commit re-verified
  (43 passed / 0 ignored).

---

## 2026-06-11: Replace the v5 networking spec with an architecture doc

### Summary
The tombstone chain is gone: `docs/NETWORKING.md` (the superseded 3,202-line
v5 spec), `docs/NETWORKING_PROGRESS.md` (its work ledger), and the three
pointer stubs (`NEW_ARCHITECTURE.md`, `architecture.md`,
`cloud_architecture.md`) are deleted; git history keeps them. In their
place, `docs/ARCHITECTURE.md` — a current-design system doc complementing
PROTOCOL.md: PROTOCOL.md owns the wire, ARCHITECTURE.md owns the system
(process/deployment shapes, identity & trust store, the two-server model,
the dispatcher's classification table, the service surface map, the
multi-tenant cloud deployment, and the LinkRegistry / RoutingCore /
TunnelPool / ConnectionManager layering). Disposition of NETWORKING.md's
material: §3–4 (threat model, identity, trust, two servers, dispatcher,
tenancy), §7 (CLI shape), §8.4/8.12/8.13, and the resource caps were
rewritten for the new design into ARCHITECTURE.md; §4.8, §5–6, §8.5–8.11 were
protocol-level and already superseded by PROTOCOL.md (the SPAKE2 crypto
detail of §5.2.1 survives only in `services/pairing.rs` for now); the
glossary, invariant catalog (§10), and reference implementation map (§12)
died with v5's vocabulary.

### Changes
- `docs/ARCHITECTURE.md` created; the five superseded docs `git rm`ed.
- `docs/PROTOCOL.md` status header now points at ARCHITECTURE.md instead
  of the deleted NETWORKING.md.
- Every `docs/NETWORKING.md §x.y` / `NETWORKING_REVIEW.md §6.x` citation in
  `crates/` re-pointed at PROTOCOL.md/ARCHITECTURE.md section names or
  rewritten as self-contained prose (spec chapter headers in
  `tests/spec/{identity,presence,routing,sessions,wire}.rs`; doc comments
  in `connection.rs`, `setup.rs`, `tunnel/pool.rs`, `testnet/daemon.rs`).
  The invariant catalog is not preserved as a numbered list; comments now
  say what they mean.
- `CLAUDE.md`/`AGENTS.md` still reference the deleted docs; they are
  re-pointed in a follow-up chunk.

### Verification
- Spec suite: 43 passed / 0 ignored. Lib tests: 394 passed.
- `cargo fmt --all`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `grep -rn "NETWORKING\|NEW_ARCHITECTURE\|cloud_architecture" --include="*.rs"
  --include="*.md" .` is clean outside `notes/`, DEVLOG history, and
  CLAUDE.md (deferred above).

### Next Steps
- Re-point CLAUDE.md and AGENTS.md at PROTOCOL.md + ARCHITECTURE.md.
- Ledger candidate: `RoutedStream::expect_stalled_open` in
  `testnet/daemon.rs` no longer has a caller — revocation now breaks
  streams instead of stalling them — and could be deleted.

---

## 2026-06-11: e2e runner catches up with the reworked config

### Summary
All 14 e2e tests failed after the protocol-rework chunks with one cause: the runner's
generated `local.yaml` still set `randomise_link_name`, deleted with wire
link names in chunk 2 (the config parser rejects unknown fields). Removed
the field from the template in `e2e-runner/src/executor.rs`.

### Verification
- `cargo run -p e2e-runner -- run`: 14 passed, 0 failed.

---

## 2026-06-11: protocol rework complete — docs graduate

### Summary
Closing docs pass for the protocol-rework implementation (chunks 1–5, commits
6735df6 → ccd7a25). `docs/PROTOCOL.md` graduates from "target design" to
the implemented spec, locked in by the prose suite in
`crates/amux/tests/spec/`. `docs/NETWORKING.md` is marked superseded
(v5, historical) with a banner saying exactly what the rework replaced and that
PROTOCOL.md + the spec suite win where they disagree.

### Changes
- `docs/PROTOCOL.md`: status header → implemented.
- `docs/NETWORKING.md`: supersession banner.

### Verification
- Final state on the rework branch: lib 394 passed; spec suite 43 passed /
  0 ignored; CI clippy clean; workspace build clean. The wire
  `Message.body` oneof matches PROTOCOL.md's vocabulary verbatim:
  Hello · HelloAck · NeighborUp · NeighborDown · TunnelOpen · TunnelData
  · TunnelClose · Reauth · LinkClose, plus `PairingService.Pair`;
  `PROTOCOL_VERSION = 6`.

### Next Steps
- Decide the fate of NETWORKING.md's still-accurate material (identity,
  trust store, two-server model, dispatcher): fold into PROTOCOL.md
  companions or rewrite as a current reference.
- §6 ledger follow-ups: move route activation's TLS handshake off the
  ConnectionManager events task (§6.12); `last_dial_error` changes push
  no event to subscription-only UIs (D15 note).

---

## 2026-06-11: protocol-rework chunk 5 — fire-and-forget Reauth; reachability shrinks to two fields

### Summary
The final protocol-rework code chunk: D12 (P5) and D15 (P7). Credential refresh is now
fire-and-forget — the daemon sends `Reauth { token }` on the cloud link at
expiry − 5 min and expects nothing back; the cloud's only answers are the
two things it can already say: silence (refresh accepted, the link
continues) or `LinkClose(AUTH_EXPIRED)` (the daemon reconnects with a
fresh token — the recovery path that exists anyway). `ReauthAck`, the 15s
ack-timeout state machine, and `ConnectorReauthState` are deleted; the
protocol never acknowledges housekeeping, it only signals state changes.
A healthy link, and every session riding its tunnels, is never disturbed
by refresh. Separately, the host-listing reachability surface shrinks to
two honest fields: `HostEntry` carries `online: bool` (routing-derived
presence) + `last_dial_error: optional string` (the last failed dial
attempt's outcome, cleared when a route comes up). The
`reachable/unreachable/unknown` enum, its three proto wrapper messages,
and the `StatusChanged` plumbing are deleted — nothing probes, so
"unknown" is `!online && last_dial_error.is_none()`, derived client-side
if anyone cares. This completes the reworked wire vocabulary: `Hello`/`HelloAck`
· `NeighborUp`/`NeighborDown` · `TunnelOpen`/`TunnelData`/`TunnelClose` ·
`LinkClose` · `Reauth` · `PairingService.Pair`.

### Changes
- `proto/amux/v1/amux.proto`: `ReauthAck` deleted from `Message.body`
  (`LinkClose` renumbered to 9); the three `HostReachability*` wrapper
  messages and `HostReachabilityStatus` deleted;
  `HostEntry.reachability_status` → `optional string last_dial_error`.
- `routing/connect/mod.rs`: `ConnectorReauthState` and
  `LINK_AUTH_REAUTH_RESPONSE_TIMEOUT` deleted; `LinkConnectorAuth` itself
  now holds the token in force and gains `refresh_deadline`/`send_refresh`
  (send the Reauth, adopt the refreshed token's expiry as the next
  deadline — no pending/awaiting state). The ack-timeout select arm and
  the `ReauthAck` body arm are gone; the refresh-send arm survives. On the
  acceptor, a good refresh extends auth silently; a bad token (validation
  failure or user mismatch) answers `LinkClose(AUTH_EXPIRED)` and closes.
- `routing/events.rs`: `HostReachabilityStatus` deleted;
  `HostReachabilityEvent::StatusChanged` deleted; `HostEntry` carries
  `last_dial_error`. `routing/core.rs::notify_host_status_changed`
  deleted; `connection.rs` record/clear_reachability_error keep the
  storage (it IS `last_dial_error`) but emit nothing.
- `services/client.rs`: both host-entry builders read the stored dial
  error straight off the connection manager (`stored_last_dial_error`);
  `host_reachability_status_to_wire` deleted; the pairing-candidate filter
  drops its `reachability_status.is_none()` clause (the trust-status check
  already said it); `publish_host_status_update` survives for trust
  transitions but returns nothing; `HostEventOutcome::Updated` deleted.
- `client/mod.rs`: `host_reachability_status_from_wire` and the
  status-presence validation rules deleted; `last_dial_error` decodes as a
  plain optional string. `HostReachabilityStatus` un-exported from
  `lib.rs`/`routing/mod.rs`; `amux-cli` test fixture updated (the CLI/UI
  never rendered the three-state beyond carrying the field).
- Spec: `cloud_peers_keep_communicating_across_a_jwt_expiry` strengthened
  per D12 — an echo session opened before the refresh point echoes again
  after `jwt.expired()` on the same stream/tunnel/link (tunnels die with
  links, so the surviving stream is proof of zero disturbance).

### Decisions Made
- Lib tests:
  `unauthenticated_acceptor_rejects_reauth_ack` deleted (message gone);
  `authenticated_acceptor_accepts_reauth_for_same_user` rewritten as
  `…_silently_extends_auth_on_reauth_for_same_user` (initial token expires
  inside the asserted quiet window, so silence also proves the extension);
  `…_rejects_reauth_for_different_user` rewritten to expect
  `LinkClose(AUTH_EXPIRED)`, plus a new invalid-token twin;
  `connector_sends_reauth_before_token_expiry` rewritten ack-free plus new
  `connector_schedules_the_next_refresh_from_the_refreshed_token` (locks
  the no-ack rescheduling rule); connection.rs
  `reachability_error_changes_emit_host_status_event` rewritten as
  `reachability_errors_are_stored_until_cleared` (storage, no events);
  client.rs status tests re-pointed at `last_dial_error`
  (`host_status_change_publishes_cached_reachability_error` folded into
  the list-hosts dial-error test — there is no status event to publish).
- `last_dial_error` changes do not push `HostUpdated` events (that was the
  deleted `StatusChanged` plumbing); listings re-read the storage on
  demand, which is what the harness verbs and UIs poll anyway.

### Verification
- `cargo build -p amux --features testnet` and `cargo build --workspace`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed.
- Spec suite: 43 passed / 0 ignored, ×2 runs (32.25s / 32.26s), with the
  strengthened JWT-expiry test.

### Next Steps
- Protocol-rework implementation complete (chunks 1–5: D10/D11 → D2/D13/D14 → D3a/P8 →
  D9 → D12/D15). Graduate the remaining doc work: PROTOCOL.md is current;
  NETWORKING.md still describes v5 and is superseded where they overlap.
- Ledger note: the connector trusts the refresher's `expires_at`
  unconditionally — a refresher that keeps minting tokens already inside
  the 5-minute refresh window drives back-to-back refreshes (pre-existing
  shape, now without even an ack to pace it).

---

## 2026-06-11: protocol-rework chunk 4 — one pairing protocol (SPAKE2), two secret deliveries

### Summary
P2/D9 plus the D13 rename: pairing is now ONE wire protocol —
`PairingService.Pair(stream PairMessage)`, the SPAKE2 exchange — with two
out-of-band secret-delivery mechanisms: a typed 6-digit PIN (no camera) and
a QR-carried 256-bit secret (scan → paired; the user never sees it). The QR
payload shrinks to `{host_id, cloud_url, secret}` — the pubkey drops out
because SPAKE2 provides mutual authentication from possession of the
secret. This is stronger, not just simpler: the old `PairByToken` flow sent
a bearer token through a pubkey-pinned TLS channel, so the secret crossed
the wire; now it never does. One-shot consumption, ~5-minute TTL, and the
5-attempt cap are uniform across both deliveries (the cap is moot for a
256-bit secret but harmless). The cloud-only token-ingress restriction
(review item D5) is deleted with the token protocol — the Pair stream is
admitted on any pre-trust pairing transport. `PairBySpake2` → `Pair` and
`PairBySpake2Message` → `PairMessage`: the qualifier was a fossil of the
deleted second protocol.

### Changes
- `proto/amux/v1/amux.proto`: `PairByToken` RPC + request/response deleted;
  `PairBySpake2(stream PairBySpake2Message)` → `Pair(stream PairMessage)`;
  `PairingError.Reason::INVALID_TOKEN` deleted (reasons renumbered);
  `PairQrCloudPeerRequest` is `{host_id, secret}`;
  `StartPairingResponse.secret` oneof carries `qr_secret` not
  `one_shot_token`.
- `pairing/mod.rs`: `PairMode` collapses to one secret kind — bytes plus
  attempt accounting (`failed/in_flight/reserved`) and an audit label.
  `PairSecret::Token`, `PairModeTokenAttempt`, `begin/complete/abort`
  token methods, `token_matches`, and `InvalidToken`/`NotTokenMode`/
  `NotPinMode` errors deleted. The attempt machinery drops its PIN
  qualifier (`PairModeAttempt`/`PairModeCommit`, `begin_attempt`,
  `record_failure`, `begin_commit`, `complete_success`,
  `PAIR_ATTEMPT_LIMIT`); `attempt.secret()` returns the SPAKE2 password
  bytes. New `start_qr_secret[_for_duration]` mints the 256-bit secret.
- `services/pairing.rs`: `pair_by_token` server method,
  `pair_by_token_initiator`, `commit_pairing_token`, and the cloud-only
  `token_pairing_request_reachability` deleted; the SPAKE2 initiator
  (`pair_initiator`) takes `secret: &[u8]`; one `pair_mode_status` mapper.
- QR-pinned TLS verifier mode deleted: `transport/tls.rs` loses
  `QrServerVerification`, `qr_pairing_channel_from_io`, and the
  `expected_pubkey` threading (`tunnel/pool.rs::qr_pairing_channel_via`,
  `connection.rs::cloud_qr_pairing_channel_to`). The initiator verification
  matrix is {trust-pinned, none}; the surviving anonymous-TLS channel
  helpers drop their `pin_` prefix (`pairing_channel*`,
  `pairing_channel_via`, `cloud_pairing_channel_to`).
- `services/client.rs`: `PairPinCloudPeer`/`PairQrCloudPeer` now share one
  `pair_cloud_peer_with_secret` path (channel → SPAKE2 → host-id match →
  commit); the QR pubkey/duplicate-pubkey preflight went with the pubkey.
  `pairing/qr.rs` encodes/parses the new payload; `client/mod.rs` has
  `PairingSecret::QrSecret` and the two-argument `pair_qr_cloud_peer`;
  `amux-cli` renders/consumes the new payload.
- Harness: `testnet/pairing.rs::QrPayload` is `{host_id, secret}`;
  `with_qr` drives the same admin RPC as before. Spec chapter prose updated
  to D9 (no behavioral flips; all 43 specs unchanged and green).
- Kept deliberately: `TunnelTransport::has_cloud_pairing_reachability`
  (the responder still learns `Reachability::Cloud` from the pairing
  tunnel's arrival link) and `StartPairing`'s "QR requires cloud mode"
  check (the minted payload's `cloud_url` is what makes a QR consumable
  today; the wire protocol itself no longer cares).

### Decisions Made
- Wrong-secret on the QR path reports the same opaque `INVALID_PIN` as the
  PIN path: one protocol, one failure surface.
- Lib tests that exercised `PairByToken` were deleted where the behavior
  died with the token (cloud-only ingress, token race/reservation, token
  self-pairing preflights) and rewritten on the unified path where the
  behavior survives (trust-save failure releasing the reservation,
  duplicate-pubkey rejection at staging, pubkey replacement incl. the D10
  in-flight-tunnel teardown, QR-secret one-shot consumption).

### Verification
- `cargo build --workspace` and `cargo build -p amux --features testnet`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed (was 402: token tests deleted,
  several consolidated into unified-path equivalents).
- Spec suite: 43 passed / 0 ignored, ×2 runs (~32.3s each), plus a third
  green run after prose-only spec comment updates.

### Next Steps
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger note: `StartPairing` still refuses QR mode without cloud config —
  revisit if a non-cloud QR consumer path (e.g. LAN rendezvous by host_id)
  ever exists.

---

## 2026-06-11: protocol-rework chunk 3 — every call is a tunnel, with an explicit lifecycle

### Summary
P8 + D3a: the tunnel protocol is now the link lifecycle one layer up —
`TunnelOpen { tunnel_id, src, dst }` · `TunnelData { tunnel_id, dst,
payload }` · `TunnelClose { tunnel_id, dst }`, with `tunnel_id` a plain
16-byte UUID. Only an Open allocates endpoint state (the reply address
travels exactly once, in the Open; cloud rate limiting keys on Opens);
Data for an unknown id is a principled drop — zero allocation, link stays
up — which closes B1 structurally; normal teardown sends TunnelClose
proactively from either end (rejection too: there is no open-ack, the
inner mTLS handshake is the acknowledgement). Relays forward all three
frame types statelessly by `dst`. The dual-path channel discipline is
deleted: every peer call rides a tunnel, including to adjacent peers
(`Route::Direct(link)` materializes a tunnel pinned to that link with
`dst = peer`). Because tunnels are opened by sending frames, and frames
flow both ways, every live link is callable from both ends — both sides
now record a Direct route at link establishment (the multi-tenant cloud
acceptor excepted: the relay records no routes), which makes the
SSH-pairing responder's call-back real (D6).

### Changes
- `proto/amux/v1/amux.proto`: `TunnelId`/`TunnelFrame` deleted;
  `TunnelOpen`/`TunnelData`/`TunnelClose` added; `Message.body` renumbered.
- `tunnel/types.rs`: `TunnelId` is a UUID newtype (initiator/nonce gone —
  "initiated by me" is now an explicit `ActiveTunnel.initiated` flag).
- `tunnel/mod.rs`: initiator endpoints send the Open lazily, just ahead of
  their first Data frame (a never-used tunnel never allocates remotely);
  hosted endpoints never open. `TUNNEL_FRAME_PAYLOAD_MAX` →
  `TUNNEL_DATA_PAYLOAD_MAX` (cap unchanged).
- `tunnel/pool.rs`: one inbound handler per frame type; retired-tunnel
  tombstones (`BoundedTunnelIdSet`, `RETIRED_TUNNEL_CAP`) and the cloud
  tunnel-id cache deleted — with explicit Opens there is nothing for a
  stale Data frame to allocate. Endpoint drop/retirement sends a
  best-effort TunnelClose on the pinned link (`LinkRegistry::
  send_best_effort`); `InboundClosed`/`IncomingTunnelsClosed`/
  `MissingTunnelId` error variants gone. New `channel_on_link` for Direct
  routes.
- `connection.rs`: `ChannelKey::Direct`, `RouteRuntimeState`,
  `register_direct`/`remove_direct` deleted; the pool keys on
  `(peer, Route)` and materialization is one path — open a tunnel.
  NeighborUp gained the same never-eagerly-tunnel-into-the-cloud guard
  ClaimUp had (§6.7).
- `routing/connect/mod.rs`: the `direct_channel` threading is gone;
  `run_established_connect` records `apply_direct_up` on both roles
  (`auth_session.is_none()` — i.e. everyone but the cloud acceptor).
- Harness: `WirePeer` scripts Open/Data/Close; testnet tracks *dialed*
  direct-link TCP sockets (`trusted_device_channel_tracked` +
  `ReachabilityLinkConnector::track_dialed_tcp`) so an in-process restart
  severs them — with acceptors recording routes, a leaked dialer socket
  kept the acceptor seeing a stopped daemon online. `over_ssh` now stands
  the post-pairing SSH link in with the test TCP transport, as production
  dials its stored reachability at commit time.

### Decisions Made
- Pre-planned spec flips: the SSH-responder test asserts
  `server.can_call(&laptop)` over the inbound link; the B1 `#[ignore]`
  marker became a real passing test (open → close → late data: dropped,
  link up); wire scripts rewritten to the new grammar.
- Additional flips, all structural consequences of the rework, called out here:
  `direct_beats_cloud_when_both_are_available` — the acceptor now routes
  back over the link itself, not via the cloud;
  `revocation_evicts_routes_and_breaks_in_flight_streams` (was
  `…strands_in_flight_streams`) — streams ride tunnels, tunnels die with
  links/TunnelClose, so the §6.5 stall is gone and the spec-intended
  prompt break is real; the SSH-relay lib test gives the responder a
  pinned entry for the initiator (D4: tunnel mTLS is uniform, SSH links no
  longer inherit transport trust); startup lib tests poll for active
  routes (activation now performs a real tunnel TLS handshake).

### Verification
- `cargo test -p amux --lib`: 402 passed.
- Spec suite (`--features testnet --test spec`): 43 passed / 0 ignored,
  ×2 runs (~32.3s each).
- `cargo build --workspace` clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.

### Next Steps
- Chunk 4 (P2/D9): pairing collapse to one SPAKE2 protocol; delete
  `PairByToken`, `INVALID_TOKEN`, the QR-pinned verifier mode.
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger candidates from this chunk: eager route activation now runs a
  device-TLS handshake serially on the events task (the §6.7 "move
  activation off the event loop" smell got heavier); link-up now costs
  two tunnel TLS handshakes (both ends activate eagerly) even if no call
  is ever made.

---

## 2026-06-11: protocol-rework chunk 2 — route by host id with adjacency-only events

### Summary
The core routing rewrite (D2, D14, the routing-relevant D13 renames;
PROTOCOL_VERSION = 6). Routes are now `Direct(link) | Via(relay_host)` and
the wire carries host-id-addressed frames under two structural rules:
advertise only adjacency (`NeighborUp`/`NeighborDown` claim strictly "I
have/lost a direct link to H") and forward only to adjacency (a frame for
`dst` is forwarded iff a direct link to `dst` exists, else dropped). Route
lists, prepend-on-forward, split-horizon, hop caps, route dedup, wire link
names, and the snapshot/delta phase distinction (`SnapshotComplete`,
pre-activation event buffering) are deleted. The neighbor snapshot is a
field of `Hello`/`HelloAccepted`, reconciled atomically with link
registration. Presence is a two-hop derivation; replies leave on the
tunnel's arrival link, which removes the §6.6 direction-dependent
black-hole structurally. `RoutingService` → `LinkService`. The dual-path
channel discipline (direct channels eager, Via tunnels lazy) survives
until chunk 3 (P8).

### Changes
- `proto/amux/v1/amux.proto`: `Route` message deleted; `TunnelFrame.dst`
  is a host id; `HostUp/HostDown` → top-level `NeighborUp { host }` /
  `NeighborDown { host_id, reason }` (no route field); `RoutingEvent`
  envelope and `SnapshotComplete` deleted; `Hello`/`HelloAccepted` lose
  link names and gain `neighbors`; `service RoutingService` →
  `service LinkService`.
- `routing/route.rs` deleted (898 + 443 lines of route-list machinery with
  `routing/core.rs` rewritten around host entries + claims); registry-level
  adjacency discipline moved into `routing/link_registry.rs::register`
  (snapshot diff + `NeighborUp` fanout under one lock).
- `tunnel/pool.rs`: forwarding is the registry lookup
  (`forward_to_peer`), non-awaiting — a congested peer link gets the
  existing try_send-or-request-close policy instead of stalling the origin
  link's inbound processing (head-of-line guard).
- `connection.rs`: `ChannelKey::{Direct(link), Via{target, relay}}`
  replaces route-keyed pooling; §6.2/§6.7 guards reduced to what the new
  model still needs.
- Startup/cloud (`services/startup/*`, `user_state.rs`): per-user
  registries fan out scoped `NeighborUp`s; the cloud is adjacency, not a
  host — neither side records a host entry for the other.
- Harness/spec: `WirePeer` speaks the new handshake; chain tests rewritten —
  `endpoints_call_each_other_through_a_chain_regardless_of_dial_direction`
  (the §6.6 pair collapsed; dial direction no longer matters) and
  `presence_reaches_exactly_two_hops_along_a_chain` (catalog 28b inverted:
  three hops out is deliberately invisible).

### Decisions Made
- Pre-planned spec flips only: 28b inversion, §6.6 collapse, handshake-
  snapshot wire tests. Everything else green with mechanical updates.
- Lib tests that asserted old observables were re-pointed at current ones:
  rate-limit/forwarding tests count `TunnelFrame` bodies (links also carry
  adjacency events now); cloud-startup tests wait on live links in the
  registries instead of host entries (the cloud never appears as a host).

### Verification
- `cargo test -p amux --lib`: 394 passed.
- Spec suite (`--features testnet --test spec`): 42 passed / 1 ignored,
  ×3 runs (~32.3s each).
- `cargo fmt`; CI clippy (`--workspace --all-targets --features
  amux/testnet -- -D warnings`) clean; `cargo build --workspace` clean.

### Next Steps
- Chunk 3 (P8 + D3a): every call a tunnel, `TunnelOpen`/`TunnelData`/
  `TunnelClose` with UUID ids, delete the dual-path materialization and
  `ChannelKey::Direct`; SSH-responder spec test flips cannot_call →
  can_call.

---

## 2026-06-11: protocol-rework chunk 1 — delete preserve_tunnel_id and GoAway drain; rename GoAway → LinkClose

### Summary
First implementation chunk of the protocol rework: the two pure deletions (D10/P3,
D11/P6). `preserve_tunnel_id` — the `Option<TunnelId>` threaded from the
dispatcher through the pairing service into `ConnectionManager` so a
same-host_id/different-pubkey replacement could keep the in-flight pairing
tunnel alive — is gone; teardown now tears down everything, and an initiator
that misses `PairingComplete` re-pairs. The GoAway drain machinery
(`drain_timeout_ms`, draining flags, draining-mode frame filtering, the
`LinkDraining`/`Draining` error variants) is gone, and the wire message is
renamed `GoAway` → `LinkClose { reason, error }`: receiving it means "the
link is closed now, here's why" — no grace period.

### Changes
- `proto/amux/v1/amux.proto`: `GoAway` → `LinkClose` (drops
  `drain_timeout_ms`), `GoAwayReason` → `LinkCloseReason` (values unchanged).
  PROTOCOL_VERSION deliberately not bumped — that lands with the full wire
  rewrite next chunk.
- `connection.rs`, `tunnel/pool.rs`, `services/pairing.rs`, `dispatcher.rs`,
  `transport/io.rs`, `tunnel/transport.rs`: every `*_preserving_tunnel`
  doubled method collapsed to its plain form; `BoxedGrpcAuth::PreTrustPairing`
  and `TunnelTransport` lose their `tunnel_id` fields.
- `routing/connect/mod.rs`: drain deadline/flag/filtering deleted; inbound
  `LinkClose` breaks the loop immediately (`PostHandshakeAction::Drain` →
  `LinkClosed`); `goaway_drain_duration` deleted.
- `routing/link_registry.rs`: `draining` writer flag, `mark_draining`,
  `is_draining`, `LinkRegistryError::Draining` deleted;
  `send_goaway_to_*` → `send_link_close_to_*` (no drain arg).
- `server.rs`: shutdown notification renamed; the 200 ms pre-abort sleep is
  kept as a purely local flush grace (`SERVER_LINK_CLOSE_FLUSH_TIMEOUT`).
- Testnet harness mirror `GoAwayReason` → `LinkCloseReason`,
  `expect_goaway` → `expect_link_close`; spec tests renamed accordingly.
- Tests: drain-behavior tests rewritten to assert immediate-close (or
  link-removed) semantics; the cloud pin/QR pairing startup tests no longer
  preload a stale key (they locked in the preserved-tunnel success); new
  `pair_by_token_replacement_retires_the_in_flight_pairing_tunnel` locks the
  D10 behavior directly.

### Decisions Made
- Per D10, a cloud pairing that triggers key replacement now fails at the
  initiator (timeout today; prompt once D3's TunnelClose lands) and the
  initiator re-pairs. Known v5-interim consequence: after the replacement the
  responder holds no route back to the initiator until a link flap, so the
  immediate re-pair over the same relay can black-hole — the new reply rule
  (replies ride the arrival link) removes this class structurally.
- Internal `routing::LinkCloseReason` (writer close requests) keeps its name;
  the wire enum converges on the same name per D11.

### Verification
- `cargo build -p amux --features testnet` clean; lib tests 449 passed; spec
  suite green twice (43 passed, 1 ignored, ~32s each); fmt + CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`) clean.

### Next Steps
- P1+P8 core rewrite: host-id routing, every-call-a-tunnel,
  TunnelOpen/TunnelData/TunnelClose, PROTOCOL_VERSION = 6.

---

## 2026-06-11: Protocol rework one-pager

### Summary
Concluded the protocol-simplification walkthrough (all proposals from the
networking review resolved) and wrote `docs/PROTOCOL.md` — the target
design as a one-pager. Core decisions: host-id routing with one proxy hop
through any relay (advertise-only-adjacency / forward-only-to-adjacency);
every peer call is a tunnel with pinned e2e mTLS inside (links grant
forwarding only, never call authority); QR pairing collapses into the
SPAKE2 flow as a QR-carried 256-bit secret; neighbor snapshot moves into
Hello/HelloAck (SnapshotComplete deleted); GoAway drain deleted and the
message renamed LinkClose; Reauth kept but ReauthAck deleted
(fire-and-forget refresh, so live sessions are never disturbed); TunnelClose
added; HostUp/Down → NeighborUp/Down, TunnelFrame → TunnelData,
RoutingService → LinkService, PairBySpake2 → Pair.

### Decisions Made
- Full rationale recorded in the protocol-decisions working note (D1–D15,
  local working notes). Notable rejections: link-scoped tunnel IDs (forces
  relay remap state), tunnels surviving link replacement (cross-link
  reordering), dropping Reauth entirely (hourly breaks of live cloud
  sessions).
- Amended same day (D3a): tunnel lifecycle made explicit —
  TunnelOpen/TunnelData/TunnelClose with plain UUID ids, no open-ack (inner
  TLS is the ack, TunnelClose the rejection); only Opens allocate endpoint
  state. Replaces implicit-open + `TunnelId{initiator, nonce}`; both
  lifecycle grammars (link/tunnel) are now symmetric.

### Verification
- Design-only change; no code. Spec suite unaffected (expected flips when
  the rework lands are pre-recorded in the decisions notes).

### Next Steps
- Implement in sequence: P3+P6 deletions → P1+P8 core rewrite → P2 →
  P5/P7, spec suite green throughout.

---

## 2026-06-10: Spec suite polish, harness docs, CI wiring

### Summary
Final pass over the spec-test suite: hoisted remaining plumbing out of test
bodies into harness verbs (`Pin::wrong_guess`, `WirePeer::flood_handshakes_
until_rate_limited`), documented the testnet harness at module level, ordered
chapter modules as documentation, and wired the suite into CI.

### Changes
- `crates/amux/src/testnet/` rustdoc on all public items; ~25-line module doc
  covering TestNet/Daemon/WirePeer and the eventually/restart/sever
  disciplines.
- `.github/workflows/ci.yml`: test job runs
  `cargo test -p amux --features testnet --test spec`; clippy covers the
  feature.

### Verification
- Spec suite green twice (43 passed, 1 ignored, ~32s); lib 451 passed; fmt +
  clippy (CI invocation) clean.

### Next Steps
- Decide whether `notes/SPEC_TESTS_DESIGN.md` and `notes/NETWORKING_REVIEW.md`
  (gitignored, referenced from committed code comments) should be force-added.

---

## 2026-06-10: Spec chapters 5–6 — remote sessions, authority, wire conformance

### Summary
Catalog items 30–39: remote agent attach with IO round-trips over direct and
cloud routes, session survival across topology churn, full runtime authority
for paired peers, local-admin RPC rejection over remote transports, and a
`WirePeer` scripted protocol actor exercising the real TCP listener +
dispatcher + TLS for the wire-conformance chapter.

### Changes
- `crates/amux/tests/spec/{sessions,wire}.rs`; `src/testnet/{session,wire}.rs`.
- Production: only `#[cfg(test)]` → `#[cfg(any(test, feature = "testnet"))]`
  widenings in `agents/` and `services/agent/session_rpc.rs`.

### Decisions Made
- Item 38's race half (review finding B1: a benign tunnel-close race can
  escalate to a link-level GoAway) is an `#[ignore]`d test documenting the
  bug; the deterministic unknown-tunnel-drop case asserts the spec'd behavior.

### Verification
- Spec suite 43 passed / 1 ignored across 3 consecutive runs; lib 451 passed;
  fmt + clippy clean.

---

## 2026-06-10: Spec chapters 3–4 — presence, routing & failover

### Summary
Catalog items 15–29 (+28b): presence lifecycle, per-user isolation at the
relay, pairing-candidate scoping, direct-beats-cloud selection, failover in
both directions, make-then-break swaps with in-flight stream breakage,
startup re-establishment, revocation teardown, 3- and 4-node relay chains,
and JWT-expiry continuity driven through the real Reauth path with
short-TTL testnet tokens.

### Changes
- `crates/amux/tests/spec/{presence,routing}.rs`; harness verbs `stop`,
  `sees_offline`, `cannot_call`, pairing-candidate observers,
  `open_event_stream_to` (`RoutedStream`), `.cloud_user(...)`,
  `.trusted(...)`, expiring-JWT support in the testnet relay.
- Product fixes found by the suite (NETWORKING_REVIEW.md §6.7/§6.9):
  `connection.rs` no longer eagerly materializes multi-hop routes to relay
  hosts (head-of-line blocked the routing-event loop for the 10s handshake
  timeout); `tunnel/pool.rs` route removal no longer retires healthy hosted
  inbound tunnels on make-then-break swaps.

### Decisions Made
- Tests lock current behavior where it diverges from docs/NETWORKING.md, with
  NOTE comments: dropped-route streams fail with `h2 protocol error` (not
  UNAVAILABLE); revocation strands in-flight streams both ways; chained
  relaying is dial-direction-dependent (acceptor-side HostUp suppression).

### Verification
- Spec suite 32 passed across 5 consecutive runs; lib 451 passed (incl. new
  regression tests for both product fixes); fmt + clippy clean.

---

## 2026-06-10: Spec chapters 1–2 — identity, trust, pairing; harness hardening

### Summary
Catalog items 1–14: identity persistence across restart, trust locality, all
three pairing flows (PIN direct/cloud, QR, SSH over an in-process stream
seam), wrong-PIN opacity, the 5-attempt cap, pair-mode TTL and one-shot
consumption, self-pairing rejection, and key rotation. Pairing verbs drive
the real local-admin RPCs, not trust-store pokes.

### Changes
- `crates/amux/tests/spec/{identity,pairing}.rs`; `src/testnet/pairing.rs`.
- Harness hardening after a hang hunt: `eventually()` now bounds both the
  condition poll and the failure dump; routed-call verbs are
  timeout-bounded (`can_call` for post-churn assertions); `restart()` severs
  tracked inbound and outbound sockets so peers observe a real process death
  (aborting the connector task alone leaks the established link on both
  ends — documented finding).
- Product fix (NETWORKING_REVIEW.md §6.2): a stale
  `ConnectionManager::activate_route` could demote a fresh direct route and
  permanently strand its pooled channel; activation now refuses
  longer-or-equal demotion.

### Verification
- Spec suite 16 passed, 3 consecutive runs, zero hangs; lib 450 passed.

---

## 2026-06-09: Testnet spec-test harness and smoke tests

### Summary
Built the `amux::testnet` harness (behind a `testnet` feature) and the
`tests/spec` integration target per `notes/SPEC_TESTS_DESIGN.md`: whole-daemon
in-process networks with real localhost TCP + mTLS and a real cloud relay,
declarative `TestNet` builder with trust pre-pairing, eventually-assertions
with topology failure dumps, and operator verbs (cloud outage, link sever,
restart). Two smoke tests cover the canonical example and the operator verbs.

### Changes
- `crates/amux/src/testnet/{mod,net,daemon,assertions}.rs`;
  `crates/amux/tests/spec/`; `testnet` feature + `[[test]] spec` target.
- Production: cfg-gate widenings plus feature-gated accessors and a tracked
  TCP-accept seam in `dispatcher.rs` (no behavior changes).

### Decisions Made
- Assembly generalizes the existing `services/startup/mod.rs` integration
  tests rather than inventing a new path; no skip-TLS anywhere.
- Daemon bring-up is sequenced (direct links, then cloud) to avoid a
  route-activation race discovered during construction — later root-caused
  and fixed as NETWORKING_REVIEW.md §6.2.

### Verification
- `cargo test -p amux --features testnet --test spec` — 2 passed, stable
  across 10 runs; lib 450 passed; clippy + fmt clean.

---

## Earlier history (compacted 2026-06-11)

Entries below this point were compacted into era summaries; the full
entries remain in this file's git history (any commit before the
compaction).

### 2026-05 → 2026-06-08: Networking & Security v1 (protocol v5)
The full v5 networking architecture landed directly as commits (devlog
gap): gRPC `RoutingService.Connect` bidi links, route-list stack routing,
`TunnelFrame` tunnels carrying end-to-end pinned mTLS, SPAKE2 + token
pairing, the trust store, the two-server model with dispatcher
classification, and the multi-tenant cloud relay (TLS+JWT). Anchor
commits: fd02e8e ("Implement v1 routing and service architecture"),
681ee49 ("Implement amux Networking & Security v1"), 707f620. All of it
was specified in docs/NETWORKING.md (since deleted) and superseded by
the protocol rework — see the June entries above.

### 2026-05-15: Embedded-library refactor
`amux` reshaped into a library for embedded clients plus `amux-ui` (a
reactive client runtime); public API surfaced deliberately; cleanup pass
behind it.

### 2026-04: Hardening and idiomatic-Rust era
Wire enums reshaped for non-Rust clients with forward-compatibility
`Unknown` variants; per-client minimum-version enforcement; CLI
ergonomics (`server` subcommand, positional attach, suspend/resume);
pre-release security hardening; a multi-pass idiomatic-Rust refactor and
domain reshuffle (opaque hook handling, facade cleanup, server-giant
splits); init flow + cloud-state reshape; negotiated idle timeouts for
heartbeats; subscription leases; `amux debug` rework.

### 2026-03: Agents and sessions era
Workspace split into crates with a public API; `AgentSession` enum +
`PtyHandle`; suspend/resume with persisted state; `amux update`;
graceful shutdown; Windows support behind a platform abstraction; the
Claude hook-event family (PreToolUse/PostToolUse/Notification/Stop) and
the AskUserQuestion keystroke work; structured I/O sequence numbers;
external session capture (fork-and-swap); protocol reset to v1 with
handshake extraction and per-user sockets.

### 2026-02: Custom-protocol era
Cloud mode over WebSocket + MessagePack; the protocol-v3 restructure
(command enum, opaque payloads, request_id, route management,
SubscriptionClosed); agent/host discovery (AnnounceAgent/AnnounceHost);
user multi-tenancy (`ServerUserState`); the Claude Code plugin with
auto-install hooks; the tracing migration and the zero-clippy policy;
TCP keepalives; protocol version checking on Connect.

### 2026-01: Prototype era
The initial PTY-multiplexer prototype; milestone 1 with the e2e testing
framework; TCP transport and remote subscriptions; hooks, structured
logs, and the dashboard; link-based stack routing — the design whose
relay statefulness the rework finally resolved by routing on host ids.
