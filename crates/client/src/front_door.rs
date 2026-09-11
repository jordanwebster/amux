use tonic::transport::Channel;

use crate::ProfileAdminClient;

/// An installation administration client that does not select a process-wide
/// profile. Each profile operation carries its immutable account identifier.
#[derive(Clone)]
pub struct FrontDoorClient {
    pub profiles: wire::profile_service_client::ProfileServiceClient<Channel>,
    pub installation: wire::installation_service_client::InstallationServiceClient<Channel>,
}

impl FrontDoorClient {
    pub fn from_channel(channel: Channel) -> Self {
        Self {
            profiles: wire::profile_service_client::ProfileServiceClient::new(channel.clone()),
            installation: wire::installation_service_client::InstallationServiceClient::new(
                channel,
            ),
        }
    }

    pub fn admin(&self, id: model::ProfileId) -> ProfileAdminClient {
        ProfileAdminClient::new(id, self.profiles.clone())
    }

    #[cfg(not(unix))]
    pub async fn connect_socket(_path: &std::path::Path) -> Result<Self, crate::ConnectError> {
        Err(crate::ConnectError::Unsupported)
    }

    #[cfg(unix)]
    pub async fn connect_socket(path: &std::path::Path) -> Result<Self, crate::ConnectError> {
        Ok(Self::from_channel(
            crate::connect::connect_socket(path).await?,
        ))
    }
}
