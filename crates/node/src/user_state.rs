use std::sync::Arc;

use host_api::LocalAgentHost;
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::subscription::SubscriptionReporter;
use crate::update::UpdateReporter;

pub(crate) struct ServerState {
    pub(crate) config: Config,
    pub(crate) host_id: Uuid,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
    pub(crate) update_reporter: Option<Arc<dyn UpdateReporter>>,
    pub(crate) is_cloud_server: bool,
    pub(crate) jwt_validator: Option<Arc<JwtValidator>>,
    pub(crate) local_agent_host: Option<Arc<dyn LocalAgentHost>>,
}

impl ServerState {
    pub(crate) fn new(
        config: Config,
        host_id: Uuid,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
    ) -> Self {
        Self {
            config,
            host_id,
            credentials,
            subscription_reporter: None,
            update_reporter,
            is_cloud_server: false,
            jwt_validator: None,
            // Device startup injects the host after entering an async runtime;
            // cloud relays and service-level tests intentionally keep `None`.
            local_agent_host: None,
        }
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.config.host_name
    }

    pub(crate) fn tcp_port(&self) -> Option<u16> {
        self.config.tcp_port
    }

    pub(crate) fn jwt_validator(&self) -> Option<Arc<JwtValidator>> {
        self.jwt_validator.clone()
    }

    pub(crate) fn minimum_client_version(&self, client_id: &str) -> Option<String> {
        self.config.minimum_client_versions.get(client_id).cloned()
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }

    pub(crate) fn local_agent_host(&self) -> Option<Arc<dyn LocalAgentHost>> {
        self.local_agent_host.clone()
    }
}
