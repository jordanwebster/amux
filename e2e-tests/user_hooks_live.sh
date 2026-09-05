#!/bin/sh
# Live acceptance for the user hook boundary. This uses the operator's logged-in
# Claude installation, but isolates Claude settings and all amux state below a
# temporary directory.
#
# Usage: e2e-tests/user_hooks_live.sh [evidence-directory]
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
amux_bin=${AMUX_BIN:-"$repo_root/target/debug/amux"}
evidence_arg=${1:-"$repo_root/.autopilot/evidence/live/user-hooks"}
case "$evidence_arg" in
  /*) evidence_dir=$evidence_arg ;;
  *) evidence_dir=$repo_root/$evidence_arg ;;
esac
scratch=
config=

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  code=$?
  trap - EXIT HUP INT TERM
  if [ -n "$scratch" ]; then
    timeout 10 tmux -S "$scratch/tmux.sock" kill-server >/dev/null 2>&1 || true
  fi
  if [ -n "$config" ] && [ -f "$config" ] && [ -x "$amux_bin" ]; then
    timeout 15 "$amux_bin" --config "$config" server stop >/dev/null 2>&1 || true
  fi
  if [ -n "$scratch" ]; then
    case "$scratch" in
      /tmp/amux-user-hooks.*|/private/tmp/amux-user-hooks.*)
        rm -rf -- "$scratch"
        ;;
    esac
  fi
  exit "$code"
}
trap cleanup EXIT HUP INT TERM

[ -x "$amux_bin" ] || fail "amux binary not found at $amux_bin; run wt build"
command -v claude >/dev/null 2>&1 || fail "Claude Code is required"
command -v tmux >/dev/null 2>&1 || fail "tmux is required"
[ ! -e "$evidence_dir/direct-claude.txt" ] || fail "use a fresh evidence directory: $evidence_dir"

mkdir -p "$evidence_dir"
scratch=$(mktemp -d /tmp/amux-user-hooks.XXXXXX)
chmod 700 "$scratch"
claude_config=$scratch/claude-config
project=$scratch/project
direct_marker=$scratch/direct-marker.txt
amux_marker=$scratch/amux-marker.txt
hook_script=$scratch/stop-hook.sh
config=$scratch/amux.yaml
mkdir -p "$claude_config" "$project"

# Keep settings isolated while authentication uses the normal secure store.
# Neither comparison should inherit the enclosing Claude session's identity.
export CLAUDE_CONFIG_DIR=$claude_config
export CLAUDE_SECURESTORAGE_CONFIG_DIR=''
export DISABLE_AUTOUPDATER=1 DISABLE_UPDATES=1 DISABLE_INSTALLATION_CHECKS=1
unset CLAUDECODE CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_SESSION_ID CLAUDE_PID \
  CLAUDE_EFFORT AI_AGENT TRACEPARENT CLAUDE_CODE_ENTRYPOINT CLAUDE_CODE_EXECPATH \
  CLAUDE_CODE_BRIDGE_SESSION_ID CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN

cat > "$hook_script" <<'EOF'
#!/bin/sh
cat >/dev/null
printf 'Stop hook fired\n' > "${HOOK_MARKER:?HOOK_MARKER is required}"
EOF
chmod 700 "$hook_script"

cat > "$claude_config/settings.json" <<EOF
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": "$hook_script"
      }]
    }]
  }
}
EOF

(
  cd "$project"
  HOOK_MARKER=$direct_marker \
    timeout 180 claude -p --model haiku \
    "Reply with exactly DIRECT_HOOK_CHECK and nothing else." \
    > "$evidence_dir/direct-claude.txt" 2> "$evidence_dir/direct-stderr.txt"
)
[ -f "$direct_marker" ] || fail "the direct Claude Stop hook did not write its marker"
cp "$direct_marker" "$evidence_dir/direct-marker.txt"

cat > "$config" <<EOF
host_name: user-hooks
socket_path: '$scratch/amux.sock'
state_path: '$scratch/state.yaml'
data_dir: '$scratch/data'
enable_cloud_mode: false
prevent_idle_sleep: false
claude:
  driver: sdk
ui:
  default_open_mode: chat
EOF

timeout 30 "$amux_bin" --config "$config" init >/dev/null
HOOK_MARKER=$amux_marker AMUX_LOG=$evidence_dir/daemon.log \
  timeout 30 "$amux_bin" --config "$config" server start >/dev/null

# Submit through the chat only once its composer is ready for input.
timeout 10 tmux -f /dev/null -S "$scratch/tmux.sock" new-session -d \
  -s hook -x 120 -y 40 -c "$project" \
  env AMUX_LOG="$evidence_dir/tui.log" \
  timeout 420 "$amux_bin" --config "$config" new claude --name hook-check -- --model haiku
timeout 10 tmux -S "$scratch/tmux.sock" set-option -g status off
elapsed=0
while [ "$elapsed" -lt 60 ]; do
  timeout 10 tmux -S "$scratch/tmux.sock" capture-pane -p -t hook > "$evidence_dir/amux-ready.txt"
  if grep -Fq 'enter send · ctrl+j newline' "$evidence_dir/amux-ready.txt"; then
    break
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done
grep -Fq 'enter send · ctrl+j newline' "$evidence_dir/amux-ready.txt" || fail "the SDK chat did not become ready"
timeout 15 "$amux_bin" --config "$config" ls --all > "$evidence_dir/amux-inventory.txt"
grep -Fq 'hook-check [claude/sdk]' "$evidence_dir/amux-inventory.txt" || fail "the hook agent did not use the SDK driver"
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t hook -l \
  'Reply with exactly AMUX_HOOK_CHECK and nothing else.'
timeout 10 tmux -S "$scratch/tmux.sock" send-keys -t hook Enter

elapsed=0
while [ "$elapsed" -lt 300 ]; do
  if [ -f "$amux_marker" ]; then
    break
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done
[ -f "$amux_marker" ] || fail "the amux SDK agent's Stop hook did not write its marker"

cp "$amux_marker" "$evidence_dir/amux-marker.txt"
timeout 10 tmux -S "$scratch/tmux.sock" capture-pane -p -t hook > "$evidence_dir/amux-reply.txt"
{
  printf 'Claude: %s\n' "$(timeout 15 claude --version)"
  printf 'Direct Claude marker:\n'
  cat "$direct_marker"
  printf 'amux SDK marker:\n'
  cat "$amux_marker"
} | tee "$evidence_dir/transcript.txt"
