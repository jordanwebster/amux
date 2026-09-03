# Composer and feed live QA

Date: 2026-09-03

## Result

Passed the live same-host attachment flow after explicitly selecting the
worktree daemon configuration inside tmux. All five frames are canonical
120x40 `tmux capture-pane` outputs from the running QA session:

![pasted-text checkpoint](frames/01-pasted-text-token.txt)
![clipboard-image checkpoint](frames/02-clipboard-image-token.txt)
![mixed-draft checkpoint](frames/03-mixed-draft.txt)
![sent-prompt checkpoint](frames/04-sent-prompt-attachment-block.txt)
![OS-viewer checkpoint](frames/05-os-viewer-image.txt)

## Steps and exact output (successful rerun)

The existing worktree daemon was verified with:

```text
$ timeout 30 target/debug/amux --config /Users/jlw/.wt/trees/amux/diffs/.wt/amux/config.yaml list
Running agents:
  attachments-qa [claude/pty] - /Users/jlw/.wt/trees/amux/diffs
```

The crucial launch command was passed explicitly inside the pane/session setup;
the fleet and chat entry point used:

```text
$ tmux new-window -t attachments-qa -n fleet '/bin/zsh'
$ tmux send-keys -t attachments-qa:fleet 'target/debug/amux --config /Users/jlw/.wt/trees/amux/diffs/.wt/amux/config.yaml ui' C-m
```

The pane was resized to 120x40 and opened the `attachments-qa` agent. In the
composer, I bracket-pasted a nine-line report, pressed Ctrl+V with
`images/global_architecture.png` on the macOS clipboard, combined both, and
sent `what is wrong here` with the image token. Each checkpoint was captured
with `tmux capture-pane -p -S -40 -t <pane> > frames/<name>.txt`.

The model replied that it received image 16, described concrete diagram issues,
and did not produce a permission request. The OS viewer check ran in a 120x40
tmux shell pane:

```text
$ open -a Preview /Users/jlw/.wt/trees/amux/diffs/images/global_architecture.png
OS viewer opened: Preview
Preview
```

The earlier failed-launch attempt and its diagnosis remain below for
traceability; they are not the result of this rerun.

1. From the repository root, checked the CLI and Claude version:

   ```text
   $ timeout 60 target/debug/amux --help
   Terminal multiplexer for AI agents (Claude, Codex, etc.)
   $ timeout 60 claude --version
   2.1.259 (Claude Code)
   ```

2. Started the local server:

   ```text
   $ timeout 60 target/debug/amux server start
   Server started.
   ```

3. Created a 120x40 tmux session and ran the real entry point:

   ```text
   $ tmux new-session -d -s attachments-qa -x 120 -y 40 /bin/zsh
   $ tmux send-keys -t attachments-qa 'target/debug/amux new claude --name attachments-qa' C-m
   Error: failed to create agent: failed to start local agent bb829892-2b96-4e7a-b38d-8cebdc1e829e: managed Claude MCP launch route is no longer valid
   ```

4. Retried with the alternate supported driver:

   ```text
   $ target/debug/amux new claude --driver sdk --name attachments-sdk
   Error: failed to create agent: failed to start local agent 0ff20b2b-4a12-49a6-a026-bc8a0ecbf2ac: managed Claude MCP launch route is no longer valid
   ```

5. Repeated the default launch five times and captured the 120x40 pane after
   each attempt. The files in `frames/` are the resulting pane captures. No
   attachment bytes were pasted or sent because the composer did not exist.

Defect filed as autopilot Task 40.

## Task 40 diagnosis and unblock

The launch code was not the source of the error. The long-lived tmux server did
not carry this worktree's `AMUX_CONFIG`, so commands in its panes used
`~/.config/amux/config.yaml` and connected to the default daemon instead. That
daemon had been started from
`/Users/jlw/.wt/trees/amux/debugging/target/debug/amux`; the file had since been
removed, so its immutable managed MCP route correctly failed validation.

After restarting this worktree's daemon and passing the config explicitly, both
drivers created and registered an agent without the route error:

```text
target/debug/amux --config /tmp/amux-task40-chat.yaml new claude --name task40-pty-proof
target/debug/amux --config /tmp/amux-task40-chat.yaml new claude --driver sdk --name task40-sdk-proof
```

The PTY driver reached the idle composer in `task40-pty-open.txt`. The SDK
driver reached its existing chat placeholder in `task40-sdk-open.txt`; `amux
list` showed both agents running. Future tmux QA must pass `--config` explicitly
or export `AMUX_CONFIG` inside the pane rather than relying on tmux's global
server environment.
