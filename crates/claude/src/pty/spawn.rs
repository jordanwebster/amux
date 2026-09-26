//! Starting Claude Code under a PTY.

use std::ffi::OsString;
use std::path::Path;

use crate::hooks::HOOK_SOCKET_ENV;
use crate::launch::{Launch, pty_spawn_args};

/// Start Claude Code in a PTY of `size`, with the launch's arguments and
/// environment scrub, and its hooks forwarded to `hook_socket`.
pub fn spawn(
    launch: &Launch,
    size: pty_host::PtySize,
    hook_socket: &Path,
) -> Result<pty_host::PtyProcess, pty_host::PtyError> {
    pty_host::spawn(pty_host::PtySpawn {
        command: launch.binary.clone(),
        args: pty_spawn_args(launch),
        cwd: launch.cwd.clone(),
        env: vec![(
            OsString::from(HOOK_SOCKET_ENV),
            hook_socket.as_os_str().to_owned(),
        )],
        env_remove: launch.env_scrub.iter().map(OsString::from).collect(),
        size,
    })
}
