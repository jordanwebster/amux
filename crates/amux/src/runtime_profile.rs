//! Localized runtime gates for embedded/mobile client-only builds.
//!
//! The first mobile milestone uses Cargo feature selection rather than a public
//! runtime platform API: desktop defaults keep `local-agents`, while embedded
//! clients use `default-features = false`.

pub(crate) fn local_agents_enabled() -> bool {
    cfg!(feature = "local-agents")
}

pub(crate) fn sleep_inhibition_enabled() -> bool {
    local_agents_enabled()
}

pub(crate) fn local_client_listener_enabled() -> bool {
    local_agents_enabled()
}

pub(crate) fn external_tcp_listener_enabled() -> bool {
    local_agents_enabled()
}

pub(crate) fn direct_reachability_enabled() -> bool {
    local_agents_enabled()
}

pub(crate) fn periodic_update_checks_enabled() -> bool {
    local_agents_enabled()
}
