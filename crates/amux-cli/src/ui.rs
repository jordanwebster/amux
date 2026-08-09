//! Bare `amux` (and `amux ui`): the fleet TUI.
//!
//! Dispatch order is deliberate: an uninitialized machine runs the init flow
//! FIRST — auth stays CLI-owned and the TUI stays auth-passive — then the
//! fleet opens. Expired cloud auth never blocks here: the TUI opens
//! instantly and renders the degraded banner from Model state.

use amux::Config;
use amux_tui::{TuiConfig, run_fleet};
use amux_ui::{ConnectFailure, Connector, Runtime, RuntimeOptions};
use anyhow::Result;

use crate::client_common::get_client;
use crate::init::{self, InitContext};

pub async fn run(mut config: Config) -> Result<()> {
    if init::needs_init(&config) {
        init::run_init(&mut config, InitContext::implicit(), false).await?;
    }

    // The local host id comes from the stored device identity — the wire
    // does not mark the local host (see docs/UI.md, subscription policy).
    let local_host_id = amux::setup::local_host_id();

    let connector: Connector = {
        let config = config.clone();
        Box::new(move || {
            let config = config.clone();
            Box::pin(async move {
                // get_client spawns the daemon when absent; while it runs
                // the TUI shows the "Starting daemon…" state.
                get_client(&config).await.map_err(|error| ConnectFailure {
                    message: format!("{error:#}"),
                    auth_required: false,
                })
            })
        })
    };
    let mut runtime = Runtime::start(
        connector,
        RuntimeOptions {
            local_host_id,
            dump_dir: Some(amux::default_data_dir().join("ui-dumps")),
            ..RuntimeOptions::default()
        },
    );

    let tui_config = TuiConfig {
        working_dir: std::env::current_dir()?,
        leader_label: format!("C-{}", config.keybinds.leader.char as char),
        default_agent_type: amux::AgentType::Claude,
    };

    let attach_config = config.clone();
    run_fleet(&mut runtime, tui_config, move |agent| {
        let config = attach_config.clone();
        async move { crate::session_client::attach_for_ui(&config, agent).await }
    })
    .await
}
