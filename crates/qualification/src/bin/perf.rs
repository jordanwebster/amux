use std::io::Read;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result, bail};
use qualification::perf::flood::{self, FloodOptions};
use qualification::perf::{Baselines, DESKTOP_REFERENCE_STATE, Machine, MetricRun, Report};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    All,
    Flood,
}

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == ["--cluster-warmer"] {
        return run_cluster_warmer();
    }
    if cfg!(debug_assertions) {
        bail!("performance qualification must run with the release profile");
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
        Selection::All | Selection::Flood => run_flood()?,
    };
    let report = Report::evaluate(machine, runs, recorded.as_ref(), baseline)?;
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

fn run_flood() -> Result<Vec<MetricRun>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the flood's runtime")?
        .block_on(flood::run(&FloodOptions::full()))
}

/// Keeps one core busy for the whole run, the reference state baselines
/// are recorded in, so a machine that idles its cores between samples
/// measures the same as one that does not.
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

fn qualification_arguments(arguments: &[String]) -> Result<(bool, Selection)> {
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok((false, Selection::All)),
        ["--baseline"] => Ok((true, Selection::All)),
        ["--only", "flood"] => Ok((false, Selection::Flood)),
        ["--baseline", "--only", "flood"] | ["--only", "flood", "--baseline"] => {
            Ok((true, Selection::Flood))
        }
        [argument] => bail!("unknown performance argument {argument:?}"),
        _ => bail!("performance qualification accepts --baseline and --only flood"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn accepts_the_flood_alone_with_or_without_recording() {
        assert_eq!(
            qualification_arguments(&arguments(&["--only", "flood"])).unwrap(),
            (false, Selection::Flood)
        );
        assert_eq!(
            qualification_arguments(&arguments(&["--only", "flood", "--baseline"])).unwrap(),
            (true, Selection::Flood)
        );
        assert!(qualification_arguments(&arguments(&["--only", "everything"])).is_err());
    }
}
