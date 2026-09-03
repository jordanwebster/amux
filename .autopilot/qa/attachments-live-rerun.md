# Live attachments QA rerun

Date: 2026-09-03

Result: pass for the same-host composer, feed, model-delivery, and OS-opener
checkpoints. No defect was found, so no defect task was filed.

The prior launch blocker was avoided by passing the worktree config explicitly
inside tmux:

```text
target/debug/amux --config /Users/jlw/.wt/trees/amux/diffs/.wt/amux/config.yaml ui
```

The 120x40 chat pane accepted a nine-line bracketed paste as one atomic token,
accepted a PNG from the macOS clipboard via Ctrl+V, and displayed both tokens in
one draft. The sent prompt showed an attachment row, and the model described
the architecture PNG while explicitly making no permission request.

![pasted text token](../evidence/live/attachments-qa/frames/01-pasted-text-token.txt)

![clipboard image token](../evidence/live/attachments-qa/frames/02-clipboard-image-token.txt)

![mixed draft](../evidence/live/attachments-qa/frames/03-mixed-draft.txt)

![sent prompt attachment row and model response](../evidence/live/attachments-qa/frames/04-sent-prompt-attachment-block.txt)

![OS viewer confirmation](../evidence/live/attachments-qa/frames/05-os-viewer-image.txt)

Exact opener check from a 120x40 tmux shell pane:

```text
open -a Preview /Users/jlw/.wt/trees/amux/diffs/images/global_architecture.png
OS viewer opened: Preview
Preview
```

Each frame was saved using `tmux capture-pane -p -S -40 -t <pane>` and has 40
rows. The five frame files are the required evidence artifacts.
