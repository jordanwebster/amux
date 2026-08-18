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

Every substantive chunk of work ends with a handoff; use the `handoff` skill from the start.

Machinery: `/Users/jlw/source/skills` (authoritative rules in its `docs/SPEC.md`).

Repo config: `.handoff.toml`
