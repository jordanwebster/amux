use std::io::Read;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result, bail};
use testnet::perf::{
    Baselines, DESKTOP_REFERENCE_STATE, Machine, Report, run_cold_start, run_fast, run_soak,
    run_summarizer, soak_child,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    All,
    ColdStart,
    Summarizer,
}

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == ["--cluster-warmer"] {
        return run_cluster_warmer();
    }
    if arguments.first().map(String::as_str) == Some("--soak-child") {
        if arguments.len() != 3 {
            bail!("--soak-child requires KIND CONTROL_DIRECTORY");
        }
        return soak_child(&arguments[1], std::path::Path::new(&arguments[2]));
    }
    if cfg!(debug_assertions) {
        bail!("performance qualification must run with the release profile");
    }
    if arguments.first().map(String::as_str) == Some("soak") {
        let recording = soak_arguments(&arguments)?;
        return run_soak(Machine::detect()?, recording);
    }
    let mut warmer = ClusterWarmer::start()?;
    let result = run_qualification(&arguments);
    let stop_result = warmer.stop();
    result?;
    stop_result
}

fn run_qualification(arguments: &[String]) -> Result<()> {
    let (baseline, selection) = qualification_arguments(arguments)?;

    let machine = Machine::detect()?;
    let path = machine.baseline_path();
    let recorded = if baseline {
        None
    } else {
        Baselines::read(&path, &machine, Some(DESKTOP_REFERENCE_STATE))?
    };
    let runs = match selection {
        Selection::All => run_fast()?,
        Selection::ColdStart => run_cold_start()?,
        Selection::Summarizer => run_summarizer()?,
    };
    let metric_names = runs.iter().map(|run| run.metric.name).collect::<Vec<_>>();
    let projected = match (&recorded, selection) {
        (Some(recorded), Selection::ColdStart | Selection::Summarizer) => {
            Some(recorded.project(&metric_names)?)
        }
        _ => None,
    };
    let recorded = match selection {
        Selection::All => recorded.as_ref(),
        Selection::ColdStart | Selection::Summarizer => projected.as_ref(),
    };
    let report = Report::evaluate(machine, runs, recorded, baseline)?;
    report.print();
    if baseline {
        report.write_baseline(&path)?;
        println!("baseline: wrote {}", path.display());
    }
    if !report.passed() {
        bail!("one or more performance metrics missed their budget or drift limit");
    }
    Ok(())
}

struct ClusterWarmer {
    child: Option<Child>,
    pipe: Option<ChildStdin>,
}

impl ClusterWarmer {
    fn start() -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--cluster-warmer")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start performance cluster warmer")?;
        let pipe = child
            .stdin
            .take()
            .context("cluster warmer has no parent pipe")?;
        Ok(Self {
            child: Some(child),
            pipe: Some(pipe),
        })
    }

    fn stop(&mut self) -> Result<()> {
        drop(self.pipe.take());
        let status = self
            .child
            .take()
            .context("cluster warmer was already reaped")?
            .wait()
            .context("reap performance cluster warmer")?;
        if !status.success() {
            bail!("performance cluster warmer exited with {status}");
        }
        Ok(())
    }
}

impl Drop for ClusterWarmer {
    fn drop(&mut self) {
        drop(self.pipe.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

fn run_cluster_warmer() -> Result<()> {
    let pipe_monitor = std::thread::spawn(|| {
        let mut stdin = std::io::stdin().lock();
        let mut byte = [0_u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    while !pipe_monitor.is_finished() {
        std::hint::spin_loop();
    }
    pipe_monitor
        .join()
        .map_err(|_| anyhow::anyhow!("cluster warmer pipe monitor panicked"))
}

fn soak_arguments(arguments: &[String]) -> Result<bool> {
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    match arguments.as_slice() {
        ["soak"] => Ok(false),
        ["soak", "--baseline"] => Ok(true),
        _ => bail!("memory soak accepts exactly `soak` or `soak --baseline`"),
    }
}

fn qualification_arguments(arguments: &[String]) -> Result<(bool, Selection)> {
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok((false, Selection::All)),
        ["--baseline"] => Ok((true, Selection::All)),
        ["--only", "cold-start"] => Ok((false, Selection::ColdStart)),
        ["--only", "summarizer"] => Ok((false, Selection::Summarizer)),
        ["--baseline", "--only", "summarizer"] | ["--only", "summarizer", "--baseline"] => {
            bail!("a summarizer-only run cannot replace the complete performance baseline")
        }
        [argument] => bail!("unknown performance argument {argument:?}"),
        _ => bail!(
            "performance qualification accepts --baseline, --only cold-start, or --only summarizer"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn accepts_summarizer_only_without_baseline_recording() {
        assert_eq!(
            qualification_arguments(&arguments(&["--only", "summarizer"])).unwrap(),
            (false, Selection::Summarizer)
        );
        assert!(
            qualification_arguments(&arguments(&["--only", "summarizer", "--baseline"])).is_err()
        );
    }

    #[test]
    fn accepts_cold_start_only_without_baseline_recording() {
        assert_eq!(
            qualification_arguments(&arguments(&["--only", "cold-start"])).unwrap(),
            (false, Selection::ColdStart)
        );
    }

    #[test]
    fn soak_accepts_only_plain_and_baseline_modes() {
        assert!(!soak_arguments(&arguments(&["soak"])).unwrap());
        assert!(soak_arguments(&arguments(&["soak", "--baseline"])).unwrap());
        assert!(soak_arguments(&arguments(&["soak", "extra"])).is_err());
        assert!(soak_arguments(&arguments(&["soak", "--baseline", "extra"])).is_err());
    }
}
