//! `tui-set`: a declared world for working on the terminal client, served
//! by real daemons on the fake providers.
//!
//! `tui-set <name>` serves the set from `journeys/sets/<name>.json`, plays
//! its timeline, and runs the real `amux` client on the set's client host in
//! this terminal; beats after `launch` play while it runs. The door's
//! address is in `target/tui-set/<name>/door.json`, so another terminal can
//! change the world while you look (`{"Sever": …}`, `{"Send": …}`).
//!
//! `tui-set <name> --frames KEYS` draws the client headlessly instead: the
//! same keys syntax as `wait:MS`, `C-x`, `hover:X,Y`, `paste:TEXT`, and one
//! text and PNG frame per `--size`.

mod frames;
mod served;
mod set;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, ValueEnum};
use tui::{ColorMode, ColorPreference, TerminalColors, Theme};

use crate::served::Served;
use crate::set::Set;

#[derive(Parser)]
#[command(
    name = "tui-set",
    about = "Serve a declared world and run the terminal client on it"
)]
struct Cli {
    /// The set, by its file name under journeys/sets; `list` lists them.
    name: String,
    /// Draw frames headlessly after these keys instead of running the
    /// client in this terminal.
    #[arg(long)]
    frames: Option<String>,
    /// WIDTHxHEIGHT of each frame, repeatable.
    #[arg(long = "size", default_values = ["120x36"])]
    sizes: Vec<String>,
    /// The agent whose chat the frames open on, instead of the set's.
    #[arg(long)]
    open: Option<String>,
    /// Open home rather than the set's chat.
    #[arg(long)]
    home: bool,
    /// Send one request to the door of the set already running under this
    /// name (`--door '{"Sever": {"a": "laptop", "b": "cabin"}}'`), print
    /// its answer and leave.
    #[arg(long)]
    door: Option<String>,
    /// Where frames are written; `target/tui-set/<name>/frames` by default.
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "captured")]
    theme: ThemeArg,
}

#[derive(Clone, Copy, ValueEnum)]
enum ThemeArg {
    Dark,
    Light,
    /// A fixed dark terminal's colours.
    Sample,
    /// The colours this terminal reported the last time `tui-set` ran in
    /// it, so frames drawn without a terminal look like the person's;
    /// `sample` until then.
    Captured,
}

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = root();
    if cli.name == "list" {
        for (name, description) in Set::list(&root)? {
            println!("{name}\n    {description}\n");
        }
        return Ok(());
    }
    if let Some(request) = &cli.door {
        let ready: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                root.join("target/tui-set")
                    .join(&cli.name)
                    .join("door.json"),
            )
            .with_context(|| format!("no set {} is running here", cli.name))?,
        )?;
        let control = ready["control"].as_str().unwrap_or_default().to_owned();
        let answer = served::request_at(&control, &serde_json::from_str(request)?)?;
        println!("{answer}");
        return Ok(());
    }
    let set = Set::load(&root, &cli.name)?;
    let (before, after) = set.beats();
    if cli.frames.is_none() {
        // Asked before anything else reads the terminal, as `amux` does,
        // and kept for frames drawn later without one.
        if let Some(colors) = tui::query_terminal_colors(std::time::Duration::from_millis(250)) {
            save_colors(&root, &colors);
        }
    }
    let served = tokio::task::block_in_place(|| -> Result<Served> {
        let mut served = Served::start(&root, &set)?;
        if let Err(error) = served.play(&set, &before) {
            let _ = served.shutdown();
            return Err(error);
        }
        Ok(served)
    })?;
    let config = served.config(&set.client)?;
    if let Err(error) = served::patch_config(&config, &set.config) {
        tokio::task::block_in_place(|| served.shutdown())?;
        return Err(error);
    }
    let outcome = match &cli.frames {
        Some(keys) => frames(&root, &cli, &set, &served, &config, keys, after).await,
        None => interactive(&root, &set, &served, &config, after).await,
    };
    tokio::task::block_in_place(|| served.shutdown())?;
    outcome
}

/// The real `amux` on the client host, in this terminal, while the beats
/// after `launch` play.
async fn interactive(
    root: &Path,
    set: &Set,
    served: &Served,
    config: &Path,
    after: Vec<set::Beat>,
) -> Result<()> {
    println!("{}: {}", set.name, set.description);
    println!(
        "door: {} (target/tui-set/{}/door.json)",
        served.control, set.name
    );
    let work = served.work_dir(&set.client)?;
    let live = spawn_beats(served, set, after);
    let status = tokio::task::block_in_place(|| {
        Command::new(root.join("target/debug/amux"))
            .arg("--config")
            .arg(config)
            .current_dir(&work)
            .env_remove("AMUX_CONFIG")
            .env_remove("AMUX_LOG")
            .status()
            .context("running amux")
    })?;
    live.stop();
    if !status.success() {
        bail!("amux exited with {status}");
    }
    Ok(())
}

/// Beats playing while the client runs, on a thread of their own against
/// the door; they stop when the client does.
struct Live {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Live {
    fn stop(self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn spawn_beats(served: &Served, set: &Set, beats: Vec<set::Beat>) -> Live {
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live = Live {
        stopped: stopped.clone(),
    };
    let control = served.control.clone();
    let name = set.name.clone();
    let hosts: Vec<(String, Option<String>)> = beats
        .iter()
        .filter_map(|beat| match beat {
            set::Beat::Settle(agent) => Some((agent.clone(), set.host_of(agent))),
            _ => None,
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        for beat in beats {
            if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let played = match &beat {
                set::Beat::Send { agent, text } => served::request_at(
                    &control,
                    &serde_json::json!({ "Send": { "agent": agent, "text": text } }),
                )
                .map(drop),
                set::Beat::WaitMs(ms) => {
                    std::thread::sleep(std::time::Duration::from_millis(*ms));
                    Ok(())
                }
                set::Beat::Gate(gate) => served::request_at(
                    &control,
                    &serde_json::json!({ "OpenGate": { "name": gate } }),
                )
                .map(drop),
                set::Beat::Door(request) => served::request_at(&control, request).map(drop),
                set::Beat::Settle(agent) => {
                    let host = hosts
                        .iter()
                        .find(|(name, _)| name == agent)
                        .and_then(|(_, host)| host.clone());
                    settle_at(&control, agent, host.as_deref())
                }
                set::Beat::Launch => Ok(()),
            };
            if let Err(error) = played {
                if !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                    eprintln!("{name}: beat {beat:?}: {error:#}");
                }
                return;
            }
        }
    });
    live
}

/// A settle while the client runs: the agent's chat at rest.
fn settle_at(control: &str, agent: &str, host: Option<&str>) -> Result<()> {
    let Some(host) = host else {
        bail!("no declared agent {agent}");
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if served::exited(control, host, agent)? {
            return Ok(());
        }
        let chat = served::request_at(
            control,
            &serde_json::json!({ "Chat": { "host": host, "agent": agent } }),
        )?;
        if matches!(chat["phase"].as_str(), Some("IDLE" | "NEEDS_YOU")) {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            bail!("{agent} did not come to rest");
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

async fn frames(
    root: &Path,
    cli: &Cli,
    set: &Set,
    served: &Served,
    config: &Path,
    keys: &str,
    after: Vec<set::Beat>,
) -> Result<()> {
    let open = if cli.home {
        None
    } else {
        cli.open.as_ref().or(set.open.as_ref())
    };
    let open = match open {
        Some(name) => Some(
            served
                .agent_id(name)
                .with_context(|| format!("no declared agent {name}"))?,
        ),
        None => None,
    };
    let local_host = served.host_id(&set.client)?;
    let sizes = cli
        .sizes
        .iter()
        .map(|size| frames::parse_size(size))
        .collect::<Result<Vec<_>>>()?;
    let out = cli.out.clone().unwrap_or_else(|| served.out.join("frames"));
    let live = spawn_beats(served, set, after);
    let written = frames::render(frames::Request {
        config: config.to_owned(),
        working_dir: served.work_dir(&set.client)?,
        open,
        local_host,
        keys: frames::parse_keys(keys)?,
        control: served.control.clone(),
        sizes,
        theme: theme(root, cli.theme),
        out: out.clone(),
        stem: set.name.clone(),
    })
    .await;
    live.stop();
    for stem in written? {
        println!("{}", out.join(stem).display());
    }
    Ok(())
}

fn colors_path(root: &Path) -> PathBuf {
    root.join("target/tui-set/terminal-colors.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Colors {
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
    ansi: [(u8, u8, u8); 16],
}

fn save_colors(root: &Path, colors: &TerminalColors) {
    let saved = Colors {
        background: colors.background,
        foreground: colors.foreground,
        ansi: colors.ansi,
    };
    if let Ok(json) = serde_json::to_vec_pretty(&saved) {
        let path = colors_path(root);
        let _ = std::fs::create_dir_all(path.parent().expect("a parent"));
        let _ = std::fs::write(path, json);
    }
}

fn load_colors(root: &Path) -> Option<TerminalColors> {
    let saved: Colors = serde_json::from_slice(&std::fs::read(colors_path(root)).ok()?).ok()?;
    Some(TerminalColors {
        background: saved.background,
        foreground: saved.foreground,
        ansi: saved.ansi,
    })
}

/// A dark terminal's answer to the colour queries: a near-black ground,
/// grey text and the usual sixteen colours.
const SAMPLE: TerminalColors = TerminalColors {
    background: (21, 21, 21),
    foreground: (208, 208, 208),
    ansi: [
        (21, 21, 21),
        (204, 102, 102),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (208, 208, 208),
        (92, 99, 112),
        (224, 108, 117),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (255, 255, 255),
    ],
};

fn theme(root: &Path, arg: ThemeArg) -> Theme {
    let mode = tui::detect_color_mode(ColorPreference::TrueColor, None, None, false);
    let mode: ColorMode = mode;
    match arg {
        ThemeArg::Dark => Theme::dark(mode),
        ThemeArg::Light => Theme::light(mode),
        ThemeArg::Sample => Theme::from_terminal(SAMPLE, mode),
        ThemeArg::Captured => Theme::from_terminal(load_colors(root).unwrap_or(SAMPLE), mode),
    }
}
