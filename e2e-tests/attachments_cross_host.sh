#!/bin/sh
# Live acceptance for a review authored and reopened from remote viewing hosts.
#
# This uses the operator's logged-in Claude installation. It keeps every amux
# daemon, identity, trust store, repository, and cache below a private scratch
# root; Claude's own transcript remains in its normal project history, as it
# does for the other opt-in live harnesses.
#
# Usage: e2e-tests/attachments_cross_host.sh [evidence-directory]
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
amux_bin=${AMUX_BIN:-"$repo_root/target/debug/amux"}
evidence_arg=${1:-"$repo_root/.autopilot/evidence/live/review-cross-host"}
case "$evidence_arg" in
  /*) evidence_dir=$evidence_arg ;;
  *) evidence_dir=$repo_root/$evidence_arg ;;
esac
frames_dir=$evidence_dir/frames
agent_name=review-cross-host
reply_token=REVIEW_CROSS_HOST_RECEIVED
run_id=$$
agent_tmux=amux-review-agent-$run_id
viewer_u_tmux=amux-review-viewer-u-$run_id
viewer_v_tmux=amux-review-viewer-v-$run_id
pair_pid=
scratch=
a_config=
u_config=
v_config=

say() {
  printf '%s\n' "$*"
}

fail() {
  say "FAIL: $*" >&2
  exit 1
}

cleanup() {
  code=$?
  trap - EXIT HUP INT TERM
  if [ -n "$pair_pid" ]; then
    kill "$pair_pid" >/dev/null 2>&1 || true
  fi
  for session in "$agent_tmux" "$viewer_u_tmux" "$viewer_v_tmux"; do
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
  done
  for config in "$a_config" "$u_config" "$v_config"; do
    if [ -n "$config" ] && [ -f "$config" ] && [ -x "$amux_bin" ]; then
      timeout 15 "$amux_bin" --config "$config" server stop >/dev/null 2>&1 || true
    fi
  done
  if [ -n "$scratch" ]; then
    case "$scratch" in
      /tmp/amux-review-cross-host.*|/private/tmp/amux-review-cross-host.*)
        rm -rf -- "$scratch"
        ;;
    esac
  fi
  exit "$code"
}
trap cleanup EXIT HUP INT TERM

[ -x "$amux_bin" ] || fail "amux binary not found at $amux_bin; run cargo build -p amux-cli"
command -v tmux >/dev/null 2>&1 || fail "tmux is required"
command -v claude >/dev/null 2>&1 || fail "Claude Code is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required for JSON validation"

mkdir -p "$frames_dir"
scratch=$(mktemp -d /tmp/amux-review-cross-host.XXXXXX)
chmod 700 "$scratch"
a_config=$scratch/host-a.yaml
u_config=$scratch/host-u.yaml
v_config=$scratch/host-v.yaml
project_dir=$scratch/project

allocate_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}

a_port=$(allocate_port)
u_port=$(allocate_port)
v_port=$(allocate_port)

write_config() {
  config_path=$1
  host_name=$2
  socket_path=$3
  state_path=$4
  data_dir=$5
  tcp_port=${6:-}
  {
    printf "host_name: '%s'\n" "$host_name"
    printf "socket_path: '%s'\n" "$socket_path"
    printf "state_path: '%s'\n" "$state_path"
    printf "data_dir: '%s'\n" "$data_dir"
    printf 'enable_cloud_mode: false\n'
    printf 'prevent_idle_sleep: false\n'
    printf 'ui:\n'
    printf '  default_open_mode: raw\n'
    printf '  color: ansi\n'
    if [ -n "$tcp_port" ]; then
      printf 'tcp_port: %s\n' "$tcp_port"
    fi
  } > "$config_path"
}

write_config "$a_config" host-a "$scratch/a.sock" "$scratch/a-state.yaml" "$scratch/a-data" "$a_port"
write_config "$u_config" host-u "$scratch/u.sock" "$scratch/u-state.yaml" "$scratch/u-data" "$u_port"
write_config "$v_config" host-v "$scratch/v.sock" "$scratch/v-state.yaml" "$scratch/v-data" "$v_port"

for config in "$a_config" "$u_config" "$v_config"; do
  timeout 30 "$amux_bin" --config "$config" init >/dev/null
done

AMUX_LOG=$scratch/a.log timeout 30 "$amux_bin" --config "$a_config" server start >/dev/null
AMUX_LOG=$scratch/u.log timeout 30 "$amux_bin" --config "$u_config" server start >/dev/null
AMUX_LOG=$scratch/v.log timeout 30 "$amux_bin" --config "$v_config" server start >/dev/null

wait_file_text() {
  seconds=$1
  file=$2
  text=$3
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    if [ -f "$file" ] && grep -Fq -- "$text" "$file"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

wait_list_text() {
  seconds=$1
  config=$2
  text=$3
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    if timeout 10 "$amux_bin" --config "$config" list 2>/dev/null | grep -Fq -- "$text"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

pane_text() {
  tmux capture-pane -p -t "$1"
}

wait_pane_text() {
  seconds=$1
  pane=$2
  text=$3
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    if pane_text "$pane" | grep -Fq -- "$text"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

wait_pane_without_text() {
  seconds=$1
  pane=$2
  text=$3
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    if ! pane_text "$pane" | grep -Fq -- "$text"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

wait_pane_occurrences() {
  seconds=$1
  pane=$2
  text=$3
  wanted=$4
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    count=$(pane_text "$pane" | grep -Fo -- "$text" | wc -l | tr -d ' ')
    if [ "$count" -ge "$wanted" ]; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

capture_frame() {
  pane=$1
  name=$2
  if tmux has-session -t "$pane" 2>/dev/null; then
    pane_text "$pane" > "$frames_dir/$name"
  else
    printf 'tmux session ended before capture: %s\n' "$pane" > "$frames_dir/$name"
  fi
}

pair_from_a() {
  responder_config=$1
  responder_port=$2
  label=$3
  pair_log=$scratch/pair-$label.log
  : > "$pair_log"
  timeout 90 "$amux_bin" --config "$responder_config" pair --listen > "$pair_log" 2>&1 &
  pair_pid=$!
  wait_file_text 20 "$pair_log" 'Pairing PIN:' || fail "$label did not publish a LAN PIN"
  pin=$(sed -n 's/^Pairing PIN: //p' "$pair_log" | head -n 1)
  [ -n "$pin" ] || fail "could not parse LAN PIN for $label"
  printf '%s\n' "$pin" | timeout 60 "$amux_bin" --config "$a_config" pair --connect "127.0.0.1:$responder_port" > "$scratch/pair-$label-client.log" 2>&1 || {
    sed -n '1,120p' "$scratch/pair-$label-client.log" >&2
    fail "LAN PIN pairing failed for $label"
  }
  wait "$pair_pid" || {
    sed -n '1,120p' "$pair_log" >&2
    fail "$label did not finish LAN PIN pairing"
  }
  pair_pid=
}

say 'Pairing agent host A to viewing hosts U and V by LAN PIN...'
# A initiates both links and therefore persists both direct reachabilities. Its
# restart below must redial the still-running viewing hosts without re-pairing.
pair_from_a "$u_config" "$u_port" u
pair_from_a "$v_config" "$v_port" v

mkdir -p "$project_dir"
git -C "$project_dir" init -q --initial-branch=main
git -C "$project_dir" config user.email live-review@example.invalid
git -C "$project_dir" config user.name 'amux live review'
printf 'alpha heading\nalpha old value\nalpha tail\n' > "$project_dir/alpha.txt"
printf 'beta heading\nbeta old value\nbeta tail\n' > "$project_dir/beta.txt"
git -C "$project_dir" add alpha.txt beta.txt
git -C "$project_dir" commit -q -m 'Seed live review fixture'
printf 'alpha heading\nalpha new value\nalpha tail\n' > "$project_dir/alpha.txt"
printf 'beta heading\nbeta new value\nbeta tail\n' > "$project_dir/beta.txt"

model=${AMUX_CLAUDE_LIVE_MODEL:-haiku}
tmux new-session -d -s "$agent_tmux" -x 120 -y 40 -c "$project_dir" \
  "$amux_bin" --config "$a_config" new claude --name "$agent_name" -- \
  --model "$model" --dangerously-skip-permissions
wait_list_text 90 "$a_config" "$agent_name" || {
  capture_frame "$agent_tmux" 00-agent-launch-failure.txt
  fail "managed Claude agent did not start"
}
if wait_pane_text 30 "$agent_tmux" 'Yes, I trust this folder'; then
  tmux send-keys -t "$agent_tmux" Down Enter
  wait_pane_without_text 30 "$agent_tmux" 'Quick safety check' || {
    capture_frame "$agent_tmux" 00-agent-trust-failure.txt
    fail 'managed Claude agent did not accept workspace trust'
  }
fi
wait_list_text 60 "$u_config" "$agent_name" || fail "host U did not discover the remote agent"
wait_list_text 60 "$v_config" "$agent_name" || fail "host V did not discover the remote agent"
capture_frame "$agent_tmux" 00-agent-ready.txt

open_chat() {
  session=$1
  config=$2
  tmux new-session -d -s "$session" -x 120 -y 40 "$amux_bin" --config "$config"
  if ! wait_pane_text 60 "$session" "$agent_name"; then
    capture_frame "$session" "00-$session-fleet-failure.txt"
    timeout 15 "$amux_bin" --config "$config" list > "$frames_dir/00-$session-list-failure.txt" 2>&1 || true
    fail "$session did not show the remote agent"
  fi
  tmux send-keys -t "$session" o
  wait_pane_text 30 "$session" 'C-a r review diff' || fail "$session did not open structured chat"
}

say 'Opening host U structured chat and authoring two cross-row comments...'
open_chat "$viewer_u_tmux" "$u_config"
open_chat "$viewer_v_tmux" "$v_config"
tmux send-keys -t "$viewer_u_tmux" -l -- "Do not edit files. Read both inline review comments and reply with exactly $reply_token."
tmux send-keys -t "$viewer_u_tmux" C-a r
wait_pane_text 30 "$viewer_u_tmux" 'review · working tree' || {
  capture_frame "$viewer_u_tmux" 01-review-open-failure.txt
  fail 'host U did not open the review page'
}
capture_frame "$viewer_u_tmux" 01-host-u-review-open.txt

# In each file, two j presses reach the removed row; v then j selects the
# removed row and the added row immediately below it.
for key in j j v j c; do tmux send-keys -t "$viewer_u_tmux" "$key"; done
tmux send-keys -t "$viewer_u_tmux" -l -- 'Replace alpha deliberately; keep its caller contract.'
tmux send-keys -t "$viewer_u_tmux" Enter
wait_pane_text 15 "$viewer_u_tmux" '1 comment' || fail 'first review comment was not saved'
tmux send-keys -t "$viewer_u_tmux" ']'
for key in j j v j c; do tmux send-keys -t "$viewer_u_tmux" "$key"; done
tmux send-keys -t "$viewer_u_tmux" -l -- 'Replace beta deliberately; preserve its result shape.'
tmux send-keys -t "$viewer_u_tmux" Enter
wait_pane_text 15 "$viewer_u_tmux" '2 comments' || fail 'second review comment was not saved'
capture_frame "$viewer_u_tmux" 02-host-u-two-comments.txt

tmux send-keys -t "$viewer_u_tmux" q
sleep 1
capture_frame "$viewer_u_tmux" 03-host-u-review-token.txt
wait_pane_text 15 "$viewer_u_tmux" 'C-a r resume review' || fail 'review token did not return to the draft'
tmux send-keys -t "$viewer_u_tmux" Enter
wait_list_text 20 "$a_config" "$agent_name" || {
  capture_frame "$agent_tmux" 04-agent-ended-after-send.txt
  fail 'host A lost the Claude agent after the review send'
}

say 'Opening the sent review in a second remote viewer...'

open_review_reader() {
  pane=$1
  # With no focus the first older-block chord chooses the newest block. Walk
  # backward until the review attachment accepts the open chord.
  attempt=0
  tmux send-keys -t "$pane" C-a o
  while [ "$attempt" -lt 30 ]; do
    if pane_text "$pane" | grep -Fq -- 'review — working tree'; then
      return 0
    fi
    tmux send-keys -t "$pane" C-a k
    tmux send-keys -t "$pane" C-a o
    sleep 1
    attempt=$((attempt + 1))
  done
  return 1
}

open_review_reader "$viewer_v_tmux" || {
  capture_frame "$viewer_v_tmux" 04-viewer-open-failure.txt
  capture_frame "$viewer_u_tmux" 04-viewer-u-at-v-failure.txt
  for log in "$scratch"/a.log "$scratch"/u.log "$scratch"/v.log; do
    if [ -f "$log" ]; then
      cp "$log" "$frames_dir/04-$(basename "$log")"
    fi
  done
  timeout 15 "$amux_bin" --config "$a_config" debug daemon --verbose --format json > "$frames_dir/04-host-a-debug.json" 2>&1 || true
  timeout 15 "$amux_bin" --config "$v_config" debug daemon --verbose --format json > "$frames_dir/04-host-v-debug.json" 2>&1 || true
  capture_frame "$agent_tmux" 04-agent-at-v-failure.txt
  fail 'host V could not open the sent review'
}
wait_pane_text 30 "$viewer_v_tmux" '2 comments in 2 files' || fail 'host V reader lost the comment counts'
wait_pane_text 30 "$viewer_v_tmux" 'Replace alpha deliberately' || fail 'host V reader lost the alpha comment'
wait_pane_text 30 "$viewer_v_tmux" 'Replace beta deliberately' || fail 'host V reader lost the beta comment'
capture_frame "$viewer_v_tmux" 04-host-v-review-reader.txt
tmux send-keys -t "$viewer_v_tmux" q

wait_pane_occurrences 300 "$viewer_u_tmux" "$reply_token" 2 || {
  capture_frame "$viewer_u_tmux" 05-model-reply-timeout.txt
  fail "Claude did not reply with $reply_token"
}
# Focus back from the assistant response to keep the sent attachment block on
# screen in the capture.
tmux send-keys -t "$viewer_u_tmux" C-a k
tmux send-keys -t "$viewer_u_tmux" C-a k
capture_frame "$viewer_u_tmux" 05-host-u-sent-review-and-reply.txt

say 'Suspending and resuming host A, then reopening from the still-running viewer V...'
timeout 30 "$amux_bin" --config "$a_config" server suspend > "$scratch/suspend.log"
sleep 2
capture_frame "$viewer_v_tmux" 06-host-v-during-restart.txt
AMUX_LOG=$scratch/a-resumed.log timeout 60 "$amux_bin" --config "$a_config" server resume > "$scratch/resume.log"
wait_list_text 90 "$a_config" "$agent_name" || fail 'host A did not resume the Claude agent'
wait_list_text 90 "$v_config" "$agent_name" || fail 'host V did not reconnect to host A'

# The old chat was subscribed to the pre-restart agent instance. Reopen it
# after inventory replay to exercise the documented viewer-resubscribe path.
tmux kill-session -t "$viewer_v_tmux"
open_chat "$viewer_v_tmux" "$v_config"

open_review_reader "$viewer_v_tmux" || {
  capture_frame "$viewer_v_tmux" 07-reopen-after-restart-failure.txt
  fail 'host V could not reopen the review after host A restarted'
}
wait_pane_text 30 "$viewer_v_tmux" 'Replace alpha deliberately' || fail 'restarted reader lost the alpha comment'
wait_pane_text 30 "$viewer_v_tmux" 'Replace beta deliberately' || fail 'restarted reader lost the beta comment'
capture_frame "$viewer_v_tmux" 07-host-v-review-after-restart.txt

debug_json=$scratch/daemon-debug.json
timeout 30 "$amux_bin" --config "$a_config" debug daemon --verbose --format json > "$debug_json"
session_id=$(python3 - "$debug_json" "$agent_name" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    dump = json.load(source)
for user in dump.get("users", []):
    for agent in user.get("agents", []):
        if agent.get("name") == sys.argv[2]:
            value = agent.get("session", {}).get("session_id")
            if value:
                print(value)
                raise SystemExit(0)
raise SystemExit("managed Claude session id missing from daemon debug output")
PY
)
claude_config=${CLAUDE_CONFIG_DIR:-"${HOME:?HOME is required}/.claude"}
transcript=$(find "$claude_config/projects" -type f -name "$session_id.jsonl" -print 2>/dev/null | head -n 1)
[ -n "$transcript" ] || fail "Claude transcript $session_id.jsonl was not found"
cp "$transcript" "$evidence_dir/rows.jsonl"

python3 - "$evidence_dir/rows.jsonl" <<'PY'
import json
import sys

strings = []

def collect(value):
    if isinstance(value, str):
        strings.append(value)
    elif isinstance(value, list):
        for item in value:
            collect(item)
    elif isinstance(value, dict):
        for item in value.values():
            collect(item)

with open(sys.argv[1], encoding="utf-8") as source:
    for number, line in enumerate(source, 1):
        try:
            collect(json.loads(line))
        except json.JSONDecodeError as error:
            raise SystemExit(f"invalid transcript JSON on line {number}: {error}")

received = "\n".join(strings)
required = [
    '<amux-attachment kind="review"',
    'diff="sha256:',
    'base="working-tree"',
    'head="',
    'blobs: [["alpha.txt","',
    '],["beta.txt","',
    '## alpha.txt @@ old:',
    '..new:',
    '&gt; -alpha old value',
    '&gt; +alpha new value',
    'Replace alpha deliberately; keep its caller contract.',
    '## beta.txt @@ old:',
    '&gt; -beta old value',
    '&gt; +beta new value',
    'Replace beta deliberately; preserve its result shape.',
]
missing = [value for value in required if value not in received]
if missing:
    raise SystemExit("model transcript lacks review facts: " + repr(missing))
PY

commit=$(git -C "$repo_root" rev-parse --short HEAD)
version=$($amux_bin --version)
cat > "$evidence_dir/README.md" <<EOF
# Cross-host review with restart

Captured on $(date -u +%Y-%m-%dT%H:%M:%SZ) from commit \`$commit\` with
\`$version\` and Claude model \`$model\`.

Hosts U and V were separate isolated amux daemons, each paired to agent host A
by LAN PIN. Host U opened the structured chat from the fleet, froze A's
two-file working-tree diff, and wrote one multi-line selection across the old
and new rows in each file. It sent the live review token to the real managed
Claude agent, which replied \`$reply_token\`.

Host V rendered the sent Review block from the remote stream and opened it in
the reader. Host A was then suspended and resumed, preserving the same agent;
the still-running V daemon reconnected, replayed the pinned refs, fetched the
diff, and opened both comments again.

## Frames

- \`frames/01-host-u-review-open.txt\` — remote working-tree review on U.
- \`frames/02-host-u-two-comments.txt\` — two inline threads over two files.
- \`frames/03-host-u-review-token.txt\` — live Review token back in the draft.
- \`frames/04-host-v-review-reader.txt\` — second remote viewer with both comments.
- \`frames/05-host-u-sent-review-and-reply.txt\` — sent block and model reply.
- \`frames/06-host-v-during-restart.txt\` — V while A is restarting.
- \`frames/07-host-v-review-after-restart.txt\` — the same review reopened after reconnect.
- \`rows.jsonl\` — the recipient-owned Claude transcript. The script parses it
  as JSON and requires both paths, old/new endpoint sides and lines, quoted old
  and new rows, the diff id, working-tree base, head, blob identity, and both
  comment texts.

## Replay

\`timeout 1800 e2e-tests/attachments_cross_host.sh .autopilot/evidence/live/review-cross-host\`

This is an opt-in live capture: it uses the operator's logged-in Claude Code
installation. The script isolates amux state under a private temporary root
and removes that root and its tmux sessions on exit.
EOF

say "PASS: cross-host review captured in $evidence_dir"
