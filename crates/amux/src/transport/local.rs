use std::path::Path;

#[cfg(unix)]
pub(crate) use super::unix::{
    UnixMessageReader as LocalMessageReader, UnixMessageWriter as LocalMessageWriter,
    UnixTransport as LocalTransport,
};

#[cfg(windows)]
pub(crate) type LocalTransport = super::named_pipe::NamedPipeClientTransport;

#[cfg(windows)]
pub(crate) type LocalMessageReader =
    super::named_pipe::NamedPipeMessageReader<tokio::net::windows::named_pipe::NamedPipeClient>;

#[cfg(windows)]
pub(crate) type LocalMessageWriter =
    super::named_pipe::NamedPipeMessageWriter<tokio::net::windows::named_pipe::NamedPipeClient>;

/// Platform-abstracted local IPC listener.
pub(crate) struct LocalListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    pipe_name: String,
}

impl LocalListener {
    pub(crate) fn bind(socket_path: &Path) -> Result<Self, std::io::Error> {
        #[cfg(unix)]
        {
            if let Some(parent) = socket_path.parent()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            let _ = std::fs::remove_file(socket_path);
            let listener = tokio::net::UnixListener::bind(socket_path)?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(Self { inner: listener })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                pipe_name: socket_path.to_string_lossy().into_owned(),
            })
        }
    }

    pub(crate) async fn accept(
        &self,
    ) -> std::io::Result<impl crate::transport::TransportSplit + use<>> {
        #[cfg(unix)]
        {
            let (stream, _) = self.inner.accept().await?;
            Ok(super::unix::UnixTransport::new(stream))
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let server = ServerOptions::new().create(&self.pipe_name)?;
            server.connect().await?;
            Ok(super::named_pipe::NamedPipeTransport::new(server))
        }
    }
}
