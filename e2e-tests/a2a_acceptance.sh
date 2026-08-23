#!/bin/sh
# Operator-run acceptance for agent-to-agent messaging: a real Claude parent
# spawns a real Codex child through the amux tools, the child completes, and
# the parent receives the completion. Only a human with both harnesses logged
# in can run this; it drives the production boundary (typing into the parent)
# rather than any internal API, so what it proves is what a user would see.
#
# Usage: e2e-tests/a2a_acceptance.sh [result-file]
#   With a result-file, PASS/FAIL and the timestamp are written there.
set -u

amux=${AMUX_BIN:-amux}
parent=${A2A_PARENT_NAME:-a2a-parent}
child=${A2A_CHILD_NAME:-a2a-child}
token=A2A_ACCEPT_DONE
result_file=${1:-}

say() { printf '\n%s\n' "$*"; }
fail() { say "FAIL: $*"; record FAIL; exit 1; }
record() {
  if [ -n "$result_file" ]; then
    printf '%s %s parent=%s child=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$parent" "$child" >> "$result_file"
  fi
}
wait_for() { # wait_for <seconds> <description> <command...>
  secs=$1; what=$2; shift 2
  i=0
  while [ "$i" -lt "$secs" ]; do
    if "$@" >/dev/null 2>&1; then return 0; fi
    i=$((i + 5)); sleep 5
  done
  fail "timed out after ${secs}s waiting for $what"
}

timeout 20 "$amux" list >/dev/null 2>&1 || fail "amux daemon is not reachable ('$amux list' failed); start it and retry"

say "1. In another terminal, start the parent and keep it attached:"
say "     $amux new claude --name $parent"
wait_for 300 "the parent '$parent' to appear in 'amux list'" sh -c "timeout 20 '$amux' list | grep -q -- '$parent'"
say "   parent is running."

say "2. Type this into the parent's Claude session, verbatim:"
say "     Use the amux spawn tool to create a codex child named $child with the prompt"
say "     \"Reply with exactly $token and nothing else.\" Then tell me, word for word, what the child replied."
wait_for 600 "the child '$child' to appear under '$parent' in 'amux list --all'" sh -c "timeout 20 '$amux' list --all | grep -q -- '$child'"
say "   child is running. Family as amux lists it:"
timeout 20 "$amux" list --all | grep -n -e "$parent" -e "$child"

say "3. Waiting for the child to finish its turn (its 'working on' text clears on completion)..."
sleep 20
timeout 20 "$amux" list --all | grep -n -e "$parent" -e "$child"

say "4. Look at the parent's session. Did Claude report the child's reply ($token),"
say "   and does its transcript show the message arriving from '$child'? [y/N]"
read -r answer
case "$answer" in
  y|Y|yes|YES) say "PASS: parent received the child's completion"; record PASS ;;
  *) fail "operator did not observe the completion in the parent" ;;
esac

say "5. Optional cleanup (deletes the parent and its child):"
say "     $amux rm $parent --force"
exit 0
