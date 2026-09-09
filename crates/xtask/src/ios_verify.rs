use std::collections::BTreeSet;
use std::error::Error;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

// Ordering matters: build the bridge and app before simulator checks. Destructive
// baseline updates and deliberate-failure probes are separate developer commands.
const RECIPES: &[&str] = &[
    "lint",
    "test",
    "spec",
    "mobile-check",
    "ios-lint",
    "ios-rust",
    "ios-simulator",
    "ios-build",
    "ios-loopback-smoke",
    "ios-unit",
    "ios-door-smoke",
    "ios-goldens",
    "ios-journey",
    "ios-accessibility",
    "ios-perf",
    "ios-scope-audit",
];

const REQUIRED_JOURNEYS: &[&str] = &[
    "home-coldstart",
    "home",
    "conversation",
    "asks",
    "review",
    "writing",
    "claude-sessions",
    "hosts-lifecycle",
    "hosts",
    "accounts",
    "production-startup",
    "reports",
    "accessibility",
];

fn check_journeys(manifest: &str) -> Result<(), Box<dyn Error>> {
    let manifest: serde_json::Value = serde_json::from_str(manifest)?;
    let journeys = manifest["journeys"]
        .as_array()
        .ok_or("no declared journeys")?;
    for required in REQUIRED_JOURNEYS {
        if !journeys.iter().any(|journey| journey["id"] == *required) {
            return Err(format!("iOS verification requires journey {required}").into());
        }
    }
    Ok(())
}

fn recipes(config: &str) -> Result<Vec<&'static str>, Box<dyn Error>> {
    let config: toml::Value = toml::from_str(config)?;
    let tasks = config
        .get("task")
        .and_then(toml::Value::as_table)
        .ok_or("no declared tasks")?;
    for recipe in RECIPES {
        if !tasks.contains_key(*recipe) {
            return Err(format!("iOS verification requires recipe {recipe}").into());
        }
    }
    Ok(RECIPES.to_vec())
}

fn check_completed_journeys(completed: &BTreeSet<String>) -> Result<(), Box<dyn Error>> {
    for required in REQUIRED_JOURNEYS {
        if !completed.contains(*required) {
            return Err(format!("iOS journey {required} did not report a full pass").into());
        }
    }
    Ok(())
}

/// A machine's budget row, as `scripts/ios-perf.py --machine` answers it.
#[derive(serde::Deserialize)]
struct PerfMachine {
    name: String,
    /// Whether this machine's budgets are absolute rather than relative to a
    /// recorded run.
    hard: bool,
    baseline: String,
    baseline_present: bool,
}

// Hard budgets exist independently of a recorded run. Relative budgets need a
// committed baseline; verification must never create its own comparison data.
fn measure_perf(machine: &PerfMachine) -> bool {
    machine.hard || machine.baseline_present
}

fn report_missing_baseline(machine: &PerfMachine) -> Result<(), Box<dyn Error>> {
    let output = std::path::Path::new("target/ios/perf");
    if output.exists() {
        std::fs::remove_dir_all(output)?;
    }
    std::fs::create_dir_all(output)?;
    let report = format!(
        "no baseline for this runner: {} ({}). Performance was not measured.\n",
        machine.name, machine.baseline
    );
    eprint!("{report}");
    std::fs::write(output.join("report.md"), &report)?;
    std::fs::write(
        output.join("verdict.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "status": "not_measured",
            "reason": "no baseline for this runner",
            "machine": machine.name,
            "baseline": machine.baseline,
        }))?,
    )?;
    Ok(())
}

/// Asks the measurement script which machine this is. The script owns the
/// answer; nothing here reads the measurement document.
fn perf_machine() -> Result<PerfMachine, String> {
    let output = Command::new("timeout")
        .args(["120", "python3", "-B", "scripts/ios-perf.py", "--machine"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let selected = recipes(&std::fs::read_to_string(".wt.toml")?)?;
    check_journeys(&std::fs::read_to_string("ios/Journeys/manifest.json")?)?;
    eprintln!("Required iOS journeys: {}", REQUIRED_JOURNEYS.join(", "));
    eprintln!("iOS verification: {}", selected.join(", "));
    for recipe in selected {
        if recipe == "ios-perf" {
            let machine = perf_machine()?;
            if !measure_perf(&machine) {
                report_missing_baseline(&machine)?;
                continue;
            }
        }
        eprintln!("Running wt run {recipe}");
        // Recipes own their individual deadlines. The outer deadline also
        // bounds dependencies without cutting off a longer recipe early.
        let mut child = Command::new("timeout")
            .args(["12600", "wt", "run", recipe])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut completed = BTreeSet::new();
        for line in BufReader::new(child.stdout.take().ok_or("no recipe stdout")?).lines() {
            let line = line?;
            println!("{line}");
            if recipe == "ios-journey"
                && let Some(id) = line.strip_suffix(": passed")
            {
                completed.insert(id.to_owned());
            }
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("wt run {recipe} failed: {status}").into());
        }
        if recipe == "ios-journey" {
            check_completed_journeys(&completed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> String {
        RECIPES
            .iter()
            .map(|name| format!("[task.{name}]\nrun='true'\n"))
            .collect()
    }

    #[test]
    fn ios_verify_requires_every_recipe_without_selecting_update_or_remote_commands() {
        assert_eq!(recipes(&config()).unwrap(), RECIPES);
        for recipe in RECIPES {
            let incomplete = config().replace(&format!("[task.{recipe}]\nrun='true'\n"), "");
            assert!(
                recipes(&incomplete)
                    .unwrap_err()
                    .to_string()
                    .contains(recipe)
            );
        }
        for excluded in [
            "ios-verify",
            "ios-goldens-perturb",
            "ci-gate",
            "ci-observe",
            "qa-cloud-signin",
            "qa-sandbox-purchase",
            "qa-live-journey",
        ] {
            assert!(!RECIPES.contains(&excluded));
        }
    }

    #[test]
    fn ios_verify_runs_accessibility_after_journeys_through_the_wt_entrypoint() {
        let config = include_str!("../../../.wt.toml");
        let selected = recipes(config).unwrap();
        let journey = selected
            .iter()
            .position(|name| *name == "ios-journey")
            .unwrap();
        assert_eq!(selected[journey + 1], "ios-accessibility");
        let config: toml::Value = toml::from_str(config).unwrap();
        assert_eq!(
            config["task"]["ios-verify"]["run"].as_str(),
            Some("scripts/ios-verify.sh")
        );
        assert!(include_str!("../../../scripts/ios-verify.sh").contains("xtask -- ios-verify"));
    }

    #[test]
    fn ios_verify_requires_every_journey_to_exist_and_report_a_full_pass() {
        let manifest = include_str!("../../../ios/Journeys/manifest.json");
        check_journeys(manifest).unwrap();
        let all: BTreeSet<_> = REQUIRED_JOURNEYS
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        check_completed_journeys(&all).unwrap();
        for required in REQUIRED_JOURNEYS {
            let mut value: serde_json::Value = serde_json::from_str(manifest).unwrap();
            value["journeys"]
                .as_array_mut()
                .unwrap()
                .retain(|journey| journey["id"] != *required);
            assert!(
                check_journeys(&value.to_string())
                    .unwrap_err()
                    .to_string()
                    .contains(required)
            );
            let mut incomplete = all.clone();
            incomplete.remove(*required);
            assert!(
                check_completed_journeys(&incomplete)
                    .unwrap_err()
                    .to_string()
                    .contains(required)
            );
        }
    }

    #[test]
    fn ios_verify_measures_hard_budgets_and_existing_baselines_only() {
        for hard in [false, true] {
            for baseline_present in [false, true] {
                let machine = PerfMachine {
                    name: "test-machine".into(),
                    hard,
                    baseline: "ios/Perf/baselines/test-machine.json".into(),
                    baseline_present,
                };
                assert_eq!(measure_perf(&machine), hard || baseline_present);
            }
        }
    }
}
