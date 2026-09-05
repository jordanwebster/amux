#!/bin/sh
# Shared setup for opt-in live captures. Source after setting live_name and
# repo_root. Only this run's daemons and private tmux server are stopped.

live_init() {
  amux_bin=${AMUX_BIN:-"$repo_root/target/debug/amux"}
  case "$amux_bin" in
    /*) ;;
    *) amux_bin=$(command -v "$amux_bin") ;;
  esac
  for dependency in timeout tmux python3; do
    command -v "$dependency" >/dev/null 2>&1 || live_fail "$dependency is required"
  done
  [ -x "$amux_bin" ] || live_fail "amux not found at $amux_bin; run wt build"
  evidence_dir=${1:-"$repo_root/.autopilot/evidence/live/$live_name"}
  mkdir -p "$evidence_dir"
  evidence_dir=$(CDPATH='' cd -- "$evidence_dir" && pwd -P)
  frames_dir=$evidence_dir/frames
  mkdir -p "$frames_dir"
  : > "$evidence_dir/transcript.txt"
  scratch=$(mktemp -d /tmp/amux-live.XXXXXX)
  chmod 700 "$scratch"
  pair_pid=
  trap live_cleanup EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  # Ignore personal tmux settings; retain exited panes for failure diagnosis.
  timeout 10 tmux -f /dev/null -S "$scratch/tmux.sock" new-session -d \
    -s keeper -x 120 -y 40 'timeout 1800 sleep 1800'
  live_tmux set-option -g status off
  live_tmux set-option -g remain-on-exit on
  live_say "Capture: $live_name; $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  live_say "Build: $(timeout 10 "$amux_bin" --version)"
  live_say "Tree: $(git -C "$repo_root" -c core.fsmonitor=false rev-parse HEAD)"
}

live_say() { printf '%s\n' "$*" | tee -a "$evidence_dir/transcript.txt"; }
live_fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
live_tmux() { timeout 10 tmux -S "$scratch/tmux.sock" "$@"; }

live_cleanup() {
  live_exit=$?
  trap - EXIT HUP INT TERM
  [ -z "$pair_pid" ] || kill "$pair_pid" 2>/dev/null || true
  if [ "$live_exit" -ne 0 ]; then
    live_say "FAIL: capture exited $live_exit; incomplete evidence"
    for live_pane in $(live_tmux list-panes -a -F '#{pane_id}' 2>/dev/null); do
      live_tmux capture-pane -p -t "$live_pane" > "$frames_dir/failure-${live_pane#%}.txt" 2>/dev/null || true
    done
  fi
  live_tmux kill-server >/dev/null 2>&1 || true
  for live_config in "$scratch"/host-*.yaml; do
    [ -f "$live_config" ] || continue
    timeout 15 "$amux_bin" --config "$live_config" server stop >/dev/null 2>&1 || true
  done
  for live_log in "$scratch"/*.log; do
    [ ! -f "$live_log" ] || cp "$live_log" "$evidence_dir/"
  done
  rm -rf -- "$scratch"
  exit "$live_exit"
}

live_config() {
  # live_config <host label> [tcp port]; no Claude or open-mode overrides.
  cat > "$scratch/host-$1.yaml" <<EOF
host_name: '$1'
socket_path: '$scratch/$1.sock'
state_path: '$scratch/$1-state.yaml'
data_dir: '$scratch/$1-data'
enable_cloud_mode: false
prevent_idle_sleep: false
ui:
  color: ansi
EOF
  if [ -n "${2:-}" ]; then
    printf 'tcp_port: %s\n' "$2" >> "$scratch/host-$1.yaml"
  fi
}

live_start() {
  timeout 30 "$amux_bin" --config "$1" init > "$scratch/init-$(basename "$1").log" 2>&1
  AMUX_LOG="$scratch/daemon-$(basename "$1").log" \
    timeout 30 "$amux_bin" --config "$1" server start
}

live_wait_file() {
  live_deadline=$(($(date +%s) + $1))
  while [ "$(date +%s)" -lt "$live_deadline" ]; do
    if [ -f "$2" ] && grep -Fq -- "$3" "$2"; then return 0; fi
    sleep 1
  done
  live_fail "waiting for '$3' in $2"
}

live_wait_list() {
  live_deadline=$(($(date +%s) + 90))
  while [ "$(date +%s)" -lt "$live_deadline" ]; do
    if timeout 10 "$amux_bin" --config "$1" ls --all > "$scratch/inventory.txt" 2>/dev/null &&
      grep -Fq -- "$2" "$scratch/inventory.txt"; then return 0; fi
    sleep 1
  done
  live_fail "agent '$2' did not appear in $1"
}

live_wait_pane() {
  live_deadline=$(($(date +%s) + $1))
  while [ "$(date +%s)" -lt "$live_deadline" ]; do
    live_tmux capture-pane -p -t "$2" > "$scratch/pane.txt" || live_fail "pane $2 ended"
    if grep -Fq -- "$3" "$scratch/pane.txt"; then return 0; fi
    sleep 1
  done
  live_fail "pane $2 did not show '$3'"
}

live_frame() {
  [ "$(live_tmux display-message -p -t "$1" '#{pane_width}x#{pane_height}')" = 120x40 ] ||
    live_fail "pane $1 is not 120x40"
  live_tmux capture-pane -p -t "$1" > "$frames_dir/$2.txt"
  live_tmux capture-pane -p -e -t "$1" > "$frames_dir/$2.ansi"
  live_say "Frame: frames/$2.txt"
}

live_new() {
  # Remaining arguments are the real amux new arguments (including overrides).
  live_session=$1; live_configuration=$2; live_project=$3; shift 3
  live_tmux new-session -d -s "$live_session" -x 120 -y 40 -c "$live_project" \
    env AMUX_LOG="$scratch/client-$live_session.log" \
    timeout 1500 "$amux_bin" --config "$live_configuration" new "$@"
}

live_fleet_select() {
  live_tmux new-session -d -s "$1" -x 120 -y 40 \
    env AMUX_LOG="$scratch/client-$1.log" timeout 1500 "$amux_bin" --config "$2" ui
  live_wait_pane 60 "$1" "$3"
  live_tmux send-keys -t "$1" /
  live_tmux send-keys -t "$1" -l -- "$3"
  # Leaving filter mode retains the filter and enables the plain 'o' binding.
  live_tmux send-keys -t "$1" Escape
  live_wait_pane 30 "$1" "$3"
}

live_assert_inventory() {
  python3 - "$@" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text()
for expectation in sys.argv[2:]:
    name, kind = expectation.split('=', 1)
    rows = [line for line in text.splitlines() if line.strip().split(' ', 1)[0] == name]
    if len(rows) != 1 or f'[{kind}]' not in rows[0]:
        raise SystemExit(f'{name}: expected one [{kind}] inventory row, got {rows}')
PY
}
