# Operating Rules

amux is unreleased and in active development. Never preserve backwards
compatibility — old local protocols, config keys, and public APIs may be
broken freely. Simplify decisions accordingly.

- Wrap every test invocation in `timeout`. A firing timeout is a hang to
  diagnose, not a slow suite.
- Update `DEVLOG.md` in the same commit as each chunk of work.
- Never add Co-Authored-By trailers.
- Committed documentation lives in `docs/` (start at `docs/README.md`);
  `notes/` is gitignored working material. Graduate notes into `docs/`
  deliberately; never force-add them.
- This file holds rules about how agents operate. Documentation about the
  source belongs in `docs/` and component READMEs, not here.

## Handoff

Machinery: `/Users/jlw/source/skills` (authoritative rules in its `docs/SPEC.md`).

Repo config: `.handoff.toml`

These are standing agent obligations. This AGENTS.md section instructs the task
agent; the packet verifier and audit independently attack the resulting claims
and evidence.

- Write intended claims at scaffold, before gathering evidence.
- Apply all four evidence standards: altitude matches the claim; boundary
  evidence crosses the named production path; observation, comparison, and
  oracle acceptance remain separate; hygiene excludes secrets, tokens, and
  private paths and declares nondeterminism and redactions.
- A user-observable claim's evidence must include a human-inspectable
  observation at the claim's altitude, with its replay command, or the claim is
  declared unwitnessed with the residual risk; internal claims carry no witness
  field.
- Treat evidence work required by a claim as presumptively in scope. Surface a
  material expansion of cost, architecture, repository shape, or review
  latency before building it. If Jordan is unavailable, degrade honestly:
  mark affected claims inconclusive and file the prerequisite. Necessary work
  is in; merely noticed work is filed as a braindump.
- Journal friction as it happens.
- Make granular commits and never squash a task branch.
- Assemble the packet at task end.
