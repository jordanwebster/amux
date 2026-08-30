#!/bin/sh

set -eu

if [ "${AMUX_OFFLINE:-0}" = 1 ]; then
    echo "workspace-test: offline sandbox; skipping 10 TCP-only tests"
    timeout 900 cargo test --workspace --all-targets --no-run
    timeout 900 cargo test --workspace --exclude amux --all-targets
    timeout 900 cargo test -p amux --lib -- \
        --skip auth::cloud::tests::network_failures_remain_retriable_connection_errors \
        --skip services::startup::tests::cloud_pin_pairing_updates_both_trust_stores \
        --skip services::startup::tests::cloud_qr_pairing_updates_both_trust_stores \
        --skip services::startup::tests::cloud_routing_service_drives_remote_agent_inventory \
        --skip services::startup::tests::cloud_routing_service_serves_tcp_listener \
        --skip services::startup::tests::cloud_tls_incoming_accepts_new_socket_while_first_handshake_stalls \
        --skip services::startup::tests::direct_pin_pairing_over_tcp_updates_both_trust_stores \
        --skip services::startup::tests::direct_tcp_reachabilities_on_both_peers_establish_two_outbound_links \
        --skip services::startup::tests::direct_tcp_reachability_establishes_runtime_link_from_trust_store
    for test_target in \
        a2a_fixtures \
        claude_pty_live \
        claude_pty_live_args \
        claude_sdk_live \
        claude_sdk_live_args \
        codex_live \
        codex_live_args \
        codex_live_depfile \
        derived_rows \
        typed_protocols
    do
        timeout 900 cargo test -p amux --test "$test_target"
    done
    timeout 900 cargo test -p amux --test embedded -- \
        --skip embedded_server_does_not_poll_for_updates
    exit 0
fi

exec timeout 900 cargo test --workspace --all-targets
