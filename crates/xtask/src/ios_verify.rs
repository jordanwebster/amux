use std::error::Error;
use std::process::Command;

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
    let selected: Vec<_> = RECIPES
        .iter()
        .copied()
        .filter(|name| tasks.contains_key(*name))
        .collect();
    if !selected
        .iter()
        .any(|name| matches!(*name, "test" | "mobile-check" | "ios-rust"))
    {
        return Err("iOS verification has no Rust checks".into());
    }
    Ok(selected)
}

/// What this branch's own verification asks a recipe for.
///
/// Mid-flight the goldens are asked the narrower of their two questions: of
/// the screens that exist today, does every one still draw what it was locked
/// as. The whole catalogue — every screen the flight owes, built or not — is
/// the closing gate, and stays a bare `wt run ios-goldens`.
fn arguments(recipe: &str) -> &'static [&'static str] {
    match recipe {
        "ios-goldens" => &["--", "--built"],
        _ => &[],
    }
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

/// What a measured run on this machine has to be asked for.
///
/// A machine with hard budgets, or one that has already recorded a run, is
/// measured with no argument: the numbers it is judged against exist. A machine
/// judged against its own recorded run and holding no recording yet is asked
/// for that recording, because skipping it instead is a state nothing leaves:
/// the run that would write the baseline is the run being skipped for want of
/// one. The recording run is still judged, against the budgets the definitions
/// pin, and its medians become what the next run is held to.
fn perf_arguments(machine: &PerfMachine) -> Vec<&'static str> {
    if machine.hard || machine.baseline_present {
        return Vec::new();
    }
    vec!["--", "--baseline"]
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
        let mut extra: Vec<&'static str> = Vec::new();
        if recipe == "ios-perf" {
            match perf_machine() {
                // An unrecognised Mac has no budget row at all, so a number
                // from it would mean nothing and there is nothing to record.
                Err(why) => {
                    eprintln!("Skipping wt run ios-perf: {why}");
                    continue;
                }
                Ok(machine) => {
                    extra = perf_arguments(&machine);
                    if !extra.is_empty() {
                        eprintln!(
                            "Recording {}'s baseline: {} does not exist yet",
                            machine.name, machine.baseline
                        );
                    }
                }
            }
        }
        let arguments = [arguments(recipe), extra.as_slice()].concat();
        eprintln!("Running wt run {recipe} {}", arguments.join(" "));
        let status = Command::new("timeout")
            .args(["1800", "wt", "run", recipe])
            .args(arguments)
            .status()?;
        if !status.success() {
            return Err(format!("wt run {recipe} failed: {status}").into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_verify_requires_sdk_sessions_alongside_every_other_journey() {
        let manifest = include_str!("../../../ios/Journeys/manifest.json");
        check_journeys(manifest).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(manifest).unwrap();
        value["journeys"]
            .as_array_mut()
            .unwrap()
            .retain(|journey| journey["id"] != "claude-sessions");
        assert!(
            check_journeys(&value.to_string())
                .unwrap_err()
                .to_string()
                .contains("claude-sessions")
        );
    }

    #[test]
    fn ios_verify_rejects_empty_or_ui_only_verification() {
        for config in ["", "[task.ios-unit]\nrun='true'", "[task.lint]\nrun='true'"] {
            assert!(recipes(config).is_err());
        }
    }

    fn machine(json: &str) -> PerfMachine {
        serde_json::from_str(json).expect("a machine row")
    }

    /// The pinned Mac's budgets are written down, so it is measured whether or
    /// not anybody has recorded a run on it.
    #[test]
    fn a_machine_with_written_budgets_is_measured() {
        assert!(
            perf_arguments(&machine(
                r#"{"name":"pinned-mac","hard":true,
                "baseline":"ios/Perf/baselines/pinned-mac.json","baseline_present":false}"#
            ))
            .is_empty()
        );
    }

    /// A machine judged against its own recorded run has nothing to compare
    /// with until that run exists, so its first verification takes the run that
    /// records it. Skipping instead would never end: the missing file is what
    /// the skipped run writes.
    #[test]
    fn a_machine_awaiting_its_baseline_records_one() {
        let awaiting = machine(
            r#"{"name":"macos-26","hard":false,
                "baseline":"ios/Perf/baselines/macos-26.json","baseline_present":false}"#,
        );
        assert_eq!(perf_arguments(&awaiting), ["--", "--baseline"]);

        let recorded = machine(
            r#"{"name":"macos-26","hard":false,
                "baseline":"ios/Perf/baselines/macos-26.json","baseline_present":true}"#,
        );
        assert!(perf_arguments(&recorded).is_empty());
    }

    /// Mid-flight the goldens are run over the screens that exist; the whole
    /// catalogue is the closing gate and nothing else takes an argument.
    #[test]
    fn verification_runs_the_goldens_over_the_screens_that_exist() {
        assert_eq!(arguments("ios-goldens"), ["--", "--built"]);
        for recipe in RECIPES.iter().filter(|name| **name != "ios-goldens") {
            assert!(arguments(recipe).is_empty(), "{recipe} was given arguments");
        }
    }

    /// The screens package's own rules — no UIKit outside a registered leaf,
    /// no platform conditionals, no spinners — are only rules if a check runs
    /// them, so this branch's verification has to name the recipe that does.
    #[test]
    fn verification_runs_the_screens_lint() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let config = std::fs::read_to_string(root.join(".wt.toml")).expect("the checkout's tasks");
        let selected = recipes(&config).expect("a verification selection");
        assert!(selected.contains(&"ios-lint"), "{selected:?}");
    }

    #[test]
    fn ios_verify_grows_with_recipes_without_recursing_or_updating_goldens() {
        let selected = recipes("[task.mobile-check]\nrun='rust-check'\n[task.ios-verify]\nrun='verify'\n[task.ios-goldens]\nrun='goldens'\n[task.ci-gate]\nrun='push'\n[task.ios-goldens-perturb]\nrun='perturb'\n[task.ios-unit]\nrun='unit'").unwrap();
        assert_eq!(selected, ["mobile-check", "ios-unit", "ios-goldens"]);
    }
}
