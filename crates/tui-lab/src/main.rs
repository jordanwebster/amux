//! The TUI lab: the terminal client over a scripted fake runtime, for
//! trying designs and flows without a daemon, providers or real agents.
//!
//! `tui-lab run <scenario>` opens it in this terminal, `tui-lab watch`
//! relaunches it on every source change, and `tui-lab render` draws frames
//! headlessly. Scenarios are YAML under `crates/tui-lab/scenarios/`.

mod body;
mod boot;
mod lab;
mod place;
mod render;
mod scenario;
#[cfg(unix)]
mod watch;
mod world;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use tui::{ColorMode, ColorPreference, Theme};

/// The exit status the lab uses to ask `tui-lab watch` for a relaunch.
const RELAUNCH_STATUS: i32 = 75;

#[derive(Parser)]
#[command(
    name = "tui-lab",
    about = "The amux terminal client over scripted dummy data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the lab in this terminal.
    Run {
        /// A scenario name; the saved place's scenario when omitted.
        scenario: Option<String>,
        /// Reopen at the saved place: timeline step, open chat, draft.
        #[arg(long)]
        resume: bool,
        #[arg(long, value_enum, default_value = "terminal")]
        theme: ThemeArg,
    },
    /// Run the lab and relaunch it at the same place on every change.
    Watch { scenario: Option<String> },
    /// List the scenarios.
    List,
    /// Draw frames headlessly to text and PNG.
    Render {
        scenario: String,
        /// Timeline beats applied before drawing.
        #[arg(long, default_value_t = 0)]
        step: usize,
        /// WIDTHxHEIGHT, repeatable.
        #[arg(long = "size", default_values = ["120x40"])]
        sizes: Vec<String>,
        /// Keys pressed before drawing, like `down enter 'hello there' enter`.
        #[arg(long, default_value = "")]
        keys: String,
        #[arg(long, default_value = "notes/tui-lab/frames")]
        out: PathBuf,
        #[arg(long, value_enum, default_value = "dark")]
        theme: ThemeArg,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ThemeArg {
    /// The terminal's own colours when it reports them, else amux dark.
    Terminal,
    Dark,
    Light,
}

fn color_mode() -> ColorMode {
    tui::detect_color_mode(
        ColorPreference::Auto,
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("NO_COLOR").is_some(),
    )
}

fn theme(arg: ThemeArg, mode: ColorMode) -> Theme {
    match arg {
        ThemeArg::Terminal => match tui::query_terminal_colors(Duration::from_millis(250)) {
            Some(colors) => Theme::from_terminal(colors, mode),
            None => Theme::dark(mode),
        },
        ThemeArg::Dark => Theme::dark(mode),
        ThemeArg::Light => Theme::light(mode),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run {
            scenario,
            resume,
            theme: theme_arg,
        } => {
            tui::install_panic_hook();
            let saved = place::Place::load();
            let (name, mut place) = match (scenario, resume) {
                (Some(name), true) => {
                    let place = saved.filter(|p| p.scenario == name);
                    (name, place)
                }
                (Some(name), false) => (name, None),
                (None, _) => match saved {
                    Some(place) => (place.scenario.clone(), Some(place)),
                    None => bail!("no saved place; name a scenario (tui-lab list)"),
                },
            };
            // Asked before the alternate screen, while nothing else reads
            // stdin.
            let theme = theme(theme_arg, color_mode());
            loop {
                let loaded = scenario::load(&name)?;
                match lab::run(&loaded, place.take(), theme).await? {
                    lab::Leave::Quit => return Ok(()),
                    lab::Leave::Relaunch => std::process::exit(RELAUNCH_STATUS),
                    lab::Leave::Reset => continue,
                }
            }
        }
        #[cfg(unix)]
        Command::Watch { scenario } => watch::watch(scenario),
        // The watcher asks the lab to relaunch through Unix signals.
        #[cfg(not(unix))]
        Command::Watch { .. } => bail!("tui-lab watch needs Unix; use tui-lab run"),
        Command::List => {
            for (name, description) in scenario::list()? {
                println!("{name:<16} {description}");
            }
            Ok(())
        }
        Command::Render {
            scenario,
            step,
            sizes,
            keys,
            out,
            theme: theme_arg,
        } => {
            let loaded = scenario::load(&scenario)?;
            let sizes = sizes
                .iter()
                .map(|s| render::parse_size(s))
                .collect::<Result<Vec<_>>>()?;
            let written = render::render(render::Request {
                scenario: &loaded,
                step,
                sizes,
                keys: render::parse_keys(&keys)?,
                out: &out,
                theme: theme(theme_arg, ColorMode::TrueColor),
            })
            .await?;
            for stem in written {
                println!("{}", out.join(stem).display());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn draw(name: &str, keys: &str, sizes: Vec<(u16, u16)>) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let loaded = scenario::load(name).unwrap();
        let stems = render::render(render::Request {
            scenario: &loaded,
            step: 0,
            sizes,
            keys: render::parse_keys(keys).unwrap(),
            out: dir.path(),
            theme: Theme::dark(ColorMode::TrueColor),
        })
        .await
        .unwrap();
        stems
            .iter()
            .map(|stem| {
                assert!(dir.path().join(format!("{stem}.png")).is_file());
                std::fs::read_to_string(dir.path().join(format!("{stem}.txt"))).unwrap()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_scenario_loads_and_draws_at_two_sizes() {
        let scenarios = scenario::list().unwrap();
        assert!(scenarios.len() >= 4, "{scenarios:?}");
        for (name, description) in scenarios {
            assert!(!description.starts_with("(broken"), "{name}: {description}");
            for frame in draw(&name, "", vec![(100, 30), (60, 20)]).await {
                assert!(frame.trim().len() > 20, "{name} drew an empty frame");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_permission_runs_the_call_and_carries_on() {
        let before = draw("asks", "", vec![(110, 30)]).await.remove(0);
        assert!(before.contains("Allow once"), "{before}");
        let after = draw("asks", "enter", vec![(110, 30)]).await.remove(0);
        assert!(after.contains("failed on run 7"), "{after}");
        assert!(!after.contains("Allow once"), "{after}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_agent_answers_its_first_prompt() {
        let frame = draw(
            "first-run",
            "n 2 enter 'fix the flaky test' enter",
            vec![(100, 20)],
        )
        .await
        .remove(0);
        assert!(frame.contains("fix the flaky test"), "{frame}");
        assert!(frame.contains("(lab reply)"), "{frame}");
    }

    #[test]
    fn keys_parse_named_control_and_typed() {
        let keys = render::parse_keys("C-a r 'hi there' enter f2").unwrap();
        let codes: Vec<_> = keys.iter().map(|k| (k.code, k.modifiers)).collect();
        use crossterm::event::{KeyCode, KeyModifiers};
        assert_eq!(codes[0], (KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(codes[1], (KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(codes.len(), 2 + 8 + 2);
        assert_eq!(codes[10].0, KeyCode::Enter);
        assert_eq!(codes[11].0, KeyCode::F(2));
    }
}
