//! Provider-neutral pseudo-terminal process hosting.
//!
//! A spawned process owns one output stream and runs in its own process group.
//! Cloning [`PtyHandle`] shares input, resize, and process identity; callers
//! that need output fan-out own that policy above this crate.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
pub use portable_pty::ExitStatus;
use portable_pty::{CommandBuilder, MasterPty, native_pty_system};
use tokio::sync::{mpsc, watch};

const IO_CHANNEL_CAPACITY: usize = 256;

/// What to run and where.
#[derive(Clone, Debug)]
pub struct PtySpawn {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
    pub size: PtySize,
}

/// Terminal geometry in character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// A PTY spawn or I/O failure.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to spawn PTY process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("PTY process exited: {0:?}")]
    Exited(ExitStatus),
    #[error("PTY I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

/// A running child and its independently usable exit monitor.
pub struct PtyProcess {
    pub handle: PtyHandle,
    pub exit: ExitMonitor,
}

struct Shared {
    input_tx: mpsc::Sender<Bytes>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    current_size: Mutex<PtySize>,
    output: Mutex<Option<mpsc::Receiver<Bytes>>>,
    exit_rx: watch::Receiver<Option<ExitStatus>>,
    pid: u32,
    #[cfg(not(unix))]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

/// Cheaply cloned access to one PTY process.
#[derive(Clone)]
pub struct PtyHandle {
    shared: Arc<Shared>,
}

impl PtyHandle {
    /// Queue all bytes for writing to the PTY.
    pub async fn write(&self, bytes: &[u8]) -> Result<(), PtyError> {
        if let Some(status) = self.shared.exit_rx.borrow().clone() {
            return Err(PtyError::Exited(status));
        }
        self.shared
            .input_tx
            .send(Bytes::copy_from_slice(bytes))
            .await
            .map_err(|_| {
                self.shared
                    .exit_rx
                    .borrow()
                    .clone()
                    .map(PtyError::Exited)
                    .unwrap_or_else(|| {
                        PtyError::Io(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "PTY writer is closed",
                        ))
                    })
            })
    }

    /// Resize the terminal and notify the foreground process group.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        let mut current = self
            .shared
            .current_size
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *current == size {
            return Ok(());
        }
        let master = self
            .shared
            .master
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(master) = master.as_ref() else {
            return self.exited_error();
        };
        master
            .resize(portable_size(size))
            .map_err(anyhow_to_io)
            .map_err(PtyError::Io)?;
        *current = size;
        Ok(())
    }

    /// Take the process's single owned output stream.
    ///
    /// A second call returns an already-closed receiver.
    pub fn output(&self) -> mpsc::Receiver<Bytes> {
        self.shared
            .output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .unwrap_or_else(|| {
                let (_tx, rx) = mpsc::channel(1);
                rx
            })
    }

    /// Return the child process id, which is also its process-group id on Unix.
    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// Signal the entire hosted process group without requiring an async runtime.
    pub fn signal_process_group(&self, signal: ProcessGroupSignal) -> Result<(), PtyError> {
        if self.shared.exit_rx.borrow().is_some() {
            return Ok(());
        }

        #[cfg(unix)]
        {
            let signal = match signal {
                ProcessGroupSignal::Terminate => libc::SIGTERM,
                ProcessGroupSignal::Kill => libc::SIGKILL,
            };
            signal_group(self.pid(), signal).map_err(PtyError::Io)
        }

        #[cfg(not(unix))]
        {
            let _ = signal;
            self.shared
                .killer
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .kill()
                .map_err(anyhow_to_io)
                .map_err(PtyError::Io)
        }
    }

    fn exited_error(&self) -> Result<(), PtyError> {
        Err(self
            .shared
            .exit_rx
            .borrow()
            .clone()
            .map(PtyError::Exited)
            .unwrap_or_else(|| {
                PtyError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY master is closed",
                ))
            }))
    }
}

/// Observes one child exit without owning or killing it.
#[derive(Clone)]
pub struct ExitMonitor {
    rx: watch::Receiver<Option<ExitStatus>>,
}

impl ExitMonitor {
    /// Wait until the child exits. Repeated calls return the same status.
    pub async fn wait(&mut self) -> ExitStatus {
        loop {
            if let Some(status) = self.rx.borrow().clone() {
                return status;
            }
            if self.rx.changed().await.is_err() {
                return ExitStatus::with_signal("exit monitor closed");
            }
        }
    }

    /// Return the exit status without waiting, when available.
    pub fn status(&self) -> Option<ExitStatus> {
        self.rx.borrow().clone()
    }
}

/// Process-group termination policy.
#[derive(Clone, Copy, Debug)]
pub enum Terminate {
    Graceful { grace: Duration },
    Kill,
}

/// A signal delivered synchronously to the hosted process group.
#[derive(Clone, Copy, Debug)]
pub enum ProcessGroupSignal {
    Terminate,
    Kill,
}

/// Spawn a process on a new PTY in its own process group.
pub fn spawn(spec: PtySpawn) -> Result<PtyProcess, PtyError> {
    let pair = native_pty_system()
        .openpty(portable_size(spec.size))
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;

    let mut command = CommandBuilder::new(&spec.command);
    for arg in &spec.args {
        command.arg(arg);
    }
    command.cwd(&spec.cwd);
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let pid = child.process_id().ok_or_else(|| {
        PtyError::Spawn(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "PTY child has no process id",
        ))
    })?;
    #[cfg(not(unix))]
    let killer = child.clone_killer();
    drop(pair.slave);

    let master = pair.master;
    let mut reader = master
        .try_clone_reader()
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let mut writer = master
        .take_writer()
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(IO_CHANNEL_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel::<Bytes>(IO_CHANNEL_CAPACITY);
    let (exit_tx, exit_rx) = watch::channel(None);

    tokio::task::spawn_blocking(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if output_tx
                        .blocking_send(Bytes::copy_from_slice(&buffer[..read]))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    let shared = Arc::new(Shared {
        input_tx,
        master: Mutex::new(Some(master)),
        current_size: Mutex::new(spec.size),
        output: Mutex::new(Some(output_rx)),
        exit_rx: exit_rx.clone(),
        pid,
        #[cfg(not(unix))]
        killer: Mutex::new(killer),
    });
    let wait_shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let status = child
            .wait()
            .unwrap_or_else(|error| ExitStatus::with_signal(&format!("wait failed: {error}")));
        wait_shared
            .master
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        exit_tx.send_replace(Some(status));
    });

    Ok(PtyProcess {
        handle: PtyHandle { shared },
        exit: ExitMonitor { rx: exit_rx },
    })
}

/// End the entire process group and return its exit status.
pub async fn terminate(process: &PtyProcess, policy: Terminate) -> ExitStatus {
    if let Some(status) = process.exit.status() {
        return status;
    }

    #[cfg(unix)]
    {
        match policy {
            Terminate::Kill => {
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Kill);
            }
            Terminate::Graceful { grace } => {
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Terminate);
                let mut monitor = process.exit.clone();
                if let Ok(status) = tokio::time::timeout(grace, monitor.wait()).await {
                    return status;
                }
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Kill);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = policy;
        let _ = process
            .handle
            .shared
            .killer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .kill();
    }

    let mut monitor = process.exit.clone();
    monitor.wait().await
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let pid = i32::try_from(pid).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("PTY process id {pid} does not fit in a signed process id"),
        )
    })?;
    // portable-pty creates a session whose leader is the spawned child, so a
    // negative pid addresses the child and every provider shim it launched.
    let result = unsafe { libc::kill(-pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error)
}

fn portable_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn anyhow_to_io(error: anyhow::Error) -> std::io::Error {
    match error.downcast::<std::io::Error>() {
        Ok(error) => error,
        Err(error) => std::io::Error::other(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(script: &str) -> PtySpawn {
        PtySpawn {
            command: PathBuf::from("sh"),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            env_remove: Vec::new(),
            size: PtySize::default(),
        }
    }

    async fn read_until(output: &mut mpsc::Receiver<Bytes>, needle: &[u8]) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut seen = Vec::new();
            while !seen.windows(needle.len()).any(|window| window == needle) {
                let bytes = output.recv().await.expect("PTY output ended early");
                seen.extend_from_slice(&bytes);
            }
            seen
        })
        .await
        .expect("timed out waiting for PTY output")
    }

    #[tokio::test]
    async fn spawns_and_exposes_output_and_pid() {
        let mut process = spawn(shell("printf spawned")).unwrap();
        assert!(process.handle.pid() > 0);
        let mut output = process.handle.output();
        let seen = read_until(&mut output, b"spawned").await;
        assert!(seen.windows(7).any(|window| window == b"spawned"));
        assert!(process.exit.wait().await.success());
    }

    #[tokio::test]
    async fn writes_bytes_to_an_echoing_child() {
        let process = spawn(shell("stty -echo; printf ready; cat")).unwrap();
        let mut output = process.handle.output();
        read_until(&mut output, b"ready").await;
        process.handle.write(b"echo me\n").await.unwrap();
        let seen = read_until(&mut output, b"echo me").await;
        assert!(seen.windows(7).any(|window| window == b"echo me"));
        terminate(&process, Terminate::Kill).await;
        assert!(process.exit.status().is_some());
    }

    #[tokio::test]
    async fn signals_the_process_group_without_a_runtime() {
        let mut process = spawn(shell("sleep 30")).unwrap();
        let handle = process.handle.clone();
        std::thread::spawn(move || handle.signal_process_group(ProcessGroupSignal::Terminate))
            .join()
            .unwrap()
            .unwrap();

        let status = tokio::time::timeout(Duration::from_secs(3), process.exit.wait())
            .await
            .expect("process group did not terminate after synchronous signal");
        assert!(!status.success());
    }

    #[tokio::test]
    async fn applies_environment_additions_and_removals() {
        let mut spec = shell("printf '%s/%s' \"${PTY_HOST_ADDED-unset}\" \"${HOME-unset}\"");
        spec.env.push(("PTY_HOST_ADDED".into(), "added".into()));
        spec.env_remove.push("HOME".into());
        let mut process = spawn(spec).unwrap();
        let mut output = process.handle.output();
        let seen = read_until(&mut output, b"added/unset").await;
        assert!(seen.windows(11).any(|window| window == b"added/unset"));
        assert!(process.exit.wait().await.success());
    }

    #[tokio::test]
    async fn resizes_the_terminal() {
        let mut process = spawn(shell("read line; stty size")).unwrap();
        let mut output = process.handle.output();
        process
            .handle
            .resize(PtySize { rows: 41, cols: 97 })
            .unwrap();
        process.handle.write(b"measure\n").await.unwrap();
        let seen = read_until(&mut output, b"41 97").await;
        assert!(seen.windows(5).any(|window| window == b"41 97"));
        assert!(process.exit.wait().await.success());
    }

    #[tokio::test]
    async fn reports_exit_status_once_and_after_wait() {
        let mut process = spawn(shell("exit 7")).unwrap();
        let status = process.exit.wait().await;
        assert_eq!(status.exit_code(), 7);
        assert_eq!(process.exit.status().unwrap().exit_code(), 7);
        assert_eq!(process.exit.wait().await.exit_code(), 7);
    }

    #[tokio::test]
    async fn termination_kills_the_whole_process_group() {
        let process = spawn(shell("trap '' TERM; sleep 30 & echo $!; wait")).unwrap();
        let mut output = process.handle.output();
        let seen = read_until(&mut output, b"\r\n").await;
        let text = String::from_utf8_lossy(&seen);
        let child_pid: i32 = text
            .split(|character: char| !character.is_ascii_digit())
            .find(|part| !part.is_empty())
            .unwrap()
            .parse()
            .unwrap();

        let status = tokio::time::timeout(
            Duration::from_secs(3),
            terminate(
                &process,
                Terminate::Graceful {
                    grace: Duration::from_millis(50),
                },
            ),
        )
        .await
        .expect("process group did not terminate");
        assert!(!status.success());

        let child_gone = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let result = unsafe { libc::kill(child_pid, 0) };
                if result == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(child_gone.is_ok(), "descendant survived group termination");
    }
}
