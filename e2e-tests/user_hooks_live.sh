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
  CLAUDE_CONFIG_DIR=$claude_config HOOK_MARKER=$direct_marker \
    timeout 180 claude -p --model haiku \
    "Reply with exactly DIRECT_HOOK_CHECK and nothing else." \
    > "$evidence_dir/direct-claude.txt"
)
[ -f "$direct_marker" ] || fail "the direct Claude Stop hook did not write its marker"

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
CLAUDE_CONFIG_DIR=$claude_config HOOK_MARKER=$amux_marker \
  timeout 30 "$amux_bin" --config "$config" server start >/dev/null

spawn_request='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"spawn","arguments":{"kind":"claude","name":"user-hooks-sdk","cwd":"'
spawn_request=$spawn_request$project'","prompt":"Reply with exactly AMUX_HOOK_CHECK and nothing else."}}}'
printf '%s\n' "$spawn_request" | timeout 30 "$amux_bin" --config "$config" \
  mcp agent --socket-path "$scratch/amux.sock" > "$evidence_dir/amux-spawn.json"

elapsed=0
while [ "$elapsed" -lt 300 ]; do
  if [ -f "$amux_marker" ]; then
    break
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done
[ -f "$amux_marker" ] || fail "the amux SDK agent's Stop hook did not write its marker"

cp "$direct_marker" "$evidence_dir/direct-marker.txt"
cp "$amux_marker" "$evidence_dir/amux-marker.txt"
{
  printf 'Direct Claude marker:\n'
  cat "$direct_marker"
  printf 'amux SDK marker:\n'
  cat "$amux_marker"
} | tee "$evidence_dir/transcript.txt"
