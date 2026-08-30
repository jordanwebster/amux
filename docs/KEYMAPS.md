# Claude PTY keymaps

Claude's interactive terminal accepts keystrokes, but amux clients send
semantic intents: prompt, interrupt, permission-mode cycle, or an answer to a
named permission, plan, or question ask. The daemon resolves a versioned
keymap, chooses the binary-owned program for the intent, validates its facts
and text, and writes the resulting key steps to the PTY.

The selected keymap, source, full SHA-256 digest and resolution basis appear in
an `amux.claude.keymap` row at session start and after transcript relinks. Each
`amux.claude.input_result` names the keymap, basis, fixed program and number of
bytes written. Clients never author Claude escape sequences or menu digits.

## What data can and cannot change

A keymap is data. It can change:

- named key bytes and bounded delays;
- menu entry positions;
- the provider version range to which it applies;
- verified menu shapes, currently permission suggestion counts;
- the steps inside the six fixed programs; and
- descriptive provenance and capture-backed verified-version entries.

The binary owns the intent types, ask facts, program names, intent-to-program
mapping, step vocabulary, conditions and validation rules. A keymap must
contain exactly the six roots `prompt`, `interrupt`, `mode_cycle`,
`permission_menu`, `plan_menu`, and `question_form`, with the fixed mapping.
It cannot add a new intent, interaction kind, fact, condition or step. Changing
that behavior requires a new amux binary.

This data-versus-binary limit is deliberate: a data file can repair keys,
timing, menus and already-modeled shapes without becoming an extension language
that can perform arbitrary terminal automation.

## Resolution policy

The daemon resolves against the Claude version observed for the session after
merging baked keymaps with `<data_dir>/keymaps`. A user keymap shadows a baked
keymap with the same declared `name`; filenames do not define identity.

Selection is ordered:

1. Prefer a keymap with an exact verified entry for the observed version. The
   basis is `Verified(version)`.
2. Before a keymap has any verified anchor, prefer the newest recorded keymap
   whose `applies_to` range contains the version. The basis is `InRange`.
3. Once verified anchors exist, extrapolate from the nearest verified version
   below the observed version, or the nearest one above when none is below. The
   basis is `Extrapolated(from version)` even if a broad range also matches.
4. If no keymap has a verified version, select the newest recorded keymap with
   basis `Unknown`.

Programs declare `stable` or `menu` stability. Stable programs may run for an
extrapolated or unknown version. Menu programs may extrapolate only within the
same provider minor version; they refuse outside it. Unknown menu programs
also refuse. Separately, ask facts must match the program's verified shapes,
or the input fails before any bytes are written with an unverified-shape
reason.

The shipped `claude-2.1` keymap has the bounded range
`>=2.1.228, <2.2.0`. It is not a promise that every later Claude version has the
same UI. Live verification anchors and per-program stability express the
stronger evidence.

## Closed step language

Programs are interpreted from this bounded vocabulary:

| Step | Meaning |
|---|---|
| `key` | Write one named, validated key byte sequence. |
| `paste` | Validate text, wrap it in bracketed paste, and write it to the composer. |
| `type` | Validate single-line text and type it into a menu text field. |
| `digit` | Select a digit derived from a named menu entry, selected option, Other row, or permission suggestion. |
| `delay` | Wait for a named duration, capped by the binary. |
| `repeat` | Repeat nested steps a typed, bounded count. |
| `for_each` | Iterate over typed questions or selected options. |
| `if` | Choose between branches using one of the fixed typed conditions. |
| `move_to` | Move the interpreter's per-question cursor to a selected option or Other row. |
| `call` | Call another fixed program; recursive call graphs are rejected. |

Counts and branches come only from the current typed ask and answer. The
interpreter has no general loop, shell command, arbitrary variable, or screen
query. `paste` and `type` reject unsafe control bytes before any output;
`type` also rejects newlines. Prompts and deny feedback use bracketed paste,
while plan feedback and Other text use typed menu fields.

## Provenance and verification

Each keymap records the provider version, model, dates and executable
specifications from which its behavior was transcribed. A verified-version
entry adds a provider version, probe run id and specification name. Only the
Claude probe may append those entries, and only after that specification
passes live. A provenance test requires a matching recorded or verification
ledger entry in a Claude PTY manifest.

User keymaps cannot claim verified versions. `amux keymap add` accepts an exact
inherited baked ledger only to make an override practical, then strips the
ledger from the installed user copy. It rejects a changed or invented verified
entry. The installed copy has its own content digest, so session rows identify
the exact data that encoded an input.

## Managing user keymaps

`amux keymap` manages the configured data directory:

- `amux keymap list` lists each resolved name, source, applicable range and the
  basis for the installed Claude version.
- `amux keymap show <name>` prints the selected TOML.
- `amux keymap add <file>` validates and installs a user keymap.
- `amux keymap remove <name>` removes the user override by declared name.
- `amux keymap dir` prints `<data_dir>/keymaps`.

Malformed files, unknown fields or references, a changed fixed program table,
recursive calls, unsafe key bytes and hand-authored verification all fail with
an error naming the offending origin or field.

## No screen detection

Keymaps do not inspect Claude's rendered terminal. The daemon has no terminal
screen model: it does not parse VT output, recognize panes or dialogs, handle
interstitial screens, or wait for a visible state. Programs use typed hook and
transcript facts plus fixed delays.

Consequently, an unforeseen dialog is neither detected nor handled. A program
may type into a dialog it was not designed for, including a new intermediate
dialog; failure becomes visible only later as a missing confirmation row or a
wrong outcome. The user's recovery path is raw attach. A terminal screen model
with pane recognition and state-aware waits is the named follow-up for
detecting and surviving such dialogs; it is outside the current keymap system.
