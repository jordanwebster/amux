//! The only owner of the embedded account runtime and its UI reducer.

use std::path::PathBuf;
use std::sync::Arc;

use amux::{CredentialProvider, EmbeddedRelay, RelayConnection, RelayEndpoint};
use amux_ui::{Runtime, RuntimeOptions};
use serde::Deserialize;
use tokio::sync::watch;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConfig {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub device_name: String,
    pub relay: RelayConfig,
    pub log_path: PathBuf,
    #[serde(default = "default_frame_interval_ns")]
    pub frame_interval_ns: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub url: String,
    pub tls: RelayTls,
    pub token: TokenSource,
}

#[derive(Clone, Deserialize)]
pub enum RelayTls {
    System,
    PlainLoopback,
}

#[derive(Clone, Deserialize)]
pub enum TokenSource {
    Static(String),
    Callback,
}

fn default_frame_interval_ns() -> u64 {
    16_666_667
}

impl StartConfig {
    fn server_config(&self) -> amux::Config {
        amux::Config {
            host_name: self.device_name.clone(),
            data_dir: self.data_dir.clone(),
            state_path: self.data_dir.join("state.yaml"),
            socket_path: self.data_dir.join("amux.sock"),
            prevent_idle_sleep: Some(false),
            ..Default::default()
        }
    }

    pub fn endpoint(&self) -> Result<RelayEndpoint, String> {
        if self.frame_interval_ns == 0 || self.frame_interval_ns > 1_000_000_000 {
            return Err("frame_interval_ns must be between 1 and 1000000000".into());
        }
        if !self.data_dir.is_absolute()
            || !self.cache_dir.is_absolute()
            || !self.log_path.is_absolute()
        {
            return Err("mobile paths must be absolute and device name must be nonempty".into());
        }
        self.server_config().validate().map_err(|e| e.to_string())?;
        match self.relay.tls {
            RelayTls::System => RelayEndpoint::system(&self.relay.url).map_err(|e| e.to_string()),
            RelayTls::PlainLoopback => {
                #[cfg(feature = "debug-tools")]
                {
                    let address = self
                        .relay
                        .url
                        .strip_prefix("http://")
                        .ok_or("plaintext relay must use http://")?
                        .parse()
                        .map_err(|_| "plaintext relay must be a literal socket address")?;
                    RelayEndpoint::plain_loopback(address).map_err(|e| e.to_string())
                }
                #[cfg(not(feature = "debug-tools"))]
                Err("plaintext relay requires debug-tools".into())
            }
        }
    }
}

pub struct MobileRuntime {
    pub ui: Runtime,
    pub relay: watch::Receiver<RelayConnection>,
    /// The way to ask the relay connection to stop waiting out its backoff and
    /// dial now. Held here rather than reached through the embedded runtime
    /// because this is the side of the app that owns the relay: the embedded
    /// runtime is the machine, and this is the phone's link to everything else.
    pub retry: std::sync::Arc<amux::RelayRetry>,
    // Keep the server alive until the reducer and its tasks are dropped.
    pub embedded: amux::EmbeddedRuntime,
}

impl MobileRuntime {
    pub async fn open(
        config: &StartConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self, String> {
        let endpoint = config.endpoint()?;
        let (connection, relay) = watch::channel(RelayConnection::Connecting);
        let retry = std::sync::Arc::new(amux::RelayRetry::default());
        let embedded = amux::Server::builder()
            .config(config.server_config())
            .embedded()
            .relay(EmbeddedRelay {
                endpoint,
                credentials,
                connection,
                retry: retry.clone(),
            })
            .open()
            .await
            .map_err(|e| e.to_string())?;
        let ui = Runtime::start_with_client(
            embedded.client(),
            RuntimeOptions {
                host_inventory: Some(embedded.admin()),
                // Which host is this device. Nothing infers it for an
                // embedded runtime, and without it this phone cannot tell
                // itself from the machines it is paired with.
                local_host_id: Some(embedded.host_id()),
                report_dir: Some(config.data_dir.join("reports")),
                log_path: Some(config.log_path.clone()),
                artifact_cache: Some(config.cache_dir.join("artifacts")),
                ..Default::default()
            },
        );
        Ok(Self {
            ui,
            relay,
            retry,
            embedded,
        })
    }
}
