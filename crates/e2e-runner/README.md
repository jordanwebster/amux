# E2E runner

Run real CLI and agent processes with `wt run e2e`, or select test names with
`wt run e2e -- profile`. Tests live in `e2e-tests/*.test`. Set
`E2E_TRANSCRIPT_DIR` to capture commands and their actual output as text files.
The runner's default binary build also goes through `wt build`.

Each `config` defines an isolated installation. `profiles: [personal, work]`
creates named unbound profiles within it; otherwise it has one profile named
after the config. Commands receive the first profile's `--config` path and can
select another with `--profile`. A terminal with `installation: true` instead
uses the fixture's default installation configuration, including last-used
selection. The fixture never writes credentials or a bound registry entry.

A config with `cloud_relay: true` supplies the identity fixture and relay TLS
material. Start its relay with `amux server start --cloud --foreground` in a
terminal. Its `accounts: [alice, bob]` list determines successive auto-approved
device logins; once exhausted, the last account repeats (Alice by default).
Both accounts have a name and email. Missing requested scopes, reused device
codes or refresh tokens, and invalid access tokens are refused. An optional
`cloud_account: alice` on a device logs it in through the CLI during setup.

Set `update_version: 999.0.0` on the relay config to serve a higher-version
manifest at `/update/manifest.json` and the current executable at `/update/amux`.
The runner copies the executable into a disposable directory before running
such a test, so `amux update` never replaces the shared build output. An optional
`suspended_agent: already-parked` on a device seeds a valid retained session in
its first profile and checks that record is unchanged after the test. The CLI
fleet assertions separately prove it was not resumed.

A config with `worktree: true` renders the checked-in `.wt.toml` installation
template and invokes `scripts/worktree-profile.py` in a fresh temporary
checkout directory. This tests the generated layout without creating a Git
worktree or touching the developer's running daemon.

Output lines compare exactly. Additional directives:

- `@@retry <timeout-ms> <interval-ms>` retries a one-shot command until the next
  exact output comparison succeeds.
- `@@capture <name> <prefix>` captures the remainder of one output line.
- `@@contains <text>` asserts a stable fragment of a completed command whose
  output includes a generated identifier or asynchronous status.
- `@@exit [code]` waits for the active PTY command to exit (zero by default),
  allowing another command in that terminal.
- `@@process-exited <pid>` requires a captured process to disappear on Unix.

Variables include `$config.id`, `$config.front_door`, `$config.socket_path`,
`$config.profile.id`, `$config.profile.socket_path`, `$directory.path`, and
captured names. For example, `e2e-runner client list-profiles <front-door>` and
`e2e-runner client list-agents <front-door> <label-or-uuid>` use a tonic client
generated directly from the public proto. This client has no dependency on
the amux crate: it discovers sockets over ProfileService, then calls
ClientService on the selected socket.
