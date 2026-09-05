//! The only owner of the embedded account runtime and its UI reducer.

use std::path::PathBuf;
use std::sync::Arc;

use amux::{Client, CredentialProvider, EmbeddedRelay, RelayConnection, RelayEndpoint};
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

impl StartConfig {
    fn server_config(&self) -> amux::Config {
        amux::Config {
            host_name: self.device_name.clone(),
            data_dir: self.data_dir.clone(),
            state_path: self.data_dir.join("state.yaml"),
            socket_path: self.data_dir.join("amux.sock"),
            enable_cloud_mode: Some(false),
            prevent_idle_sleep: Some(false),
            ..Default::default()
        }
    }

    pub fn endpoint(&self) -> Result<RelayEndpoint, String> {
        if !self.data_dir.is_absolute()
            || !self.cache_dir.is_absolute()
            || !self.log_path.is_absolute()
        {
            return Err("mobile paths must be absolute and device name must be nonempty".into());
        }
        self.server_config()
            .validate(false)
            .map_err(|e| e.to_string())?;
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
    // Keep the server alive until the reducer and its tasks are dropped.
    pub client: Client,
}

impl MobileRuntime {
    pub async fn open(
        config: &StartConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self, String> {
        let endpoint = config.endpoint()?;
        let (connection, relay) = watch::channel(RelayConnection::Connecting);
        let client = amux::Server::builder()
            .config(config.server_config())
            .embedded()
            .relay(EmbeddedRelay {
                endpoint,
                credentials,
                connection,
            })
            .open()
            .await
            .map_err(|e| e.to_string())?;
        let ui = Runtime::start_with_client(
            client.clone(),
            RuntimeOptions {
                report_dir: Some(config.data_dir.join("reports")),
                log_path: Some(config.log_path.clone()),
                artifact_cache: Some(config.cache_dir.join("artifacts")),
                ..Default::default()
            },
        );
        Ok(Self { ui, relay, client })
    }
}
