//! `amux supervise`: the daemon's parent under a desktop install.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use node::supervisor::{Inherited, Params, SuperviseOptions, UpdateSource};
use settings::{InstallationConfig, Switch, Updates};

/// The supervisor's log file under the data dir.
const SUPERVISOR_LOG: &str = "supervisor.log";

pub fn run(
    config: &InstallationConfig,
    config_path: Option<&Path>,
    inherit: Option<&str>,
) -> Result<()> {
    if config.supervisor == Switch::Off {
        bail!(
            "this install has no supervisor (supervisor: off): the service manager runs amux daemon"
        );
    }
    let inherited = inherit
        .map(|state| Inherited::parse(state).context("--inherit is not a handed-over state"))
        .transpose()?;
    crate::server::log_to(&config.root.join(SUPERVISOR_LOG))?;
    let updates = match config.updates()? {
        Updates::Manual => None,
        Updates::Auto => match node::release::release_key() {
            Some(key) => Some(UpdateSource {
                manifest_url: config.manifest_url(),
                key,
            }),
            None => {
                tracing::warn!(
                    "this build trusts no release key: it restarts the daemon but installs nothing"
                );
                None
            }
        },
    };
    let mut args: Vec<OsString> = Vec::new();
    if let Some(path) = config_path {
        args.push("--config".into());
        args.push(PathBuf::from(path).into_os_string());
    }
    let options = SuperviseOptions {
        binary: std::env::current_exe().context("finding the amux binary")?,
        args,
        data_dir: config.root.clone(),
        running: node::version()
            .parse()
            .context("this build's version is not semver")?,
        target: node::release::TARGET.to_owned(),
        updates,
        clock: Arc::new(agent_dir::SystemClock),
        params: Params::default(),
        inherited,
    };
    tracing::info!(
        version = node::version(),
        channel = %config.manifest_url(),
        "supervising"
    );
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(node::supervisor::supervise(options))
        .context("supervising the daemon")
}
