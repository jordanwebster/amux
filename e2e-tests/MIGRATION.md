# Legacy end-to-end migration map

This map accounts for every top-level legacy end-to-end workload. `pending`
means the source must remain until the named replacement is present and
passing; `done` means the disposition has already been completed.

| file | what it uniquely asserts | disposition | replacement | status |
| --- | --- | --- | --- | --- |
| `e2e-tests/attach.test` | A second terminal can attach to one local agent and both terminals exchange input and output. | replace | journey `second-attach` | done |
| `e2e-tests/bare_help.test` | A non-TTY bare invocation prints the main help and pairing help. | replace | `process::bare_help_prints_cli_and_pair_help_without_a_tty` | done |
| `e2e-tests/cloud_relay_connection.test` | Two signed-in cloud profiles can list and attach to an agent through the relay using the production CLI. | replace | journey `reach-host` | done |
| `e2e-tests/config_split.test` | A split installation initializes, lists, stops and restarts multiple profiles. | replace | `process::split_installation_starts_stops_and_lists_profiles` | done |
| `e2e-tests/free_tier.test` | A free account keeps host presence while refusing agent access, then exposes the agent after an in-place Pro refresh. | replace | journey `authority-boundaries` | done |
| `e2e-tests/init_onramp.test` | Fresh initialization prints a pairing code, expiry, LAN phone guidance and login upsell. | replace | journey `reach-host` | done |
| `e2e-tests/lan_discover_pair_attach.test` | Real multicast discovery, PIN pairing and direct attachment work without an account. | replace | journey `reach-host` | done |
| `e2e-tests/lan_qr_multicast_blocked.test` | QR addresses permit direct pairing and attachment when multicast is blocked. | replace | journey `reach-host` | done |
| `e2e-tests/list_agents.test` | The CLI lists two local agents and their working directories. | replace | `process::list_prints_local_agents_and_working_directories` | done |
| `e2e-tests/local_agent_ended.test` | A local attachment reports when its agent process ends. | replace | `process::local_attach_reports_agent_exit` | done |
| `e2e-tests/multiple_agents.test` | Two concurrent local agents remain independently interactive. | replace | `process::two_local_agents_remain_independently_interactive` | done |
| `e2e-tests/new_agent.test` | A newly created test agent round-trips terminal input. | replace | `process::new_agent_round_trips_terminal_input` | done |
| `e2e-tests/peer_list.test` | The CLI distinguishes a discovered LAN host from a trusted direct peer. | replace | journey `reach-host` | done |
| `e2e-tests/profile_lifecycle.test` | The CLI creates, renames, pauses, resumes, logs out and confirmation-deletes a profile. | replace | journey `authority-boundaries` | done |
| `e2e-tests/profile_login.test` | Device authorization binds two profiles to distinct accounts and prints their names, emails and tiers. | replace | `process::two_profile_logins_print_bound_accounts` | done |
| `e2e-tests/profile_selector.test` | Separate profile fleets and terminals remain isolated when the selected profile changes. | replace | journey `authority-boundaries` | done |
| `e2e-tests/profile_server_stop.test` | Installation stop kills agents in every profile, notifies attached clients and prevents resume. | replace | `process::server_stop_kills_agents_in_every_profile` | done |
| `e2e-tests/profile_worktree.test` | The worktree template and generator start and probe a daemon in a fresh worktree. | replace | `process::worktree_template_starts_and_probes_daemon` | done |
| `e2e-tests/relay_udp_blocked.test` | Nothing beyond one QUIC and one TCP-fallback device exchanging a relayed session. | delete | redundant with `relay::a_quic_device_and_a_tcp_device_reach_each_other_through_one_relay` | done |
| `e2e-tests/remote_agent_ended.test` | A remote attachment reports when its agent process ends. | replace | `process::remote_attach_reports_agent_exit` | done |
| `e2e-tests/remote_attach_by_alias.test` | An exact remote alias selects the intended agent for a bidirectional direct session. | replace | journey `second-attach` | done |
| `e2e-tests/remote_connection.test` | Nothing beyond a paired peer attaching bidirectionally to a remote agent over a direct link. | delete | redundant with `sessions::a_peer_attaches_to_a_remote_agent_over_a_direct_link` | done |
| `e2e-tests/remote_list_agents.test` | The CLI lists a paired remote agent and its working directory. | replace | `process::list_prints_remote_agents_and_working_directories` | done |
| `e2e-tests/replay_buffer.test` | A later attachment immediately receives prior output and both terminals receive subsequent output. | replace | journey `second-attach` | done |
| `e2e-tests/server_lifecycle.test` | Server start is idempotent and stop releases the running daemon. | replace | `process::server_start_is_idempotent_and_stop_releases_daemon` | done |
| `e2e-tests/server_suspend_notification.test` | An attached client sees suspension and can resume the same agent. | replace | journey `agent-lifecycle` | done |
| `e2e-tests/third_party_client.test` | A generated third-party client discovers profile sockets and their isolated agent fleets. | replace | `process::third_party_client_discovers_profile_sockets_and_agents` | done |
| `e2e-tests/update_two_profiles.test` | Replacing the executable suspends and resumes two active profiles while preserving a parked agent. | replace | `process::update_replaces_binary_and_resumes_all_profiles` | done |
| `e2e-tests/a2a_acceptance.sh` | A real Claude parent spawns a real Codex child through amux MCP and receives its completion. | move | `qualification::a2a_cross_kind_completion` | done |
| `e2e-tests/attachments_cross_host.sh` | A real cross-host review preserves exact diff identities, lines and comments across a second viewer and host reconnect. | move | `qualification::cross_host_review_survives_reconnect` | done |
| `e2e-tests/claude_driver_config.sh` | Driver defaults and overrides affect only new agents while fleet output hides backend vocabulary and SDK mode offers chat only. | move | `qualification::claude_driver_configuration` | done |
| `e2e-tests/desktop_free_live.sh` | The real terminal UI presents host loss, host selection, payment refusal and restored access after an entitlement refresh. | replace | journey `authority-boundaries` | done |
| `e2e-tests/live_common.sh` | No standalone behavior; it provides isolated tmux and daemon setup plus frame and inventory assertions for live scripts. | move | `qualification::support::live` | done |
| `e2e-tests/onramp_live.sh` | The real terminal onramp discovers a host through multicast, pairs by PIN and attaches directly without an account. | replace | journey `reach-host` | done |
| `e2e-tests/relay_quic_smoke.sh` | The real relay binary carries one TCP-fallback host and one QUIC host in the same bidirectional session. | replace | `qualification::relay_mixed_carriers` | done |
| `e2e-tests/remote_open_live.sh` | Real Claude and Codex agents apply remote-versus-local open policy, preserve provider raw chrome and report accurate inventory kinds. | move | `qualification::remote_open_entry_policy` | done |
| `e2e-tests/sdk_chat_live.sh` | A real Claude SDK session covers streaming, interruption, permissions, edits, plans, questions, elicitation and parent-child messaging in the terminal UI. | move | `qualification::claude_sdk_chat` | done |
| `e2e-tests/store_scenarios.sh` | Store-backed warm start, chat, two-terminal fanout, gap recovery and SDK resume stories render through the real terminal UI. | replace | journeys `leave-and-recover` and `second-attach` | done |
| `e2e-tests/user_hooks_live.sh` | A real user Stop hook runs both under direct Claude and under the amux SDK backend. | move | `qualification::claude_sdk_preserves_user_hooks` | done |
| `e2e-tests/sdk_chat_rows.py` | Captured Claude SDK rows can be queried by cursor, prompt, status, text, tools, permissions, plans, questions and elicitation predicates. | move | `scripts/tests/sdk_chat_rows.py` | done |
