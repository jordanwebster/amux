# Model and effort protocol fixture

This is a scripted app-server wire recording, not a live model capture. The
thread-start response uses the shape of the Codex 0.150.1 corpus. Two model
catalogue pages deliberately offer different efforts. Strict replay requires a
prompt turn and an empty turn to carry the selected model, effort, approval and
sandbox policy, including after reattachment with older thread defaults. It consumes no provider credentials or tokens.

`rows.jsonl` is the real host adapter output derived by
`timeout 900 wt test -- model_effort`. Regenerate with
`UPDATE_MODEL_EFFORT=1 timeout 900 wt test -- model_effort_backend`.
The shared UI specs consume the same rows and compare live and recorded folds.
