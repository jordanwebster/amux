//! Local sockets named by a path in the agent directory: Unix domain
//! sockets on Unix, named pipes on Windows.
//!
//! A socket's name is its path (`agents/<id>/ctl.sock`); what the operating
//! system binds is derived from it. On Unix that is the path itself unless
//! it is too long for `sockaddr_un`, which agent directories under a deep
//! data directory easily are; then the socket is reached through a short
//! symbolic link to its directory in a per-user runtime directory, so the
//! file still lives at its path. On Windows it is a pipe whose name is a
//! hash of the path. Both ends derive the same address from the same path.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

/// A connected local socket.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

pub type LocalStream = Box<dyn Io>;

/// Listens on the socket named by a path; removes it when dropped.
pub struct LocalListener {
    path: PathBuf,
    inner: imp::Listener,
}

impl LocalListener {
    /// Binds `path`, replacing a socket a dead predecessor left behind. The
    /// caller holds the directory's lock, so nothing live owns it.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        Ok(Self {
            inner: imp::Listener::bind(&path)?,
            path,
        })
    }

    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        self.inner.accept().await
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        imp::unbind(&self.path);
    }
}

/// Connects to the socket named by `path`.
pub async fn connect(path: &Path) -> io::Result<LocalStream> {
    imp::connect(&std::path::absolute(path)?).await
}

/// The path a Unix socket named `path` is bound and dialled at: the path
/// itself, or one through a short link when it is too long.
#[cfg(unix)]
pub fn unix_address(path: &Path) -> io::Result<PathBuf> {
    imp::address(&std::path::absolute(path)?)
}

/// The path to hand a program that binds the socket itself and insists its
/// directory be a real, private one (terminal Claude's messaging socket):
/// the path itself when it fits, else the same name in a real directory,
/// only this user's, in the per-user runtime directory. Nothing but the
/// program's own announcement of it leads there, so it needs no link.
#[cfg(unix)]
pub fn unix_private_address(path: &Path) -> io::Result<PathBuf> {
    imp::private_address(&std::path::absolute(path)?)
}

/// FNV-1a: stable across processes and versions, which is all a derived
/// address needs.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(unix)]
mod imp {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    use tokio::net::{UnixListener, UnixStream};

    use super::{LocalStream, fnv};

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let address = address(path)?;
            match fs::remove_file(&address) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(Self(UnixListener::bind(address)?))
        }

        pub async fn accept(&mut self) -> io::Result<LocalStream> {
            let (stream, _) = self.0.accept().await?;
            Ok(Box::new(stream))
        }
    }

    pub fn unbind(path: &Path) {
        let _ = fs::remove_file(path);
    }

    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        Ok(Box::new(UnixStream::connect(address(path)?).await?))
    }

    /// The path to bind or dial for the socket named `path`.
    pub fn address(path: &Path) -> io::Result<PathBuf> {
        // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
        let limit = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
            .sun_path
            .len();
        if path.as_os_str().as_bytes().len() < limit {
            return Ok(path.to_owned());
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no socket", path.display()),
            ));
        };
        let link = runtime_dir()?.join(format!("{:016x}", fnv(dir.as_os_str().as_bytes())));
        loop {
            match fs::read_link(&link) {
                Ok(target) if target == dir => break,
                Ok(_) => fs::remove_file(&link)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match std::os::unix::fs::symlink(dir, &link) {
                        Ok(()) => break,
                        // Someone else made it first; check what they made.
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        let address = link.join(name);
        if address.as_os_str().as_bytes().len() >= limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no short address fits {}", path.display()),
            ));
        }
        Ok(address)
    }

    pub fn private_address(path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().as_bytes().len() < limit() {
            return Ok(path.to_owned());
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no socket", path.display()),
            ));
        };
        let short = runtime_dir()?.join(format!("{:016x}.d", fnv(dir.as_os_str().as_bytes())));
        private_dir(&short)?;
        let address = short.join(name);
        if address.as_os_str().as_bytes().len() >= limit() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no short address fits {}", path.display()),
            ));
        }
        Ok(address)
    }

    fn limit() -> usize {
        // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
        unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
            .sun_path
            .len()
    }

    /// A directory only this user can write, for the short links.
    fn runtime_dir() -> io::Result<PathBuf> {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("amux-{uid}"));
        private_dir(&dir)?;
        Ok(dir)
    }

    /// Makes `dir` if it is missing, and refuses it unless it is a real
    /// directory this user owns that no one else can use.
    fn private_dir(dir: &Path) -> io::Result<()> {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(dir)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a private directory", dir.display()),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    use super::{LocalStream, fnv};

    /// ERROR_PIPE_BUSY: every instance is taken; another comes shortly.
    const PIPE_BUSY: i32 = 231;

    pub struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let name = pipe_name(path);
            let next = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&name)?;
            Ok(Self { name, next })
        }

        pub async fn accept(&mut self) -> io::Result<LocalStream> {
            self.next.connect().await?;
            let fresh = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            Ok(Box::new(connected))
        }
    }

    pub fn unbind(_path: &Path) {}

    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        let name = pipe_name(path);
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(Box::new(client)),
                Err(error) if error.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn pipe_name(path: &Path) -> String {
        let wide: Vec<u8> = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        format!(r"\\.\pipe\amux-{:016x}", fnv(&wide))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn a_socket_whose_path_is_too_long_still_lives_at_its_path() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let path = deep.join("ctl.sock");
        let mut listener = LocalListener::bind(&path).unwrap();
        assert!(path.exists(), "the socket file is in its own directory");
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            stream.write_all(b"hello").await.unwrap();
            listener
        });
        let mut client = connect(&path).await.unwrap();
        let mut got = [0; 5];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        drop(server.await.unwrap());
        assert!(!path.exists(), "dropping the listener removes the socket");
    }

    /// Terminal Claude binds its messaging socket itself and refuses a
    /// directory reached through a symbolic link.
    #[test]
    fn a_socket_another_program_binds_gets_a_real_private_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let path = deep.join("messaging.sock");
        let address = unix_private_address(&path).unwrap();
        assert_ne!(address, path, "the path is too long to bind");
        assert_eq!(address.file_name(), path.file_name());
        let dir = std::fs::symlink_metadata(address.parent().unwrap()).unwrap();
        assert!(dir.is_dir(), "a real directory, not a link");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(unix_private_address(&path).unwrap(), address, "stable");
        std::os::unix::net::UnixListener::bind(&address).unwrap();
        std::fs::remove_file(&address).unwrap();

        let short = root.path().join("m.sock");
        assert_eq!(unix_private_address(&short).unwrap(), short);
    }
}
