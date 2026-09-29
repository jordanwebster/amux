//! `tui-lab watch`: run the lab and relaunch it on every change. Sources
//! and scenarios are polled; a Rust change rebuilds in the background while
//! the old lab stays on screen, then the lab is asked to save its place and
//! the new build reopens it there. A scenario change relaunches without a
//! build. A failed build leaves the old lab running and tells it so.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};

use crate::{RELAUNCH_STATUS, place};

/// Polling interval for source changes.
const POLL: Duration = Duration::from_millis(300);
/// A rebuild that takes longer than this is a hang, not a slow build.
const BUILD_BOUND: Duration = Duration::from_secs(900);
/// How long a lab has to save its place and exit when asked.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// The crates whose sources change what the lab draws.
const WATCHED: &[&str] = &[
    "crates/tui",
    "crates/tui-lab",
    "crates/ui-view",
    "crates/ui-state",
    "crates/ui-runtime",
    "crates/model",
    "crates/client",
    "crates/shot",
    "crates/attachments",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .components()
        .collect()
}

#[derive(Default, PartialEq)]
struct Stamp {
    code: Vec<(PathBuf, SystemTime)>,
    scenarios: Vec<(PathBuf, SystemTime)>,
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, SystemTime)>, keep: &dyn Fn(&Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, keep);
        } else if keep(&path)
            && let Ok(modified) = entry.metadata().and_then(|m| m.modified())
        {
            out.push((path, modified));
        }
    }
}

fn stamp(root: &Path) -> Stamp {
    let mut stamp = Stamp::default();
    for krate in WATCHED {
        let krate = root.join(krate);
        walk(&krate.join("src"), &mut stamp.code, &|p| {
            p.extension().is_some_and(|e| e == "rs")
        });
        walk(&krate, &mut stamp.code, &|p| {
            p.file_name().is_some_and(|n| n == "Cargo.toml")
        });
    }
    walk(
        &root.join("crates/tui-lab/scenarios"),
        &mut stamp.scenarios,
        &|p| p.extension().is_some_and(|e| e == "yaml"),
    );
    stamp.code.sort();
    stamp.scenarios.sort();
    stamp
}

fn build(root: &Path) -> Result<bool> {
    std::fs::create_dir_all(place::dir())?;
    let log = std::fs::File::create(place::build_log())?;
    let mut child = Command::new("cargo")
        .args(["build", "--locked", "-p", "tui-lab"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("starting cargo")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if started.elapsed() > BUILD_BOUND {
            let _ = child.kill();
            bail!("the rebuild ran past {}s", BUILD_BOUND.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn signal(child: &Child, signal: i32) {
    // SAFETY: kill with a pid this process spawned and has not reaped.
    unsafe {
        libc::kill(child.id() as i32, signal);
    }
}

fn launch(exe: &Path, scenario: Option<&str>, resume: bool) -> Result<Child> {
    let mut command = Command::new(exe);
    command.arg("run");
    if resume {
        command.arg("--resume");
    }
    if let Some(scenario) = scenario {
        command.arg(scenario);
    }
    Ok(command.spawn()?)
}

/// Waits for the lab to leave; its exit status says whether to relaunch.
fn wait_exit(child: &mut Child) -> Result<Option<i32>> {
    let asked = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.code().unwrap_or(0)));
        }
        if asked.elapsed() > EXIT_GRACE {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn watch(scenario: Option<String>) -> Result<()> {
    let root = root();
    let exe = std::env::current_exe()?;
    let mut child = launch(&exe, scenario.as_deref(), scenario.is_none())?;
    let mut seen = stamp(&root);
    loop {
        std::thread::sleep(POLL);
        if let Some(status) = child.try_wait()? {
            match status.code() {
                Some(RELAUNCH_STATUS) => {
                    child = launch(&exe, None, true)?;
                    continue;
                }
                _ => return Ok(()),
            }
        }
        let now = stamp(&root);
        if now == seen {
            continue;
        }
        let rebuild = now.code != seen.code;
        seen = now;
        if rebuild {
            // Edits often land as a burst of saves; let them finish.
            std::thread::sleep(Duration::from_millis(150));
            seen = stamp(&root);
            match build(&root) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    signal(&child, libc::SIGUSR2);
                    continue;
                }
            }
        }
        signal(&child, libc::SIGUSR1);
        match wait_exit(&mut child)? {
            Some(0) => return Ok(()),
            _ => child = launch(&exe, None, true)?,
        }
    }
}
