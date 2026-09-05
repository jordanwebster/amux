#!/bin/sh
# Live inventory proof using the operator's Claude login. No prompts are sent.
# Usage: timeout 900 e2e-tests/claude_driver_config.sh [evidence-directory]
set -eu
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
live_name=config
# shellcheck source=e2e-tests/live_common.sh
. "$repo_root/e2e-tests/live_common.sh"
live_init "${1:-$repo_root/.autopilot/evidence/live/config}"
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

live_say 'Keep claude.driver sdk; create override-agent with --driver pty.'
live_new override "$config" "$project" claude --name override-agent --driver pty
live_wait_list "$config" override-agent
timeout 30 "$amux_bin" --config "$config" ls > "$evidence_dir/ls-override.txt"
live_assert_inventory "$evidence_dir/ls-override.txt" default-agent=claude/pty configured-agent=claude/sdk override-agent=claude/pty
cat "$evidence_dir/ls-override.txt" | tee -a "$evidence_dir/transcript.txt"
live_say 'PASS: default, configured driver, override and unchanged earlier agents verified.'
