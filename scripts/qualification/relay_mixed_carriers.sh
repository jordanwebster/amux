#!/bin/sh
# Smoke the real debug relay binary with one QUIC client and one TCP fallback.
# Usage: timeout 900 scripts/qualification/relay_mixed_carriers.sh SCRATCH_PARENT
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
amux_bin=$repo_root/target/debug/amux
fixture_bin=$repo_root/target/debug/account-fixture
test_agent_bin=$repo_root/target/debug/test-agent
scratch_parent=${1:?scratch parent is required}
evidence_dir=$repo_root/.autopilot/evidence/relay
evidence=$evidence_dir/local-smoke.txt

for dependency in timeout tmux python3; do
  command -v "$dependency" >/dev/null 2>&1 || {
    printf 'missing dependency: %s\n' "$dependency" >&2
    exit 1
  }
done
[ -x "$amux_bin" ] || { printf 'missing %s; run wt build\n' "$amux_bin" >&2; exit 1; }
[ -x "$fixture_bin" ] || { printf 'missing %s; run wt build\n' "$fixture_bin" >&2; exit 1; }
[ -x "$test_agent_bin" ] || { printf 'missing %s; run wt build\n' "$test_agent_bin" >&2; exit 1; }
[ -d "$scratch_parent" ] || { printf 'scratch parent is not a directory: %s\n' "$scratch_parent" >&2; exit 1; }

# Keep Unix-domain socket paths below the platform's SUN_LEN limit even when
# the caller's temporary directory lives under a long macOS per-user path.
if [ "${#scratch_parent}" -lt 32 ]; then
  scratch=$(mktemp -d "$scratch_parent/amux-rs.XXXXXX")
else
  scratch=$(mktemp -d /tmp/amux-rs.XXXXXX)
fi
chmod 700 "$scratch"
mkdir -p "$evidence_dir"
relay_pid=
fixture_pid=
pair_pid=

cleanup() {
  result=$?
  trap - EXIT HUP INT TERM
  [ -z "$pair_pid" ] || kill "$pair_pid" 2>/dev/null || true
  for config in "$scratch"/host-*.yaml; do
    [ -f "$config" ] || continue
    timeout 15 "$amux_bin" --config "$config" server stop >/dev/null 2>&1 || true
  done
  if [ -n "$relay_pid" ]; then
    kill "$relay_pid" 2>/dev/null || true
    wait "$relay_pid" 2>/dev/null || true
  fi
  [ -z "$fixture_pid" ] || {
    : > "$scratch/cloud-fixture/stop"
    wait "$fixture_pid" 2>/dev/null || true
  }
  timeout 10 tmux -S "$scratch/tmux.sock" kill-server >/dev/null 2>&1 || true
  if [ "$result" -ne 0 ]; then
    {
      printf 'FAIL: local relay smoke exited %s\n' "$result"
      for log in "$scratch"/*.log; do
        [ ! -f "$log" ] || { printf '\n--- %s ---\n' "$(basename "$log")"; tail -n 100 "$log"; }
      done
    } > "$evidence"
  fi
  rm -rf -- "$scratch"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

ports=$(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(3)]
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
timeout 600 "$fixture_bin" \
  --state-dir "$fixture_state" \
  --relay-port "$relay_port" > "$scratch/identity.log" 2>&1 &
fixture_pid=$!

wait_file() {
  seconds=$1
  file=$2
  text=$3
  deadline=$(($(date +%s) + seconds))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    [ -f "$file" ] && grep -Fq -- "$text" "$file" && return 0
    sleep 1
  done
  printf "timed out waiting for '%s' in %s\n" "$text" "$file" >&2
  return 1
}

wait_file 30 "$fixture_state/ready" ready
# The fixture owns these generated paths and emits only single-quoted values.
# shellcheck disable=SC1090
. "$fixture_state/fixture.env"
export AMUX_CLOUD_TLS_CA AMUX_TLS_CERT AMUX_TLS_KEY AMUX_TEST_DISCOVERY_MODE=disabled
printf 'pro\n' > "$fixture_state/tier"
wait_file 10 "$fixture_state/tier-current" pro

cat > "$scratch/relay.yaml" <<EOF
host_name: relay
socket_path: '$scratch/relay.sock'
state_path: '$scratch/relay-state.yaml'
data_dir: '$scratch/relay-data'
tcp_port: $relay_port
udp_port: $relay_port
cloud_url: '$CLOUD_URL'
prevent_idle_sleep: false
EOF

profile_config() {
  label=$1
  port=$2
  root=$scratch/$label-root
  profile_id=$(python3 - "$label" <<'PY'
import sys, uuid
print(uuid.uuid5(uuid.NAMESPACE_URL, f'amux-relay-smoke:{sys.argv[1]}'))
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
EOF
  cat > "$root/registry.yaml" <<EOF
profiles:
  - id: '$profile_id'
    label:
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
lan:
  listen: false
  port: $port
EOF
  ln -s "$profile_dir/config.yaml" "$scratch/host-$label.yaml"
}

profile_config host-a "$a_port"
profile_config host-b "$b_port"
a_config=$scratch/host-host-a.yaml
b_config=$scratch/host-host-b.yaml

AMUX_LOG="$scratch/relay.log" timeout 600 "$amux_bin" --config "$scratch/relay.yaml" \
  server start --cloud --foreground > "$scratch/relay-stdio.log" 2>&1 &
relay_pid=$!
wait_file 30 "$scratch/relay.log" 'listening for cloud TLS carriers'
wait_file 30 "$scratch/relay.log" 'listening for cloud QUIC carriers'

AMUX_TEST_RELAY_UDP_BLOCKED=1 AMUX_LOG="$scratch/daemon-a.log" \
  timeout 30 "$amux_bin" --config "$a_config" init > "$scratch/init-a.log" 2>&1
AMUX_LOG="$scratch/daemon-b.log" \
  timeout 30 "$amux_bin" --config "$b_config" init > "$scratch/init-b.log" 2>&1
timeout 15 "$amux_bin" --config "$a_config" pair --cancel >/dev/null
timeout 15 "$amux_bin" --config "$b_config" pair --cancel >/dev/null
timeout 60 "$amux_bin" --config "$a_config" login > "$scratch/login-a.log" 2>&1
timeout 60 "$amux_bin" --config "$b_config" login > "$scratch/login-b.log" 2>&1

wait_profile() {
  config=$1
  carrier=$2
  output=$3
  deadline=$(($(date +%s) + 60))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if timeout 10 "$amux_bin" --config "$config" profiles > "$output" 2>&1 &&
      grep -Fq "bound / connected (pro, $carrier)" "$output"; then
      return 0
    fi
    sleep 1
  done
  printf 'profile did not connect over %s\n' "$carrier" >&2
  return 1
}

wait_profile "$a_config" tcp "$scratch/profiles-a.txt"
wait_profile "$b_config" quic "$scratch/profiles-b.txt"

timeout 90 "$amux_bin" --config "$a_config" pair > "$scratch/pair-a.log" 2>&1 &
pair_pid=$!
wait_file 30 "$scratch/pair-a.log" 'Pairing PIN:'
pin=$(sed -n 's/^Pairing PIN: //p' "$scratch/pair-a.log" | head -n 1)
[ -n "$pin" ] || { printf 'pair responder did not publish a PIN\n' >&2; exit 1; }
printf '%s\n' "$pin" | timeout 60 "$amux_bin" --config "$b_config" pair host-a \
  > "$scratch/pair-b.log" 2>&1
wait "$pair_pid"
pair_pid=

project_a=$scratch/project-a
mkdir -p "$project_a"
git -C "$project_a" init -q
timeout 10 tmux -f /dev/null -S "$scratch/tmux.sock" new-session -d -s keeper \
  'timeout 600 sleep 600'
timeout 10 tmux -S "$scratch/tmux.sock" set-option -g status off
timeout 10 tmux -S "$scratch/tmux.sock" set-option -g remain-on-exit on

start_agent() {
  name=$1
  config=$2
  project=$3
  timeout 10 tmux -S "$scratch/tmux.sock" new-session -d -s "$name" -x 120 -y 40 \
    -c "$project" "timeout 300 '$amux_bin' --config '$config' new '$test_agent_bin' --name '$name'"
}

wait_pane() {
  session=$1
  text=$2
  deadline=$(($(date +%s) + 60))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    timeout 10 tmux -S "$scratch/tmux.sock" capture-pane -p -t "$session" > "$scratch/pane.txt"
    grep -Fq -- "$text" "$scratch/pane.txt" && return 0
    sleep 1
  done
  printf "pane %s did not show '%s'\n" "$session" "$text" >&2
  return 1
}

wait_agent() {
  config=$1
  name=$2
  deadline=$(($(date +%s) + 60))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if timeout 10 "$amux_bin" --config "$config" list --all > "$scratch/inventory.txt" 2>&1 &&
      grep -Fq -- "$name" "$scratch/inventory.txt"; then
      return 0
    fi
    sleep 1
  done
  printf "agent %s did not reach %s\n" "$name" "$config" >&2
  return 1
}

start_agent from-a "$a_config" "$project_a"
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t from-a -l -- 'hello from tcp host'
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t from-a Enter
wait_pane from-a 'echo: hello from tcp host'
wait_agent "$b_config" from-a
timeout 10 tmux -S "$scratch/tmux.sock" new-session -d -s attach-from-b -x 120 -y 40 \
  "timeout 120 '$amux_bin' --config '$b_config' attach from-a"
wait_pane attach-from-b 'echo: hello from tcp host'
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t attach-from-b -l -- 'reply from quic host'
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t attach-from-b Enter
wait_pane from-a 'echo: reply from quic host'

{
  printf 'Local relay smoke: PASS\n'
  printf 'Binary: %s\n' "$(timeout 10 "$amux_bin" --version)"
  printf 'Relay listeners: TCP and QUIC on 127.0.0.1:%s\n' "$relay_port"
  printf '\n--- UDP-blocked host profile ---\n'
  cat "$scratch/profiles-a.txt"
  printf '\n--- QUIC host profile ---\n'
  cat "$scratch/profiles-b.txt"
  printf '\n--- Pairing initiator ---\n'
  cat "$scratch/pair-b.log"
  printf '\n--- Bidirectional session at QUIC host ---\n'
  timeout 10 tmux -S "$scratch/tmux.sock" capture-pane -p -t attach-from-b
  printf '\n--- Bidirectional session at TCP host ---\n'
  timeout 10 tmux -S "$scratch/tmux.sock" capture-pane -p -t from-a
} > "$evidence.tmp"
mv "$evidence.tmp" "$evidence"
printf 'relay local smoke: PASS (%s)\n' "$evidence"
