# Research: Tracking Claude Code Sessions Across Compaction

## Background

When wrapping or multiplexing Claude Code sessions, a fundamental challenge arises: Claude Code can change its
`session_id` mid-conversation. This happens during compaction (both manual `/compact` and automatic), `/clear`, and
session forking. Each new session ID comes with a new transcript file at a new path.

For a tool like amux that needs to maintain a continuous view of an agent session, this creates a mapping problem: when
a new `session_id` appears via the `SessionStart` hook, how do we know which amux agent it belongs to? A Claude instance
spinning up could have come from anywhere.

This document captures the approaches I considered and the tradeoffs of each.

## Context: How Claude Code Sessions Work

- Claude Code writes conversation data to JSONL transcript files at `~/.claude/projects/<hash>/<session-id>.jsonl`
- On compaction, Claude creates a new session with a new UUID and a new transcript file
- The original process continues (same PID, same PTY, same environment) — compaction is an internal state change, not a
  process restart
- Claude Code supports [hooks](https://code.claude.com/docs/en/hooks) that fire on lifecycle events including
  `SessionStart`, which provides the `session_id` and `transcript_path` on stdin
- Hooks can be configured globally (user settings, plugins) or per-invocation (`--settings`)

## The Core Problem

Given:

- amux agent `A` spawns Claude, which starts with session `C1`
- We establish the mapping `A <-> C1` via the `SessionStart` hook
- Claude compacts, creating session `C2`
- `SessionStart` fires again with `C2`

How does the hook handler know that `C2` belongs to agent `A`?

## Solutions Considered

### 1. Process Tree Walking (PID Matching)

When the `SessionStart` hook fires, walk up the hook process's ancestry to find a PID matching a known agent's child
process.

**Mechanism:**

- Record the child PID when spawning each agent
- In the hook handler, walk the parent PID chain upward
- Match an ancestor PID against known agent child PIDs

**Tradeoffs:**

- OS-specific implementation (`/proc/<pid>/stat` on Linux, `sysctl`/`libproc` on macOS, different again on Windows)
- `child_process.exec` in Node spawns intermediate shell processes, so the hook's direct parent isn't Claude — need to
  walk multiple levels
- Theoretical PID recycling risk (negligible in practice)
- Subagent process trees add depth and complexity

I ruled this out as unnecessarily complex for a problem that has simpler solutions.

### 2. Per-Agent Settings File with `--settings`

Generate a per-agent settings file containing a hook command with a unique identifier baked in. Pass it to Claude via
`claude --settings <path>`. Since compaction preserves the settings, the new session fires the same hook with the same
identifier.

This is what [Happy](https://github.com/slopus/happy-cli) does. They spin up an HTTP server on a random port per session
and generate a settings file with a hook command pointing to that port. The port number acts as an implicit session
identifier — a bijection between port and session.

**Mechanism:**

- Start an HTTP server on a random port (e.g., 52290)
- Write a temp settings file:
  `{ "hooks": { "SessionStart": [{ "hooks": [{ "command": "node forwarder.cjs 52290" }] }] } }`
- Launch Claude with `--settings <that file>`
- Any `SessionStart` hook (initial or post-compaction) POSTs to that port
- The server is closed over the single session it manages, so no lookup is needed

**Tradeoffs:**

- Self-contained: no global state, no installation step, no interference with other Claude instances
- Requires temp file generation and cleanup per agent
- Requires running a per-process HTTP server solely for hook forwarding
- `--settings` is present in `claude --help` but absent from
  the [official docs site](https://code.claude.com/docs/en/settings) — unclear stability commitment
- Claims the `--settings` flag exclusively, preventing users from passing their own

### 3. PreCompact Hook with Next-Session Association

Handle the `PreCompact` hook to mark an agent as "expecting rollover." When the next `SessionStart` arrives with an
unknown session ID, associate it with the marked agent.

**Mechanism:**

- On `PreCompact`, flag the agent as expecting a new session
- On next unrecognized `SessionStart`, match it to the flagged agent

**Tradeoffs:**

- Fragile under concurrency: if two agents compact simultaneously, the mapping is ambiguous
- Race conditions between `PreCompact` and `SessionStart` delivery
- Doesn't cover all session transitions (fork, clear) without additional hook handling

I ruled this out as insufficiently robust, though it would likely work in single-agent scenarios.

### 4. Environment Variable at Spawn Time (Selected Approach)

Set an environment variable identifying the agent before spawning Claude. A globally installed hook reads this variable
and includes it when reporting to the amux server. Since compaction doesn't change the OS process, the environment is
preserved.

**Mechanism:**

- On `amux new-agent claude`, set `AMUX_AGENT_ID=<uuid>` in the spawn environment
- Global plugin's `SessionStart` hook reads `$AMUX_AGENT_ID` and sends it + `session_id` to the amux server via Unix
  socket
- On compaction: same process, same env, same variable — mapping works automatically

**Tradeoffs:**

- Simplest approach: one env var, no temp files, no cleanup, no per-agent servers
- Built entirely on stable, documented features: Unix env
  inheritance, [SessionStart hooks](https://code.claude.com/docs/en/hooks#sessionstart), [plugins](https://code.claude.com/docs/en/plugins)
- Requires a global plugin installation step (one-time, already part of amux setup)
- Works on Windows (same env inheritance semantics via `CreateProcess`)
- Composable: doesn't claim any CLI flags, users can still pass `--settings` or use other plugins

The assumption that compaction doesn't re-exec the process is safe — re-execing would lose the PTY file descriptor,
which is effectively impossible without explicit fd passing.

### 5. CLAUDE_ENV_FILE Persistence (Variant of Option 4)

Claude's `SessionStart` hook has access to a `CLAUDE_ENV_FILE` environment variable pointing to a file where hooks can
persist environment variables for subsequent Bash commands. This could serve as an additional persistence layer.

**Mechanism:**

- In the `SessionStart` hook, write `export AMUX_AGENT_ID=$AMUX_AGENT_ID` to `$CLAUDE_ENV_FILE`
- Provides persistence even if process environment were somehow lost

**Tradeoffs:**

- The docs state env file variables are available to "subsequent Bash commands" — unclear if hooks can read them
- Unclear whether the env file persists across compaction boundaries
- Unnecessary if process env inheritance works (which it does)

I considered this as a belt-and-suspenders addition but decided it adds complexity without meaningful benefit over plain
env inheritance.

## Alternative Delivery: Static Settings File + Env Var

A hybrid worth noting: instead of a global plugin, ship a single static settings file with amux and use `--settings` to
load it on every spawn:

```
AMUX_AGENT_ID=<uuid> claude --settings /path/to/amux/hooks.json
```

The settings file is generic (same for all agents), the env var provides per-agent differentiation. No temp files, no
generation.

The downside is that `--settings` is exclusive — users can't pass their own. The plugin approach avoids this since
plugins merge with all other hook sources.

## Decision

Going with **Option 4**: environment variable at spawn time with the global plugin handling hook delivery. It's the
simplest, most robust, fully documented, and composable with other tools and user configuration.

## References

- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks)
- [Claude Code Settings](https://code.claude.com/docs/en/settings)
- [Claude Code Plugins](https://code.claude.com/docs/en/plugins)
- [Happy CLI Source](https://github.com/slopus/happy-cli)
