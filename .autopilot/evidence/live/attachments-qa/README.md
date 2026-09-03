# Composer and feed live QA

Date: 2026-09-03

## Result

Blocked before the chat TUI opened. The local daemon started, but both supported
Claude launch drivers failed while creating an agent with:

`failed to create agent: failed to start local agent <uuid>: managed Claude MCP launch route is no longer valid`

Because no chat screen was rendered, the requested pasted-text token, clipboard
image token, mixed draft, sent prompt attachment block, and OS image viewer could
not be exercised. The five required checkpoint captures are retained as honest
tmux captures of the failed launch attempt:

![pasted-text checkpoint](frames/01-pasted-text-token.txt)
![clipboard-image checkpoint](frames/02-clipboard-image-token.txt)
![mixed-draft checkpoint](frames/03-mixed-draft.txt)
![sent-prompt checkpoint](frames/04-sent-prompt-attachment-block.txt)
![OS-viewer checkpoint](frames/05-os-viewer-image.txt)

## Steps and exact output

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

