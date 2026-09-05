#!/bin/sh
# Live inventory and fleet proof using the operator's Claude login. No prompts are sent.
# Usage: timeout 900 e2e-tests/claude_driver_config.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=config
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
out=${1:-$repo_root/.autopilot/evidence/live/config}
[ ! -e "$out/transcript.txt" ] || { printf 'Use a fresh evidence directory: %s\n' "$out" >&2; exit 1; }
live_init "$out"
command -v claude >/dev/null 2>&1 || live_fail 'Claude Code is required'

live_config config
config=$scratch/host-config.yaml
project=$scratch/project
mkdir -p "$project"
live_start "$config"
live_say 'No Claude configuration: create default-agent using amux new claude.'
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-before.txt"
grep -Fq 'No agents running.' "$evidence_dir/ls-before.txt" || live_fail 'isolated inventory is not empty'
live_new default "$config" "$project" claude --name default-agent
live_wait_list "$config" default-agent
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-no-config.txt"
live_assert_inventory "$evidence_dir/ls-no-config.txt" default-agent=claude/pty
cat "$evidence_dir/ls-no-config.txt" | tee -a "$evidence_dir/transcript.txt"

# Each CLI invocation loads config afresh. Keep the daemon and first agent
# alive so these captures prove the earlier agent was not converted.
printf 'claude:\n  driver: sdk\n' >> "$config"
live_say 'Set claude.driver to sdk on the same daemon; create configured-agent.'
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-sdk-before.txt"
live_assert_inventory "$evidence_dir/ls-sdk-before.txt" default-agent=claude/pty
live_new configured "$config" "$project" claude --name configured-agent
live_wait_list "$config" configured-agent
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-sdk-config.txt"
live_assert_inventory "$evidence_dir/ls-sdk-config.txt" default-agent=claude/pty configured-agent=claude/sdk
cat "$evidence_dir/ls-sdk-config.txt" | tee -a "$evidence_dir/transcript.txt"

live_say 'Open the unfiltered fleet and select configured-agent.'
live_tmux new-session -d -s fleet -x 120 -y 40 \
  env AMUX_LOG="$scratch/client-fleet.log" timeout 1500 "$amux_bin" --config "$config" ui
live_wait_pane 60 fleet default-agent
live_wait_pane 60 fleet configured-agent
live_tmux send-keys -t fleet g g
# There are exactly two rows. Read the selection instead of assuming that
# launch timing leaves the two agents in a particular attention order.
live_wait_pane 30 fleet '▎'
if ! grep -Eq '▎.*configured-agent' "$scratch/pane.txt"; then
  live_tmux send-keys -t fleet Down
fi
live_deadline=$(($(date +%s) + 30))
while :; do
  live_tmux capture-pane -p -t fleet > "$scratch/pane.txt"
  grep -Eq '▎.*configured-agent' "$scratch/pane.txt" && break
  [ "$(date +%s)" -lt "$live_deadline" ] || live_fail 'configured-agent was not selected'
  sleep 1
done
live_frame fleet fleet-mixed
live_tmux send-keys -t fleet '?'
live_wait_pane 30 fleet 'open in chat'
live_frame fleet fleet-sdk-help

python3 - "$frames_dir" <<'PY'
import pathlib, re, sys

frames = pathlib.Path(sys.argv[1])
for name in ('fleet-mixed', 'fleet-sdk-help'):
    for suffix in ('txt', 'ansi'):
        path = frames / f'{name}.{suffix}'
        text = path.read_text()
        if len(text.splitlines()) != 40 or not text.strip():
            raise SystemExit(f'{path}: expected a nonempty 120x40 frame')
        if re.search(r'sdk|pty|driver', text, re.IGNORECASE):
            raise SystemExit(f'{path}: backend vocabulary is visible')

mixed = (frames / 'fleet-mixed.txt').read_text()
if not re.search(r'^│ / +2 agents +│$', mixed, re.MULTILINE):
    raise SystemExit('expected an unfiltered two-agent fleet')
columns = []
for name in ('default-agent', 'configured-agent'):
    rows = [line for line in mixed.splitlines() if name in line]
    if len(rows) != 1 or not re.search(rf'{name}\s+claude\s', rows[0]):
        raise SystemExit(f'{name}: expected one Claude fleet row')
    columns.append(rows[0].index('claude'))
if columns[0] != columns[1]:
    raise SystemExit('Claude kind columns differ')
if not re.search(r'▎.*configured-agent', mixed):
    raise SystemExit('configured-agent must be selected before opening help')
help_text = (frames / 'fleet-sdk-help.txt').read_text()
if not re.search(r'enter\s+open in chat', help_text) or 'raw' in help_text.lower():
    raise SystemExit('selected agent help must offer chat with no raw-attach row')
PY
live_say 'PASS: mixed Claude fleet and selected agent chat-only help captured without backend vocabulary.'

live_say 'Keep claude.driver sdk; create override-agent with --driver pty.'
live_new override "$config" "$project" claude --name override-agent --driver pty
live_wait_list "$config" override-agent
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-override.txt"
live_assert_inventory "$evidence_dir/ls-override.txt" default-agent=claude/pty configured-agent=claude/sdk override-agent=claude/pty
cat "$evidence_dir/ls-override.txt" | tee -a "$evidence_dir/transcript.txt"
live_say 'PASS: default, configured driver, override and unchanged earlier agents verified.'
