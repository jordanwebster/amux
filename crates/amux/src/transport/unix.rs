//! Unix socket helpers for tonic local services.

use std::io;
use std::path::Path;

use futures_util::{Stream, stream};
use tokio::net::{UnixListener, UnixStream};

use super::GrpcIo;

pub(crate) type UnixClientTransport = GrpcIo<UnixStream>;

pub(crate) fn bind_unix_listener(socket_path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = socket_path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

pub(crate) fn unix_incoming(
    listener: UnixListener,
) -> impl Stream<Item = io::Result<UnixClientTransport>> + Send + 'static {
    stream::unfold(listener, |listener| async move {
        let item = match listener.accept().await {
            Ok((stream, _addr)) => Ok(UnixClientTransport::new(stream)),
            Err(error) => Err(error),
        };
        Some((item, listener))
    })
}
