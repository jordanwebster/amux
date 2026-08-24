//! Bare `amux` (and `amux ui`): the fleet TUI.
//!
//! Dispatch order is deliberate: an uninitialized machine runs the init flow
//! FIRST — auth stays CLI-owned and the TUI stays auth-passive — then the
//! fleet opens. Expired cloud auth never blocks here: the TUI opens
//! instantly and renders the degraded banner from Model state.

use std::sync::Arc;

use amux::Config;
use amux_tui::{TuiConfig, run_fleet};
use amux_ui::{ConnectFailure, Connector, Runtime, RuntimeOptions};
use anyhow::Result;

use crate::client_common::get_client;
use crate::init::{self, InitContext};
use crate::update::MarkerFileReporter;

pub async fn run(config: Config) -> Result<()> {
    run_inner(config, None, None).await
}

pub(crate) async fn run_for_agent(
    config: Config,
    agent: amux::AgentId,
    codex_configuration: Option<String>,
) -> Result<()> {
    run_inner(config, Some(agent), codex_configuration).await
}

async fn run_inner(
    mut config: Config,
    initial_chat: Option<amux::AgentId>,
    initial_chat_configuration: Option<String>,
) -> Result<()> {
    amux::setup::ensure_legacy_claude_plugin_removed(&config).await?;
    if init::needs_init(&config) {
        init::run_init(&mut config, InitContext::implicit(), false).await?;
    }

    // The local host id comes from the stored device identity — the wire
    // does not mark the local host (see docs/UI.md, subscription policy).
    let local_host_id = amux::setup::local_host_id(&config);
    let subscription_reporter = MarkerFileReporter::from_state_path(&config.state_path);

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
                    subscription_required: false,
                })
            })
        })
    };
    let mut runtime = Runtime::start(
        connector,
        RuntimeOptions {
            local_host_id,
            dump_dir: Some(config.data_dir.join("ui-dumps")),
            subscription_status_provider: Some(Arc::new(move || {
                subscription_reporter.subscription_required()
            })),
            ..RuntimeOptions::default()
        },
    );
    // A panic anywhere in the TUI leaves a Msg recording: the terminal.rs
    // panic hook calls amux_ui::write_panic_dump after restoring the
    // terminal.
    runtime.install_panic_dump();

    let tui_config = TuiConfig {
        working_dir: std::env::current_dir()?,
        leader: config.keybinds.leader.char as char,
        // The A1 entry-mode setting, from the usual amux config
        // (`ui.default_open_mode`; shipped default: raw attach).
        default_open_mode: match config.ui.default_open_mode {
            amux::OpenMode::Raw => amux_tui::OpenMode::RawAttach,
            amux::OpenMode::Chat => amux_tui::OpenMode::Chat,
        },
        default_agent_type: amux::AgentType::Claude,
        initial_chat,
        initial_chat_configuration,
    };

    let attach_config = config.clone();
    run_fleet(&mut runtime, tui_config, move |agent| {
        let config = attach_config.clone();
        async move { crate::session_client::attach_for_ui(&config, agent).await }
    })
    .await
}
