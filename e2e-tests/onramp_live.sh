#!/bin/sh
# Capture the account-free LAN on-ramp between two isolated desktop daemons.
# Usage: timeout 300 e2e-tests/onramp_live.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=onramp
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
live_init "${1:-$repo_root/.autopilot/evidence/onramp}"

onramp_config() {
  label=$1
  root=$scratch/$label-root
  profile_id=$(python3 - "$label" <<'PY'
import sys, uuid
print(uuid.uuid5(uuid.NAMESPACE_URL, f'amux-onramp-live:{sys.argv[1]}'))
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
EOF
  ln -s "$profile_dir/config.yaml" "$scratch/host-$label.yaml"
}

onramp_config lan-host
onramp_config lan-phone
host_config=$scratch/host-lan-host.yaml
phone_config=$scratch/host-lan-phone.yaml
live_start "$host_config"
live_start "$phone_config"

live_say 'Host: open the fresh-install pairing window and show the install QR and code.'
live_tmux new-session -d -s init -x 120 -y 40 \
  timeout 60 "$amux_bin" --config "$host_config" init
live_wait_pane 30 init 'Pairing code:'
live_frame init init
live_tmux capture-pane -p -t init > "$scratch/init-pane.txt"
pin=$(sed -n 's/.*Pairing code: \([0-9][0-9][0-9] [0-9][0-9][0-9]\).*/\1/p' "$scratch/init-pane.txt" | head -n 1)
[ -n "$pin" ] || live_fail 'init did not publish a six-digit pairing code'

project=$scratch/project
mkdir -p "$project"
git -C "$project" init -q
live_new onramp-agent "$host_config" "$project" test-agent --name live-agent
live_tmux send-keys -t onramp-agent -l -- 'hello from lan-host'
live_tmux send-keys -t onramp-agent Enter
live_wait_pane 30 onramp-agent 'echo: hello from lan-host'

live_say 'Phone: discover lan-host by name over multicast.'
found_deadline=$(($(date +%s) + 90))
while [ "$(date +%s)" -lt "$found_deadline" ]; do
  if timeout 10 "$amux_bin" --config "$phone_config" peer list > "$scratch/peers.txt" 2>/dev/null &&
    grep -Fq 'lan-host' "$scratch/peers.txt" &&
    grep -Fq 'found · on this network' "$scratch/peers.txt"; then
    break
  fi
  sleep 1
done
grep -Fq 'lan-host' "$scratch/peers.txt" || live_fail 'lan-phone did not discover lan-host'
live_tmux new-session -d -s peer-list-found -x 120 -y 40 \
  timeout 30 "$amux_bin" --config "$phone_config" peer list
live_wait_pane 30 peer-list-found 'found · on this network'
live_frame peer-list-found peer-list-found

live_say 'Phone: pair with the found host using the code printed by init.'
live_tmux new-session -d -s paired -x 120 -y 40 \
  timeout 60 "$amux_bin" --config "$phone_config" pair lan-host
sleep 1
live_tmux send-keys -t paired -l -- "$pin"
live_tmux send-keys -t paired Enter
live_wait_pane 60 paired 'Paired with lan-host'
live_frame paired paired

live_say 'Phone: attach directly to the test agent running on lan-host.'
live_tmux new-session -d -s attached -x 120 -y 40 \
  timeout 120 "$amux_bin" --config "$phone_config" attach live-agent
live_wait_pane 60 attached 'echo: hello from lan-host'
live_tmux send-keys -t attached -l -- 'hello from lan-phone'
live_tmux send-keys -t attached Enter
live_wait_pane 30 attached 'echo: hello from lan-phone'
live_frame attached attached

live_say 'PASS: init, multicast discovery, code pairing, and direct attach completed without an account.'
