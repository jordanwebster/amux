#!/bin/sh
# Capture the desktop free-relay state and its live transition to pro.
# Usage: timeout 300 e2e-tests/desktop_free_live.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=desktop-free
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
live_init "${1:-$repo_root/.autopilot/evidence/desktop}"

fixture_bin=$repo_root/target/debug/e2e-runner
[ -x "$fixture_bin" ] || live_fail "e2e-runner not found at $fixture_bin; run wt build"

ports=$(python3 - <<'PY'
import socket
sockets = [socket.socket(), socket.socket(), socket.socket()]
for item in sockets:
    item.bind(('127.0.0.1', 0))
print(*(item.getsockname()[1] for item in sockets))
PY
)
relay_port=${ports%% *}
remaining=${ports#* }
a_port=${remaining%% *}
b_port=${remaining#* }

fixture_state=$scratch/cloud-fixture
mkdir -p "$fixture_state"
timeout 300 "$fixture_bin" free-tier-fixture \
  --state-dir "$fixture_state" \
  --relay-port "$relay_port" \
  --amux-binary "$amux_bin" > "$scratch/fixture.log" 2>&1 &
fixture_pid=$!
live_wait_file 30 "$fixture_state/ready" ready
# The fixture writes only single-quoted paths and its loopback URL.
# shellcheck disable=SC1090
. "$fixture_state/fixture.env"
export AMUX_CLOUD_TLS_CA AMUX_TLS_CERT AMUX_TLS_KEY AMUX_TEST_DISCOVERY_MODE=disabled

cat > "$scratch/relay.yaml" <<EOF
host_name: relay
socket_path: '$scratch/relay.sock'
state_path: '$scratch/relay-state.yaml'
data_dir: '$scratch/relay-data'
tcp_port: $relay_port
cloud_url: '$CLOUD_URL'
prevent_idle_sleep: false
EOF

cloud_profile() {
  label=$1
  port=$2
  root=$scratch/$label-root
  profile_id=$(python3 - "$label" <<'PY'
import sys, uuid
print(uuid.uuid5(uuid.NAMESPACE_URL, f'amux-desktop-free:{sys.argv[1]}'))
PY
)
  profile_dir=$root/profiles/$profile_id
  mkdir -p "$profile_dir/state"
  cat > "$root/installation.yaml" <<EOF
host_name: '$label'
root: '$root'
front_door_socket: '$root/amux.sock'
keymaps_dir: '$root/keymaps'
prevent_idle_sleep: false
ui:
  color: ansi
EOF
  cat > "$root/registry.yaml" <<EOF
profiles:
  - id: '$profile_id'
    label:
      account_name: null
      email: null
      override_name: '$label'
    binding: null
    paused: false
    revision: 1
EOF
  cat > "$profile_dir/config.yaml" <<EOF
installation_config: '$root/installation.yaml'
socket_path: '$root/profiles/$profile_id.sock'
state_path: '$profile_dir/state/state.yaml'
data_dir: '$profile_dir/data'
cloud_url: '$CLOUD_URL'
cloud_refresh_secs: 1
lan:
  listen: true
  port: $port
EOF
  ln -s "$profile_dir/config.yaml" "$scratch/host-$label.yaml"
}

start_relay() {
  live_tmux new-session -d -s relay -x 120 -y 40 \
    env AMUX_LOG="$scratch/relay.log" AMUX_TLS_CERT="$AMUX_TLS_CERT" AMUX_TLS_KEY="$AMUX_TLS_KEY" \
    timeout 1500 "$amux_bin" --config "$scratch/relay.yaml" server start --cloud --foreground
  live_wait_file 30 "$scratch/relay.log" 'listening for cloud TLS carriers'
}

set_fixture_tier() {
  printf '%s\n' "$1" > "$fixture_state/tier"
  live_wait_file 10 "$fixture_state/tier-current" "$1"
  live_say "Fixture account: $1"
}

wait_profile_tier() {
  deadline=$(($(date +%s) + 60))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if timeout 10 "$amux_bin" --config "$1" profiles > "$scratch/profiles.txt" 2>/dev/null &&
      grep -Fq "bound / connected ($2)" "$scratch/profiles.txt"; then
      return 0
    fi
    sleep 1
  done
  live_fail "profile $1 did not become connected ($2)"
}

wait_peer_via() {
  deadline=$(($(date +%s) + 60))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if timeout 10 "$amux_bin" --config "$1" peer list > "$scratch/peers-current.txt" 2>/dev/null &&
      grep -Fq host-b "$scratch/peers-current.txt" && grep -Fq "$2" "$scratch/peers-current.txt"; then
      return 0
    fi
    sleep 1
  done
  live_fail "host-b did not report via $2"
}

cloud_profile host-a "$a_port"
cloud_profile host-b "$b_port"
a_config=$scratch/host-host-a.yaml
b_config=$scratch/host-host-b.yaml
start_relay
live_start "$a_config"
live_start "$b_config"
timeout 15 "$amux_bin" --config "$a_config" pair --cancel >/dev/null 2>&1 || true
timeout 15 "$amux_bin" --config "$b_config" pair --cancel >/dev/null 2>&1 || true

live_say 'Sign both daemons into the fixture account, which starts on the free tier.'
timeout 60 "$amux_bin" --config "$a_config" login > "$evidence_dir/login.txt"
timeout 60 "$amux_bin" --config "$b_config" login > "$scratch/login-b.txt"
wait_profile_tier "$a_config" free
wait_profile_tier "$b_config" free

project=$scratch/project
mkdir -p "$project"
git -C "$project" init -q
live_new away-agent "$b_config" "$project" test-agent --name away-agent
live_tmux send-keys -t away-agent -l -- 'hello from host-b'
live_tmux send-keys -t away-agent Enter
live_wait_pane 30 away-agent 'echo: hello from host-b'

live_say 'Temporarily grant pro only to establish the fixture devices mutual trust.'
set_fixture_tier pro
wait_profile_tier "$a_config" pro
wait_profile_tier "$b_config" pro
timeout 90 "$amux_bin" --config "$b_config" pair > "$scratch/pair-server.log" 2>&1 &
pair_pid=$!
live_wait_file 30 "$scratch/pair-server.log" 'Pairing PIN:'
pin=$(sed -n 's/^Pairing PIN: //p' "$scratch/pair-server.log" | head -n 1)
[ -n "$pin" ] || live_fail 'pair responder did not publish a PIN'
printf '%s\n' "$pin" | timeout 60 "$amux_bin" --config "$a_config" pair host-b > "$scratch/pair-client.log" 2>&1
wait "$pair_pid"
pair_pid=
live_wait_list "$a_config" away-agent
live_tmux new-session -d -s fleet -x 120 -y 40 \
  env AMUX_LOG="$scratch/client-fleet.log" timeout 1500 "$amux_bin" --config "$a_config" ui
live_wait_pane 60 fleet 'host-b ·relay'

live_say 'Restore free and restart only the disposable relay so both running daemons fetch free tokens.'
set_fixture_tier free
live_tmux kill-session -t relay
start_relay
wait_profile_tier "$a_config" free
wait_peer_via "$a_config" away
cp "$scratch/peers-current.txt" "$evidence_dir/peer-list.txt"
if timeout 10 "$amux_bin" --config "$a_config" list > "$scratch/list-free.txt" 2>&1; then
  live_fail 'free relay unexpectedly listed remote agents'
fi
grep -Fq 'Cloud subscription required' "$scratch/list-free.txt" ||
  live_fail 'free relay refusal did not carry the payment message'

live_wait_pane 60 fleet 'host-b is away'
live_wait_pane 30 fleet 'host-b ·away'
live_frame fleet fleet-away
live_tmux send-keys -t fleet h
live_wait_pane 30 fleet '  hosts'
live_frame fleet hosts-overlay
live_tmux send-keys -t fleet Escape

live_say 'Flip the account to pro; the running daemon refreshes and the remote agent becomes live.'
set_fixture_tier pro
wait_profile_tier "$a_config" pro
wait_peer_via "$a_config" relay
live_wait_list "$a_config" away-agent
live_wait_pane 60 fleet 'host-b ·relay'
live_frame fleet fleet-after-pro

live_say 'PASS: away, hosts overlay, payment refusal and live free-to-pro refresh were captured.'
