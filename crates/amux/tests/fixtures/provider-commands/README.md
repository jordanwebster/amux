# Provider command protocol fixture

This is scripted upstream IO using the app-server `skills/list` and `turn/start`
shapes, not a live model capture. Enabled, uniquely named skills in the requested
working directory become commands. A command sends the provider's typed skill
item and exact argument text; its path is resolved on the host. The terminal's
built-in slash menu is not reported by this API and is not synthesized here.

Strict replay rejects any unexpected write. The fixture includes disabled,
ambiguous and out-of-directory skills and a failed refresh that clears stale
commands. No provider credentials or tokens are used.

`rows.jsonl` contains real host readiness output consumed by the shared reducer
specs. Regenerate with `UPDATE_PROVIDER_COMMANDS=1 timeout 900 wt test --
provider_commands_backend`; run all command proofs with
`timeout 900 wt test -- provider_commands -- --nocapture`.
