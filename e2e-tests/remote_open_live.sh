#!/bin/sh
# Two isolated identities paired over loopback LAN transport, using the
# operator's Claude and Codex logins. Each Codex thread gets one small seed turn
# so its own terminal has a persisted conversation to resume.
# Usage: timeout 1500 e2e-tests/remote_open_live.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=remote-open
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
live_init "${1:-$repo_root/.autopilot/evidence/live/remote-open}"
for provider in claude codex; do
  command -v "$provider" >/dev/null 2>&1 || live_fail "$provider is required"
done

# Reserve both ports together so the allocator cannot return the same port
# twice. Daemon start still checks the unavoidable bind race with other apps.
ports=$(python3 - <<'PY'
import socket
sockets = [socket.socket(), socket.socket()]
for s in sockets:
    s.bind(('127.0.0.1', 0))
print(*(s.getsockname()[1] for s in sockets))
PY
)
a_port=${ports% *}
b_port=${ports#* }
live_config agent-host "$a_port"
live_config viewing-host "$b_port"
a_config=$scratch/host-agent-host.yaml
b_config=$scratch/host-viewing-host.yaml
live_start "$a_config"
live_start "$b_config"

live_say 'Pair viewing-host to agent-host using a one-time LAN PIN over loopback.'
timeout 90 "$amux_bin" --config "$a_config" pair --listen > "$scratch/pair-server.log" 2>&1 &
pair_pid=$!
live_wait_file 30 "$scratch/pair-server.log" 'Pairing PIN:'
pin=$(sed -n 's/^Pairing PIN: //p' "$scratch/pair-server.log" | head -n 1)
[ -n "$pin" ] || live_fail 'pair responder did not publish a PIN'
printf '%s\n' "$pin" | timeout 60 "$amux_bin" --config "$b_config" pair --connect "127.0.0.1:$a_port" > "$scratch/pair-client.log" 2>&1
wait "$pair_pid"
pair_pid=
timeout 30 "$amux_bin" --config "$b_config" peer list > "$evidence_dir/peers.txt"

project=$scratch/project
mkdir -p "$project"
git -C "$project" init -q
live_new remote-claude "$a_config" "$project" claude --name remote-claude --driver pty
live_new local-claude "$b_config" "$project" claude --name local-claude --driver pty

seed_codex() {
  # This creation-only config opens chat; every fleet check below uses the
  # original config with the shipped default. Raw resume of a never-used
  # Codex thread fails because the provider has not yet persisted a rollout.
  seed_name=$1
  seed_config=$scratch/create-$seed_name.yaml
  sed '/  color: ansi/a\
  default_open_mode: chat
' "$2" > "$seed_config"
  live_new "$seed_name" "$seed_config" "$project" codex --name "$seed_name"
  live_wait_pane 90 "$seed_name" 'enter send · ctrl+j newline'
  live_tmux send-keys -t "$seed_name" -l -- 'Reply with exactly READY_FOR_OPEN and nothing else.'
  live_tmux send-keys -t "$seed_name" Enter
  seed_deadline=$(($(date +%s) + 180))
  while [ "$(date +%s)" -lt "$seed_deadline" ]; do
    live_tmux capture-pane -p -t "$seed_name" > "$scratch/seed.txt"
    # The token occurs once in the prompt; the second occurrence is the reply.
    if [ "$(grep -Fo READY_FOR_OPEN "$scratch/seed.txt" | wc -l | tr -d ' ')" -ge 2 ]; then
      live_wait_pane 30 "$seed_name" '· idle'
      live_frame "$seed_name" "seed-$seed_name"
      return 0
    fi
    sleep 1
  done
  live_fail "$seed_name did not finish its seed conversation"
}
seed_codex remote-codex "$a_config"
seed_codex local-codex "$b_config"
for name in remote-claude remote-codex local-claude local-codex; do
  live_wait_list "$b_config" "$name"
done
timeout 30 "$amux_bin" --config "$b_config" ls > "$evidence_dir/viewing-host-inventory.txt"
live_assert_inventory "$evidence_dir/viewing-host-inventory.txt" \
  remote-claude=claude/pty remote-codex=codex local-claude=claude/pty local-codex=codex

check_open() {
  name=$1; key=$2; expected=$3; frame=$4
  live_say "Viewing host: select $name, press $key, expect $expected."
  live_fleet_select viewer "$b_config" "$name"
  if [ "$name" = remote-claude ] || [ "$name" = remote-codex ]; then
    live_wait_pane 30 viewer 'enter chat  o raw attach'
  else
    live_wait_pane 30 viewer 'enter raw attach  o chat'
  fi
  live_frame viewer "$frame-fleet"
  live_tmux send-keys -t viewer "$key"
  if [ "$expected" = chat ]; then
    live_wait_pane 60 viewer 'enter send · ctrl+j newline'
    live_wait_pane 30 viewer "$name"
  else
    # Require provider-owned chrome, rather than merely absence of our chat.
    # Trust/onboarding screens are valid terminal content: do not answer them
    # or change the operator's provider preferences for an entry-policy test.
    case "$name" in
      *claude) live_wait_pane 90 viewer 'Claude' ;;
      *codex)
        live_wait_pane 90 viewer 'Codex'
        live_wait_pane 90 viewer READY_FOR_OPEN
        ;;
    esac
    live_tmux capture-pane -p -t viewer > "$scratch/raw.txt"
    if grep -Fq 'enter send · ctrl+j newline' "$scratch/raw.txt"; then
      live_fail "$name opened chat when raw attach was expected"
    fi
  fi
  live_frame viewer "$frame"
  live_tmux send-keys -t viewer C-a s
  live_wait_pane 30 viewer 'raw attach'
  live_tmux kill-session -t viewer
}

check_open remote-claude Enter chat remote-claude-enter
check_open remote-claude o raw remote-claude-other
check_open remote-codex Enter chat remote-codex-enter
check_open remote-codex o raw remote-codex-other
check_open local-claude Enter raw local-claude-enter
check_open local-claude o chat local-claude-other
check_open local-codex Enter raw local-codex-enter
check_open local-codex o chat local-codex-other
live_say 'PASS: both remote kinds default to chat; o opens raw; both local kinds retain the shipped raw default.'
