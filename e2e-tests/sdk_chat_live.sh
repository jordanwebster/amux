#!/bin/sh
# Opt-in live Claude conversation, asks and family capture. Uses the operator's
# login and real inference. Run serially with the other live harnesses.
# Usage: timeout 1500 e2e-tests/sdk_chat_live.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=sdk-chat
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
# Refuse stale frames from a previous attempt: a failed replay must never look
# complete because a later step's capture survived from another run.
out=${1:-$repo_root/.autopilot/evidence/live/sdk-chat}
[ ! -e "$out/transcript.txt" ] || { printf 'Use a fresh evidence directory: %s\n' "$out" >&2; exit 1; }
live_init "$out"
rows_bin=$repo_root/target/debug/examples/sdk_rows
mcp_bin=$repo_root/target/debug/claude-probe
for executable in "$rows_bin" "$mcp_bin"; do
  [ -x "$executable" ] || live_fail "$executable is missing; run wt build"
done
command -v claude >/dev/null 2>&1 || live_fail 'Claude Code is required'
live_say "Claude: $(timeout 15 claude --version)"

live_config conversation
config=$scratch/host-conversation.yaml
printf 'claude:\n  driver: sdk\n' >> "$config"
project=$scratch/project
mkdir -p "$project" "$scratch/claude-config"
git -C "$project" init -q
printf 'CURRENT\n' > "$project/README.md"
# Keep test settings, sessions and project MCP configuration out of the user's
# configuration directory. Authentication still uses the normal secure store.
export CLAUDE_CONFIG_DIR=$scratch/claude-config
export CLAUDE_SECURESTORAGE_CONFIG_DIR=''
export DISABLE_AUTOUPDATER=1 DISABLE_UPDATES=1 DISABLE_INSTALLATION_CHECKS=1
unset CLAUDECODE
cat > "$CLAUDE_CONFIG_DIR/settings.json" <<'EOF'
{"model":"haiku","enableAllProjectMcpServers":true,"sandbox":{"enabled":false},"permissions":{"defaultMode":"default","allow":["mcp__external__ask_the_operator"]}}
EOF
python3 - "$project/.mcp.json" "$mcp_bin" <<'PY'
import json, pathlib, sys
pathlib.Path(sys.argv[1]).write_text(json.dumps({"mcpServers": {"external": {
    "type": "stdio", "command": sys.argv[2], "args": [],
    "env": {"CLAUDE_SPEC_MCP_SERVER": "1"}, "alwaysLoad": True}}}))
PY

sdk_rows() {
  # Atomic replacement keeps a failed read from destroying the last useful
  # capture. Rows are the daemon's envelopes, including their sequence IDs.
  timeout 15 "$rows_bin" "$config" "$1" > "$evidence_dir/$2.tmp"
  mv "$evidence_dir/$2.tmp" "$evidence_dir/$2"
}
sdk_wait() {
  # sdk_wait agent rows-file since predicate [argument...]
  sdk_agent=$1; sdk_file=$2; sdk_since=$3; shift 3
  sdk_deadline=$(($(date +%s) + 180))
  while [ "$(date +%s)" -lt "$sdk_deadline" ]; do
    sdk_rows "$sdk_agent" "$sdk_file"
    if sdk_match=$(python3 "$repo_root/e2e-tests/sdk_chat_rows.py" "$evidence_dir/$sdk_file" "$sdk_since" "$@"); then
      live_say "Observed $sdk_agent: $* (row/request $sdk_match)"
      return 0
    fi
    sleep 1
  done
  live_fail "$sdk_agent did not produce $* after row $sdk_since"
}
parent_rows=claude_sdk_v1.rows.jsonl
child_rows=child.claude_sdk_v1.rows.jsonl
sdk_prompt() {
  sdk_rows chat-parent "$parent_rows"
  since=$(python3 "$repo_root/e2e-tests/sdk_chat_rows.py" "$evidence_dir/$parent_rows" 0 cursor)
  live_wait_pane 30 chat 'enter send · ctrl+j newline'
  live_say "Prompt: $1"
  live_tmux send-keys -t chat -l -- "$1"
  # Incoming family messages can start a turn between typing and Enter. Keep
  # the one draft and retry Enter until its exact prompt row appears; an empty
  # composer cannot resubmit it. Baseline later checks at that row, excluding
  # any completion that arrived while this prompt was still in the composer.
  prompt_deadline=$(($(date +%s) + 90))
  while [ "$(date +%s)" -lt "$prompt_deadline" ]; do
    live_wait_pane 30 chat 'enter send · ctrl+j newline'
    live_tmux send-keys -t chat Enter
    sleep 1
    sdk_rows chat-parent "$parent_rows"
    if prompt_seq=$(python3 "$repo_root/e2e-tests/sdk_chat_rows.py" "$evidence_dir/$parent_rows" "$since" prompt "$1"); then
      since=$prompt_seq
      live_say "Prompt accepted at row $since."
      return 0
    fi
  done
  live_fail 'the typed prompt was not submitted'
}
sdk_finish() {
  sdk_wait chat-parent "$parent_rows" "$since" result
  live_wait_pane 30 chat 'enter send · ctrl+j newline'
  live_frame chat "$1"
}

live_start "$config"
live_new creator "$config" "$project" claude --name chat-parent -- --model haiku --permission-mode default
live_wait_list "$config" chat-parent
sdk_wait chat-parent "$parent_rows" -1 ready
live_tmux kill-session -t creator
live_fleet_select chat "$config" chat-parent
live_frame chat fleet-open
live_tmux send-keys -t chat Enter
live_wait_pane 60 chat 'enter send · ctrl+j newline'
live_frame chat conversation-idle

sdk_prompt 'Without tools, write thirty short numbered sentences about trees. Start with TREES_BEGIN and end with TREES_END.'
sdk_wait chat-parent "$parent_rows" "$since" stream
live_wait_pane 10 chat 'send gated while working'
live_frame chat conversation-streaming
sdk_wait chat-parent "$parent_rows" "$since" assistant TREES_END
sdk_finish conversation-reply

# A long text generation keeps the interruption inside an active turn without
# needing to authorize a shell command or infer liveness from a spinner alone.
sdk_prompt 'Without using tools, write 100 numbered facts about trees, each at least thirty words long. Begin directly with fact 1.'
sdk_wait chat-parent "$parent_rows" "$since" stream
live_wait_pane 10 chat 'send gated while working'
live_frame chat interrupt-working
live_say 'Keys: ctrl+x interrupts the active generation.'
live_tmux send-keys -t chat C-x
sdk_wait chat-parent "$parent_rows" "$since" interrupted
live_wait_pane 30 chat 'enter send · ctrl+j newline'
live_frame chat conversation-interrupted

sdk_prompt 'Use the Write tool once to create permission.txt containing exactly WRITE_ALLOWED. Do not use Bash or any other tool. After it succeeds reply exactly WRITE_FINISHED.'
sdk_wait chat-parent "$parent_rows" "$since" permission Write
request=$sdk_match
live_wait_pane 30 chat '1. Allow once'
live_frame chat ask-permission
live_tmux send-keys -t chat 1 Enter
sdk_wait chat-parent "$parent_rows" "$since" resolved permission "$request" allow
live_wait_file 30 "$project/permission.txt" WRITE_ALLOWED
cp "$project/permission.txt" "$evidence_dir/permission.txt"
sdk_wait chat-parent "$parent_rows" "$since" assistant WRITE_FINISHED
sdk_finish permission-continued

# Cycle only after each mode change is acknowledged, or a second key could
# still be based on the old header fact.
live_tmux send-keys -t chat BTab
sdk_wait chat-parent "$parent_rows" "$since" mode acceptEdits
live_tmux send-keys -t chat BTab
sdk_wait chat-parent "$parent_rows" "$since" mode plan
sdk_prompt "Plan changing README.md's only line from CURRENT to PLANNED, then call ExitPlanMode. Do not ask questions. Once approved, make the change and reply exactly PLAN_APPROVED."
sdk_wait chat-parent "$parent_rows" "$since" permission ExitPlanMode
request=$sdk_match
live_wait_pane 30 chat 'Approve — manual'
live_frame chat ask-plan
live_tmux send-keys -t chat 1 Enter
sdk_wait chat-parent "$parent_rows" "$since" resolved permission "$request" allow
live_wait_file 60 "$project/README.md" PLANNED
cp "$project/README.md" "$evidence_dir/planned-readme.txt"
sdk_wait chat-parent "$parent_rows" "$since" assistant PLAN_APPROVED
sdk_finish plan-continued

sdk_prompt "Use AskUserQuestion to ask exactly one single-select question with header Color, question 'Which color do you prefer?', and options Red and Blue in that order. Then repeat my answer."
sdk_wait chat-parent "$parent_rows" "$since" permission AskUserQuestion
request=$sdk_match
live_wait_pane 30 chat '2. Blue'
live_frame chat ask-question
live_tmux send-keys -t chat 2 Enter
sdk_wait chat-parent "$parent_rows" "$since" resolved permission "$request" allow
sdk_wait chat-parent "$parent_rows" "$since" tool-result Blue
sdk_finish question-continued

sdk_prompt 'Call mcp__external__ask_the_operator with word PELICAN, then reply with exactly what the tool returned.'
sdk_wait chat-parent "$parent_rows" "$since" elicitation
request=$sdk_match
live_wait_pane 30 chat 'Confirm the word PELICAN.'
live_frame chat ask-elicitation
# Use a different answer from the supplied word so the returned text proves
# the user's form content travelled all the way back through the MCP server.
live_tmux send-keys -t chat -l -- 'HERON'
live_frame chat elicitation-filled
live_tmux send-keys -t chat Tab Enter
sdk_wait chat-parent "$parent_rows" "$since" resolved elicitation "$request" accept
sdk_wait chat-parent "$parent_rows" "$since" tool-result HERON
sdk_wait chat-parent "$parent_rows" "$since" assistant HERON
sdk_finish elicitation-continued

sdk_prompt 'Use mcp__amux__spawn to create a claude child named chat-child in this directory. Give it this exact prompt: Use Write once to create child.txt containing CHILD_WRITE_ALLOWED, then send chat-parent the message CHILD_TO_PARENT using mcp__amux__send. If you later receive PARENT_TO_CHILD, send chat-parent CHILD_ACK using mcp__amux__send. Do not use Bash. After spawning, reply CHILD_SPAWNED. Do not send anything to the child yet, and do not automatically respond to its messages.'
live_wait_list "$config" chat-child
sdk_wait chat-child "$child_rows" -1 permission Write
child_request=$sdk_match
sdk_wait chat-parent "$parent_rows" "$since" assistant CHILD_SPAWNED
sdk_finish parent-child-spawned
live_wait_pane 30 chat chat-child
live_tmux send-keys -t chat C-a a
live_wait_pane 30 chat '1. Allow once'
live_frame chat child-ask-in-parent
live_tmux send-keys -t chat 1 Enter
sdk_wait chat-child "$child_rows" -1 resolved permission "$child_request" allow
sdk_wait chat-parent "$parent_rows" "$since" message CHILD_TO_PARENT chat-child
sdk_wait chat-parent "$parent_rows" "$since" completed chat-child
live_tmux send-keys -t chat Escape
live_frame chat parent-receives-child

sdk_prompt 'Use mcp__amux__send to send chat-child exactly PARENT_TO_CHILD. Then reply PARENT_SENT. When it replies CHILD_ACK, report that acknowledgement without sending any more messages.'
sdk_wait chat-child "$child_rows" -1 message PARENT_TO_CHILD chat-parent
sdk_wait chat-parent "$parent_rows" "$since" message CHILD_ACK chat-child
sdk_finish parent-exchange
live_tmux send-keys -t chat C-a n
live_wait_pane 30 chat chat-child
live_wait_pane 30 chat '→ chat-parent · CHILD_ACK'
live_frame chat child-exchange
live_tmux send-keys -t chat C-a s
live_wait_pane 30 chat chat-parent
live_tmux send-keys -t chat / C-c Escape
live_tmux send-keys -t chat z
live_wait_pane 30 chat chat-child
live_frame chat family-fleet
timeout 30 "$amux_bin" --config "$config" ls --all > "$evidence_dir/family-inventory.txt"
live_assert_inventory "$evidence_dir/family-inventory.txt" chat-parent=claude/sdk chat-child=claude/sdk
sdk_rows chat-parent "$parent_rows"
sdk_rows chat-child "$child_rows"
live_say 'PASS: live conversation, streaming, interruption, Write, plan, question, MCP elicitation and parent-child exchange captured.'
