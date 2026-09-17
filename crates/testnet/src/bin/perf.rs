use anyhow::{Result, bail};
use testnet::perf::{Baselines, Machine, Report, run_fast, run_soak, run_summarizer, soak_child};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    All,
    Summarizer,
}

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
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
        if arguments.len() != 1 {
            bail!("memory soak accepts no additional arguments");
        }
        return run_soak(Machine::detect()?);
    }
    let (baseline, selection) = qualification_arguments(&arguments)?;

    let machine = Machine::detect()?;
    let path = machine.baseline_path();
    let recorded = Baselines::read(&path, &machine)?;
    let runs = match selection {
        Selection::All => run_fast()?,
        Selection::Summarizer => run_summarizer()?,
    };
    let metric_names = runs.iter().map(|run| run.metric.name).collect::<Vec<_>>();
    let projected = match (&recorded, selection) {
        (Some(recorded), Selection::Summarizer) => Some(recorded.project(&metric_names)?),
        _ => None,
    };
    let recorded = match selection {
        Selection::All => recorded.as_ref(),
        Selection::Summarizer => projected.as_ref(),
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

fn qualification_arguments(arguments: &[String]) -> Result<(bool, Selection)> {
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok((false, Selection::All)),
        ["--baseline"] => Ok((true, Selection::All)),
        ["--only", "summarizer"] => Ok((false, Selection::Summarizer)),
        ["--baseline", "--only", "summarizer"] | ["--only", "summarizer", "--baseline"] => {
            bail!("a summarizer-only run cannot replace the complete performance baseline")
        }
        [argument] => bail!("unknown performance argument {argument:?}"),
        _ => bail!("performance qualification accepts --baseline or --only summarizer"),
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
}
