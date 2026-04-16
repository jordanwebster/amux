use std::env;
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use tracing::{info, warn};

const ASSERTION_REASON: &str = "amux server is running";
const APP_ID: &str = "amux";
const BLOCKER_SLEEP_SECONDS: &str = "2147483647";

#[derive(Debug, Default)]
pub(crate) struct SleepInhibitor {
    handle: Option<LinuxHandle>,
    missing_backend_logged: bool,
}

#[derive(Debug)]
enum LinuxHandle {
    Child { backend: LinuxBackend, child: Child },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum LinuxBackend {
    SystemdInhibit,
    GnomeSessionInhibit,
}

impl LinuxBackend {
    fn candidates() -> [Self; 2] {
        [Self::SystemdInhibit, Self::GnomeSessionInhibit]
    }

    fn command(self) -> &'static str {
        match self {
            Self::SystemdInhibit => "systemd-inhibit",
            Self::GnomeSessionInhibit => "gnome-session-inhibit",
        }
    }

    fn available(self) -> bool {
        command_in_path(self.command(), env::var_os("PATH").as_deref())
    }

    fn acquire(self) -> Result<LinuxHandle, std::io::Error> {
        let parent_pid = unsafe { libc::getpid() };
        let mut command = match self {
            Self::SystemdInhibit => {
                let mut command = Command::new(self.command());
                command.args([
                    "--what=idle",
                    "--mode=block",
                    "--who",
                    APP_ID,
                    "--why",
                    ASSERTION_REASON,
                    "--",
                    "sleep",
                    BLOCKER_SLEEP_SECONDS,
                ]);
                command
            }
            Self::GnomeSessionInhibit => {
                let mut command = Command::new(self.command());
                command.args([
                    "--inhibit",
                    "idle",
                    "--reason",
                    ASSERTION_REASON,
                    "sleep",
                    BLOCKER_SLEEP_SECONDS,
                ]);
                command
            }
        };

        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    libc::raise(libc::SIGTERM);
                }
                Ok(())
            });
        }

        Ok(LinuxHandle::Child {
            backend: self,
            child: command.spawn()?,
        })
    }
}

impl SleepInhibitor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn acquire(&mut self) {
        if self.handle.is_some() {
            return;
        }

        for backend in LinuxBackend::candidates() {
            if !backend.available() {
                continue;
            }

            match backend.acquire() {
                Ok(mut handle) => match handle.probe() {
                    Ok(()) => {
                        info!(?backend, "idle sleep prevention active");
                        self.handle = Some(handle);
                        self.missing_backend_logged = false;
                        return;
                    }
                    Err(error) => {
                        warn!(
                            ?backend,
                            reason = %error,
                            "linux idle sleep inhibitor backend failed during startup"
                        );
                    }
                },
                Err(error) => {
                    warn!(
                        ?backend,
                        reason = %error,
                        "failed to start linux idle sleep inhibitor backend"
                    );
                }
            }
        }

        if !self.missing_backend_logged {
            warn!("no linux idle sleep inhibitor backend is available");
            self.missing_backend_logged = true;
        }
    }
}

pub(crate) fn supported() -> bool {
    LinuxBackend::candidates()
        .into_iter()
        .any(LinuxBackend::available)
}

impl LinuxHandle {
    fn probe(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Child { backend, child } => match child.try_wait() {
                Ok(None) => Ok(()),
                Ok(Some(status)) => Err(std::io::Error::other(format!(
                    "{backend:?} exited immediately with status {status}"
                ))),
                Err(error) => {
                    self.stop();
                    Err(error)
                }
            },
        }
    }

    fn stop(&mut self) {
        match self {
            Self::Child { backend, child } => {
                if let Err(error) = child.kill()
                    && !child_exited(&error)
                {
                    warn!(?backend, reason = %error, "failed to stop linux idle sleep inhibitor");
                }
                if let Err(error) = child.wait()
                    && !child_exited(&error)
                {
                    warn!(?backend, reason = %error, "failed to reap linux idle sleep inhibitor");
                }
            }
        }
    }
}

impl Drop for LinuxHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn child_exited(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::InvalidInput)
}

fn command_in_path(command: &str, path: Option<&OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    env::split_paths(path).any(|dir| dir.join(command).is_file())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{LinuxBackend, command_in_path};

    #[test]
    fn command_lookup_reports_false_for_empty_path() {
        assert!(!command_in_path("systemd-inhibit", Some(&OsString::new())));
        assert!(!command_in_path("systemd-inhibit", None));
    }

    #[test]
    fn candidate_order_prefers_systemd_inhibit() {
        let candidates = LinuxBackend::candidates();
        assert_eq!(
            candidates,
            [
                LinuxBackend::SystemdInhibit,
                LinuxBackend::GnomeSessionInhibit
            ]
        );
    }
}
