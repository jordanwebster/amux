//! Public-process test ownership for isolated embedded node instances.

use node::{
    CredentialSource, Installation, InstallationOptions, InstallationRoot, InstallationSettings,
    Listeners,
};

/// An installation paired with the temporary root whose lifetime owns its files.
pub struct TestInstallation {
    owner: Installation,
    _root: tempfile::TempDir,
}

impl TestInstallation {
    pub async fn embedded(name: &str) -> Result<Self, node::InstallationError> {
        let root = tempfile::tempdir()?;
        let owner = Installation::open(InstallationOptions {
            root: InstallationRoot::OnDisk(root.path().into()),
            settings: InstallationSettings {
                host_name: name.into(),
                prevent_idle_sleep: Some(false),
                keybinds: Default::default(),
                ui: Default::default(),
                claude: Default::default(),
                keymaps_dir: root.path().join("keymaps"),
                minimum_client_versions: Default::default(),
                update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
                status_reporters: Default::default(),
            },
            listeners: Listeners::InProcessOnly,
            credentials: CredentialSource::ProfileFiles,
            identity_http: Default::default(),
            host_factory: None,
        })
        .await?;
        Ok(Self { owner, _root: root })
    }

    pub fn owner(&self) -> &Installation {
        &self.owner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn independent_nodes_keep_distinct_profiles() {
        let left = TestInstallation::embedded("left").await.unwrap();
        let right = TestInstallation::embedded("right").await.unwrap();
        let left_profile = left
            .owner()
            .create(node::OperationId::new(), Some("left".into()))
            .await
            .unwrap();
        let right_profile = right
            .owner()
            .create(node::OperationId::new(), Some("right".into()))
            .await
            .unwrap();
        assert_ne!(left_profile.record.id, right_profile.record.id);
    }
}
