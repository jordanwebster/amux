# Attachments and diff reviews

**Status**: implemented (2026-09-04). This document owns the message-level
attachment model, its canonical text syntax, artifact storage and lifetime,
delivery to agents, and attachment-specific deferred decisions.
[`CHAT.md`](./CHAT.md) owns the terminal interaction and presentation;
[`ARCHITECTURE.md`](./ARCHITECTURE.md) owns the service boundaries; and
[`A2A.md`](./A2A.md) owns the complete model-facing tool set.

## Vocabulary and model

An **attachment** is part of a chat message. Its four closed kinds are Image,
File, Text, and Review. It is represented by an `amux-attachment` element in
the message text, so prompts and replies use the same representation on every
host.

An **artifact** is a stored byte blob. Its closed kinds are Image, File, and
Diff. Image and File attachments refer to artifacts; a Review refers to the
Diff artifact it was made against; Text stays inline and has no stored blob.
Artifact metadata is introduced once, when bytes are put, and copied into
stream refs. A message's pin list contains identities only and never repeats
the metadata.

A **document** is typed content opened in the TUI reader. Text and Review
attachments are documents. Image and File attachments instead open in the
viewing host's operating-system viewer.

The distinction is intentional: attachments belong to messages, artifacts
give bytes a lifetime and location, and documents are a viewing model.

## Canonical element syntax

Artifact-backed elements are self-closing. Their identity is the lowercase
SHA-256 of their bytes, prefixed with `sha256:`:

```text
<amux-attachment id="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" kind="image" name="screenshot.png" size="120433"/>
<amux-attachment id="sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789" kind="file" name="trace.json" size="8192"/>
```

`size` is bytes. A daemon may add a `path` attribute while materialising a
prompt on the artifact's owning host. The parser accepts that form, but the
canonical element stored in a draft or returned by the `attach` tool has no
path:

```text
<amux-attachment id="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" kind="image" name="screenshot.png" size="120433" path="/agent-state/artifacts/blobs/012345…"/>
```

Pasted text and reviews carry a body:

```text
<amux-attachment kind="text" name="pasted-1" lines="240">the pasted text…</amux-attachment>
```

```text
<amux-attachment kind="review" diff="sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210" base="working-tree" head="4f2a9c1" comments="1">blobs: [["src/lib.rs","a1b2c3"]]
## src/lib.rs @@ old:12..new:13
&gt; -old call
&gt; +new call
text-bytes: 11
Use helper.</amux-attachment>
```

Attribute values and bodies escape XML-significant characters. Unknown
attributes, missing required attributes, invalid identities, mismatched review
comment counts, and malformed bodies make the whole candidate ordinary prose;
the parser never silently drops message text. The formatter produces one
canonical spelling that the parser round-trips.

### Review body and identity

A review freezes the patch when the page opens. The opening element names the
Diff artifact, base, reviewed `head`, optional `merge-base`, and comment count.
For a working-tree review, the first body line records the available new-side
Git blob hashes for changed paths. That identity says exactly what was reviewed
even if the tree changes later.

Each comment heading records `path`, then inclusive `start_side:start_line` and
`side:line` endpoints. Sides are `old` or `new`. The following `> ` lines quote
the selected diff rows verbatim, including their diff signs. `text-bytes`
frames the UTF-8 comment text so arbitrary newlines or heading-like text cannot
be mistaken for another comment. Comments have no approve/request-changes
verdict; surrounding prose is the review's general comment.

The non-visual review model and parser live in `amux-ui`. It preserves files,
hunks, numbered row facts, anchors, quoted rows, comments, and base identity.
The TUI owns only cursor, selection, folds, overlays, editor, scroll, width,
and painting.

## Composing, rendering, and opening

The composer holds mentions as atomic tokens. Cursor movement crosses a token
in one step and one backspace removes the whole token and its draft attachment.
Several kinds can appear in one draft. Ctrl+C clears text and tokens together;
ask takeovers, scrolling, and phase changes preserve both.

Bracketed paste turns text into a Text token at 8 lines or 1000 characters;
shorter text remains ordinary composer text. Ctrl+V inspects the clipboard: an
image becomes an Image token, an existing file path becomes a File token, and
text follows the same long-paste rule. Each artifact is limited to 10 MiB.

The feed splits prompts and replies into prose and attachment blocks. Together,
the inline element and synthetic refs row give every subscribed viewer enough
metadata to show kind, name, size, line count, and review comment/file counts
without fetching bytes. Opening is lazy: Image and File bytes are fetched,
verified, cached, then given to the local OS; Text opens in the reader; Review
fetches its frozen patch and opens with inline comments. If a review's patch is
unavailable, the reader still shows its comments and quoted rows with a
missing-diff notice.

## The artifact crate: owner and cache

`amux-artifacts` has no dependency on `amux`. Both of its roles use the same
layout: `blobs/<sha256 hex>` for content-addressed bytes and `index.json` for
metadata, persisted atomically. Reads rehash bytes; a missing or invalid index
is recovered by rescanning and rehashing the blob directory.

The **Owner** is authoritative for one agent at
`<data_dir>/agents/<agent-id>/artifacts`. A daemon opens existing owners at
startup and otherwise on first touch, then keeps each loaded index in memory.
`put` hashes bytes and is idempotent for the same content. A new artifact is
ephemeral. Pinning the identities named by a sent message makes them survive
the one-hour ephemeral TTL; the daemon sweeps loaded owners every five minutes
without a directory scan. A pinned artifact survives daemon restart and is
deleted with its agent.

The **Cache** belongs to a viewing host. It fetches on a miss, verifies the
response against the requested identity, persists last-use times, and evicts
least-recently-used blobs until it is within its byte bound. Corrupt cached
bytes are treated as a miss and fetched again; corrupt fetched bytes are
rejected.

The viewing-host cache is one flat shared directory for every agent at `<cache_dir>/amux/artifacts`, uses LRU eviction as its only cleanup, and defaults to 256 MiB under the ordinary config key `ui.artifact_cache_mib`.

These are two roles of one crate, not two sources of truth: only the agent's
host owns an artifact; viewing hosts hold disposable verified copies.

## RPCs, stream refs, and send ordering

Both `AgentService` and the routing `ClientService` expose the same three unary
operations. `ClientService` forwards remote calls to the agent's owning host:

| RPC | Request | Result |
|---|---|---|
| `PutArtifact` | agent, kind, name, MIME, bytes | authoritative `ArtifactRef` |
| `GetArtifact` | agent, artifact id | `ArtifactRef` and bytes |
| `Diff` | agent and `WorkingTree` or branch base | stored Diff ref, unified patch, per-file magnitudes, and base identity |

`Diff` runs in the agent's working directory for every agent kind. Working-tree
means the tree against `HEAD`, including untracked files as additions. Branch
means the current branch against the merge base of the selected base.

`SendInput` carries `pin: [artifact-id…]`. The client puts every live draft
artifact first and sends the provider input only after all puts succeed. The
daemon validates the complete list before changing any lifetime, pins it
atomically, and emits this recipient-owned structured row before delivery:

```json
{"type":"amux.attachments","input_id":"<hex input id>","refs":[{"id":"sha256:…","kind":"image","name":"screenshot.png","mime":"image/png","size":120433}]}
```

An `attach` tool call uses a null `input_id`. On session open, the owner emits
one row containing all pinned refs in creation order, so a reconnecting viewer
can render and open old attachments. Bytes never ride the subscription stream.

An empty pin list is a hard boundary: message text passes to the backend
byte-for-byte, even if it happens to contain an attachment-like element. With
pins, only image/file elements whose id is in the explicit list gain a local
path. A Review pins its Diff artifact but its inline body is not parsed or
rewritten by the daemon.

All artifact RPC clients and servers accept unary messages up to 16 MiB. Store
failures map to typed missing, too-large, corrupt, and diff-unavailable protocol
errors. The TUI states the error and restores the complete draft rather than
claiming a send succeeded.

## Provider delivery

- **Claude PTY** receives image and file elements with owner-local paths.
  Managed launches preapprove reads below that agent's artifact directory, so
  opening an attachment does not trigger a permission prompt.
- **Claude SDK** receives inline Text and Review elements unchanged and images
  as native base64 image blocks.
- **Codex** receives owner-local paths in image and file elements, plus a
  native `localImage` item for each image.

This materialisation happens only on the owning host. A remote TUI never needs
the agent's filesystem path and cannot make one up.

## The agent `attach` tool

Managed Claude and Codex agents receive the same MCP tool:

```json
{"name":"attach","input":{"path":"/absolute/or/working-dir/file","name":"optional display name"}}
```

Its schema is `{path: string, name?: string}` with no extra fields. The MCP
process reads the path on the agent's host, recognizes PNG, JPEG, GIF, and WebP
extensions as images, and otherwise uses a generic File with
`application/octet-stream`. The daemon puts and pins the bytes for the calling
agent, writes the null-input refs row, and returns the exact canonical
`amux-attachment` element as a string. When the model includes that result in a
reply, every viewer renders the same attachment block.

The tool authenticates from the managed process environment; agent and host
identities are not tool arguments.

## Deferred decisions

These surfaces are intentionally absent. Each existing constraint keeps its
addition possible without changing the shipped meaning:

- **Terminal image thumbnails** — the feed already has an Image placeholder
  and opening produces a verified local cache path; a kitty, sixel, or iTerm2
  renderer can consume that path without changing storage or message syntax.
- **Attachments on agent-to-agent `send`** — the shared element syntax and
  content-addressed owner are independent of human prompts; a future envelope
  can add an explicit pin list while preserving authenticated provenance.
- **“Edits this turn” as a diff base** — `DiffBase` is a closed enum and the
  review header records the chosen base, so this can be another variant rather
  than a new review format.
- **A fetch-by-ID model tool** — `GetArtifact` and owner-side identity checks
  already provide the operation; path materialisation remains today's model
  delivery and a later tool can be added to the same authenticated MCP server.
- **Comment re-anchoring after refresh** — V1 freezes the patch, while quoted
  rows, old/new endpoints, and base identity retain the facts a future
  re-anchoring algorithm would need.
- **Diff search and syntax highlighting** — the review core preserves typed
  rows separately from terminal painting, so both remain presentation-layer
  additions.
- **A side pane beside chat** — `ReviewView` owns width and focus and the chat
  only hosts it; splitting the available layout does not require a new review
  model.
- **Mobile and other rich clients** — the RPCs, structured refs row, canonical
  elements, and dependency-light artifact crate expose the data without
  depending on terminal code.
- **Message queueing, slash commands, and other composer grammar** — attachment
  tokens coexist with ordinary text, while Tab, `/`, and `@` remain available
  for the deferred grammar described in `CHAT.md`.

Two adjacent implementation boundaries are also deliberate. Claude SDK chat
viewing remains unsupported even though daemon-side attachment delivery works,
and wiring the iOS bridge to `amux-artifacts` is separate client work; keeping
the crate free of `amux` dependencies makes that adoption possible.
