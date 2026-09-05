use std::error::Error;
use std::process::Command;

// Ordering matters: build the bridge and app before simulator checks. Destructive
// baseline updates and deliberate-failure probes are separate developer commands.
const RECIPES: &[&str] = &[
    "lint",
    "test",
    "spec",
    "mobile-check",
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

/// Whether a measured run on this machine would mean anything, or why not.
///
/// A machine with hard budgets is judged against numbers written down in the
/// measurement document, so it can be measured the moment it exists. A machine
/// judged against its own recorded run cannot: until that run has been
/// recorded there is nothing to compare with, and the suite would fail on an
/// absence rather than on a regression. Recording the baseline is what enrols
/// such a machine, with no edit here.
fn perf_selected(machine: &PerfMachine) -> Result<(), String> {
    if machine.hard || machine.baseline_present {
        return Ok(());
    }
    Err(format!(
        "{} is judged against its own recorded run and {} has not been recorded yet",
        machine.name, machine.baseline
    ))
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
    eprintln!("iOS verification: {}", selected.join(", "));
    for recipe in selected {
        if recipe == "ios-perf"
            && let Err(why) = perf_machine().and_then(|machine| perf_selected(&machine))
        {
            eprintln!("Skipping wt run ios-perf: {why}");
            continue;
        }
        let arguments = arguments(recipe);
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
            perf_selected(&machine(
                r#"{"name":"pinned-mac","hard":true,
                "baseline":"ios/Perf/baselines/pinned-mac.json","baseline_present":false}"#
            ))
            .is_ok()
        );
    }

    /// A machine judged against its own recorded run has nothing to compare
    /// with until that run exists, and the skip says which file is missing so
    /// recording it is the whole fix.
    #[test]
    fn a_machine_awaiting_its_baseline_is_skipped_until_the_file_exists() {
        let awaiting = machine(
            r#"{"name":"macos-26","hard":false,
                "baseline":"ios/Perf/baselines/macos-26.json","baseline_present":false}"#,
        );
        let why = perf_selected(&awaiting).expect_err("no baseline, no measurement");
        assert!(why.contains("macos-26"), "{why}");
        assert!(why.contains("ios/Perf/baselines/macos-26.json"), "{why}");

        let recorded = machine(
            r#"{"name":"macos-26","hard":false,
                "baseline":"ios/Perf/baselines/macos-26.json","baseline_present":true}"#,
        );
        assert!(perf_selected(&recorded).is_ok());
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

    #[test]
    fn ios_verify_grows_with_recipes_without_recursing_or_updating_goldens() {
        let selected = recipes("[task.mobile-check]\nrun='rust-check'\n[task.ios-verify]\nrun='verify'\n[task.ios-goldens]\nrun='goldens'\n[task.ci-gate]\nrun='push'\n[task.ios-goldens-perturb]\nrun='perturb'\n[task.ios-unit]\nrun='unit'").unwrap();
        assert_eq!(selected, ["mobile-check", "ios-unit", "ios-goldens"]);
    }
}
