use anyhow::{Result, bail};
use testnet::perf::{Baselines, Machine, Report, cold_child, run_fast, run_soak, soak_child};

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().map(String::as_str) == Some("--cold-child") {
        if arguments.len() != 3 {
            bail!("--cold-child requires STORE_PATH AGENT_COUNT");
        }
        return cold_child(std::path::Path::new(&arguments[1]), arguments[2].parse()?);
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
        if arguments.len() != 1 {
            bail!("memory soak accepts no additional arguments");
        }
        return run_soak(Machine::detect()?);
    }
    let baseline = match arguments.first().map(String::as_str) {
        None => false,
        Some("--baseline") => true,
        Some(argument) => bail!("unknown performance argument {argument:?}"),
    };
    if arguments.len() > 1 {
        bail!("performance qualification accepts only --baseline");
    }

    let machine = Machine::detect()?;
    let path = machine.baseline_path();
    let recorded = Baselines::read(&path, &machine)?;
    let report = Report::evaluate(machine, run_fast()?, recorded.as_ref(), baseline)?;
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
