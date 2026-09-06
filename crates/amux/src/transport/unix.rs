//! Unix socket helpers for tonic local services.

use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream as StdUnixStream;
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
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "refusing to replace non-socket path {}",
                    socket_path.display()
                ),
            ));
        }
        Ok(_) => match StdUnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "socket is already accepting connections: {}",
                        socket_path.display()
                    ),
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                match std::fs::remove_file(socket_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};

    use super::*;

    #[test]
    fn live_listener_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("amux.sock");
        let _existing = StdUnixListener::bind(&path).unwrap();

        let error = bind_unix_listener(&path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        StdUnixStream::connect(path).unwrap();
    }

    #[tokio::test]
    async fn live_listener_replaces_a_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("amux.sock");
        drop(StdUnixListener::bind(&path).unwrap());

        let listener = bind_unix_listener(&path).unwrap();

        assert!(path.exists());
        drop(listener);
    }

    #[test]
    fn live_listener_refuses_to_replace_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("amux.sock");
        std::fs::write(&path, "occupied").unwrap();

        assert!(bind_unix_listener(&path).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "occupied");
    }

    #[test]
    fn live_listener_refuses_to_follow_or_replace_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let path = temp.path().join("amux.sock");
        std::fs::write(&target, "occupied").unwrap();
        symlink(&target, &path).unwrap();

        assert!(bind_unix_listener(&path).is_err());
        assert_eq!(std::fs::read_link(path).unwrap(), target);
    }

    #[tokio::test]
    async fn live_listener_socket_has_owner_only_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("amux.sock");

        let listener = bind_unix_listener(&path).unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
    }
}
