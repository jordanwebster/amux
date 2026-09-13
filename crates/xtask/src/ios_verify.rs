use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

// Ordering matters: build the bridge and app before simulator checks. Destructive
// baseline updates and deliberate-failure probes are separate developer commands.
// Each entry is a `just` invocation; a leading `ios` names the phone module.

/// The Rust workspace the phone's bridge is cut from.
///
/// Someone running one command before pushing wants these first, because a
/// bridge built from a workspace that does not compile is not worth
/// photographing. Continuous integration already runs every one of them as
/// its own job, on three operating systems, so it asks for the phone stages
/// alone rather than paying for a second copy of the same fourteen minutes.
const WORKSPACE: &[&str] = &["fmt-check", "lint", "test", "spec"];

/// Everything about the phone that building it can settle.
///
/// These answer from code and from one simulator: what the device and
/// simulator graphs are allowed to contain, whether the bridge and the app
/// build, whether the packaged framework links and loads, and what the unit
/// suites say. Nothing here compares a photograph, so nothing here depends on
/// which machine is looking.
const GATE: &[&str] = &[
    "mobile-check",
    "ios lint",
    "ios graph-check",
    "ios rust",
    "ios simulator golden",
    "ios build",
    "ios loopback-smoke",
    "ios unit",
];

/// What the shipping bundle is held to: every slice built under the
/// size-optimised profile, and the audit of what that bundle turned out to
/// contain.
///
/// Off the push path because of what it costs against what it can catch. It
/// was ten minutes of a thirty-four minute gate, and it asks a question about
/// the release build that no ordinary change can answer differently — the
/// graphs the gate already checks are what decide whether a provider or a
/// test crate can reach the phone. `just ios release` depends on both, so a
/// release cannot be cut without them.
const SHIPPING: &[&str] = &["ios package", "ios scope-audit"];

/// Everything that drives a running app and judges what it drew.
///
/// This is the slow half and the environment-sensitive half, and they are the
/// same half for one reason: a photograph of a simulator records the machine
/// that took it as well as the app. Kept apart from the gate so that a change
/// to the app is not held up by a difference between two Macs.
const CAPTURES: &[&str] = &[
    "ios door-smoke",
    "ios goldens",
    "ios journey",
    "ios accessibility",
    "ios perf",
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

/// The recipe names a justfile declares: every line that starts a recipe,
/// with its parameters and dependencies stripped.
fn declared(justfile: &str) -> BTreeSet<&str> {
    justfile
        .lines()
        .filter(|line| !line.starts_with(['#', ' ', '\t']) && !line.starts_with("set "))
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, rest)| !rest.starts_with('=') && !name.is_empty())
        .map(|(name, _)| name.split_whitespace().next().unwrap_or(name))
        .collect()
}

/// Which stages an invocation asks for. The whole thing by default, because
/// the developer's one command is the reason this exists; continuous
/// integration names the half it owns.
#[derive(Clone, Copy, PartialEq)]
enum Phases {
    Everything,
    Gate,
    Captures,
    Shipping,
}

impl Phases {
    fn parse(argument: Option<&str>) -> Result<Self, Box<dyn Error>> {
        match argument {
            None => Ok(Self::Everything),
            Some("--gate") => Ok(Self::Gate),
            Some("--captures") => Ok(Self::Captures),
            Some("--shipping") => Ok(Self::Shipping),
            Some(other) => Err(format!(
                "ios-verify takes --gate, --captures or --shipping, not {other}"
            )
            .into()),
        }
    }

    fn recipes(self) -> Vec<&'static str> {
        match self {
            Self::Everything => WORKSPACE
                .iter()
                .chain(GATE)
                .chain(CAPTURES)
                .chain(SHIPPING)
                .copied()
                .collect(),
            Self::Gate => GATE.to_vec(),
            Self::Captures => CAPTURES.to_vec(),
            Self::Shipping => SHIPPING.to_vec(),
        }
    }

    /// Journeys are only owed by a run that drives them.
    fn drives_journeys(self) -> bool {
        self != Self::Gate
    }
}

/// Every verification stage must be a recipe the two justfiles declare, so a
/// renamed or removed recipe fails here, by name, before anything runs. Every
/// stage is checked whichever subset was asked for, so a rename cannot hide
/// behind the half nobody ran today.
fn recipes(phases: Phases, root: &str, ios: &str) -> Result<Vec<&'static str>, Box<dyn Error>> {
    let root = declared(root);
    let ios = declared(ios);
    for recipe in WORKSPACE.iter().chain(GATE).chain(CAPTURES).chain(SHIPPING) {
        // A stage may carry arguments; the recipe is its first word.
        let known = match recipe.strip_prefix("ios ") {
            Some(rest) => ios.contains(rest.split(' ').next().unwrap_or(rest)),
            None => root.contains(recipe.split(' ').next().unwrap_or(recipe)),
        };
        if !known {
            return Err(format!("iOS verification requires recipe {recipe}").into());
        }
    }
    Ok(phases.recipes())
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

fn is_package_metadata(key: &OsStr) -> bool {
    key.to_str().is_some_and(|key| {
        key == "CARGO_MANIFEST_DIR" || key == "CARGO_MANIFEST_PATH" || key.starts_with("CARGO_PKG_")
    })
}

fn remove_package_metadata(command: &mut Command, keys: impl Iterator<Item = OsString>) {
    for key in keys.filter(|key| is_package_metadata(key)) {
        command.env_remove(key);
    }
}

/// Cargo supplies the runner's package metadata when launching it. Passing
/// those values to a fresh Cargo invocation changes build-script fingerprints
/// relative to running the same recipe directly, rebuilding valid dependencies.
/// Keep target, profile, toolchain and user configuration; only the outer
/// package's metadata is unrelated to the child build.
fn recipe_command(program: &str) -> Command {
    let mut command = Command::new(program);
    remove_package_metadata(&mut command, std::env::vars_os().map(|(key, _)| key));
    command
}

/// Asks the measurement script which machine this is. The script owns the
/// answer; nothing here reads the measurement document.
fn perf_machine() -> Result<PerfMachine, String> {
    let output = recipe_command(crate::BOUNDED)
        .args(["120", "scripts/python", "-B", "scripts/ios-perf.py", "--machine"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let argument = std::env::args().nth(2);
    let phases = Phases::parse(argument.as_deref())?;
    let selected = recipes(
        phases,
        &std::fs::read_to_string("justfile")?,
        &std::fs::read_to_string("apps/apple/justfile")?,
    )?;
    if phases.drives_journeys() {
        check_journeys(&std::fs::read_to_string(
            "apps/apple/Journeys/manifest.json",
        )?)?;
        eprintln!("Required iOS journeys: {}", REQUIRED_JOURNEYS.join(", "));
    }
    eprintln!("iOS verification: {}", selected.join(", "));
    for recipe in selected {
        if recipe == "ios perf" {
            let machine = perf_machine()?;
            if !measure_perf(&machine) {
                report_missing_baseline(&machine)?;
                continue;
            }
        }
        eprintln!("Running just {recipe}");
        // Recipes own their individual deadlines. The outer deadline also
        // bounds dependencies without cutting off a longer recipe early.
        let mut child = recipe_command(crate::BOUNDED)
            .arg("12600")
            .arg("just")
            .args(recipe.split(' '))
            .stdout(Stdio::piped())
            .spawn()?;
        let mut completed = BTreeSet::new();
        for line in BufReader::new(child.stdout.take().ok_or("no recipe stdout")?).lines() {
            let line = line?;
            println!("{line}");
            if recipe == "ios journey"
                && let Some(id) = line.strip_suffix(": passed")
            {
                completed.insert(id.to_owned());
            }
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("just {recipe} failed: {status}").into());
        }
        if recipe == "ios journey" {
            check_completed_journeys(&completed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_verify_removes_outer_package_metadata_but_preserves_build_configuration() {
        let removed = [
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION",
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_PKG_AUTHORS",
            "CARGO_PKG_RUST_VERSION",
            "CARGO_PKG_README",
        ];
        let retained = [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_AARCH64_APPLE_IOS_LINKER",
            "CARGO_NET_OFFLINE",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
            "DEVELOPER_DIR",
            "PATH",
            "AMUX_CONFIG",
        ];
        let mut command = Command::new("unused");
        for key in removed.into_iter().chain(retained) {
            command.env(key, "unchanged");
        }
        remove_package_metadata(
            &mut command,
            removed.into_iter().chain(retained).map(OsString::from),
        );
        let configured: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in removed {
            assert_eq!(configured[OsStr::new(key)], None, "{key}");
        }
        for key in retained {
            assert_eq!(
                configured[OsStr::new(key)],
                Some(OsStr::new("unchanged")),
                "{key}"
            );
        }
        // The normal constructor sanitizes the inherited environment too,
        // not just explicit Command::env overrides used above.
        let inherited = recipe_command("unused");
        for (key, _) in std::env::vars_os().filter(|(key, _)| is_package_metadata(key)) {
            assert!(
                inherited
                    .get_envs()
                    .any(|(name, value)| name == key && value.is_none())
            );
        }
    }

    const ROOT_JUSTFILE: &str = include_str!("../../../justfile");
    const IOS_JUSTFILE: &str = include_str!("../../../apps/apple/justfile");

    /// Every stage of every phase, in the order a whole run takes them.
    fn all_recipes() -> Vec<&'static str> {
        WORKSPACE
            .iter()
            .chain(GATE)
            .chain(CAPTURES)
            .chain(SHIPPING)
            .copied()
            .collect()
    }

    /// The recipe a stage names, without whatever arguments it carries.
    fn recipe_name(stage: &str) -> &str {
        let stage = stage.strip_prefix("ios ").unwrap_or(stage);
        stage.split(' ').next().unwrap_or(stage)
    }

    /// Justfiles declaring exactly the verification recipes, so a test can
    /// remove one and watch the check name it.
    fn justfiles() -> (String, String) {
        let mut root = String::new();
        let mut ios = String::new();
        for recipe in all_recipes() {
            let line = format!("{}:\n    true\n", recipe_name(recipe));
            match recipe.strip_prefix("ios ") {
                Some(_) => ios.push_str(&line),
                None => root.push_str(&line),
            }
        }
        (root, ios)
    }

    #[test]
    fn ios_verify_requires_every_recipe_without_selecting_update_or_remote_commands() {
        let (root, ios) = justfiles();
        assert_eq!(
            recipes(Phases::Everything, &root, &ios).unwrap(),
            all_recipes()
        );
        for recipe in all_recipes() {
            let (mut root, mut ios) = justfiles();
            let line = format!("{}:\n    true\n", recipe_name(recipe));
            match recipe.strip_prefix("ios ") {
                Some(_) => ios = ios.replace(&line, ""),
                None => root = root.replace(&line, ""),
            }
            // Whichever half is asked for, a missing recipe is named.
            for phases in [
                Phases::Everything,
                Phases::Gate,
                Phases::Captures,
                Phases::Shipping,
            ] {
                assert!(
                    recipes(phases, &root, &ios)
                        .unwrap_err()
                        .to_string()
                        .contains(recipe)
                );
            }
        }
        for excluded in [
            "ios verify",
            "ios goldens-perturb",
            "ios ci-gate",
            "ios ci-observe",
            "ios qa-cloud-signin",
            "ios qa-sandbox-purchase",
            "ios qa-live-journey",
            "ios release",
        ] {
            assert!(!all_recipes().contains(&excluded));
        }
    }

    #[test]
    fn ios_verify_recipes_exist_and_run_accessibility_after_journeys() {
        let selected = recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        let journey = selected
            .iter()
            .position(|name| *name == "ios journey")
            .unwrap();
        assert_eq!(selected[journey + 1], "ios accessibility");
        assert!(declared(IOS_JUSTFILE).contains("verify"));
        assert!(IOS_JUSTFILE.contains("xtask -- ios-verify"));
    }

    #[test]
    fn ios_verify_checks_nightly_formatting_before_compilation() {
        assert_eq!(
            recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap()[0],
            "fmt-check"
        );
        let fmt_check = ROOT_JUSTFILE
            .lines()
            .skip_while(|line| !line.starts_with("fmt-check:"))
            .nth(1)
            .unwrap();
        assert!(
            fmt_check.contains("cargo +nightly-") && fmt_check.contains("fmt --all -- --check")
        );
    }

    /// The gate is what gets to hold up a push, so what it may contain is a
    /// rule rather than a habit: nothing that judges a photograph, because
    /// two Macs disagree about those, and every recipe it names must exist.
    #[test]
    fn the_gate_builds_and_measures_nothing_it_has_to_photograph() {
        let gate = recipes(Phases::Gate, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        for photographed in ["ios goldens", "ios journey", "ios accessibility"] {
            assert!(
                !gate.contains(&photographed),
                "{photographed} compares pictures and cannot gate a push"
            );
        }
        assert!(gate.contains(&"ios unit"), "the gate stopped running units");
        for shipping in SHIPPING {
            assert!(
                !gate.contains(shipping),
                "{shipping} builds the shipping bundle and does not gate a push"
            );
        }
        assert!(
            !gate.iter().any(|stage| WORKSPACE.contains(stage)),
            "the gate repeats workspace jobs continuous integration already runs"
        );
    }

    /// Between them the parts are the whole thing, in the same order, so
    /// splitting the run cannot quietly drop a stage.
    #[test]
    fn the_parts_are_the_whole_of_verification() {
        let everything = recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        let split: Vec<&str> = recipes(Phases::Gate, ROOT_JUSTFILE, IOS_JUSTFILE)
            .unwrap()
            .into_iter()
            .chain(recipes(Phases::Captures, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap())
            .chain(recipes(Phases::Shipping, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap())
            .collect();
        assert_eq!(everything, [WORKSPACE.to_vec(), split].concat());
    }

    /// The shipping stages leave the push path, so the release recipe is the
    /// thing that must still owe them.
    #[test]
    fn a_release_cannot_be_cut_without_the_shipping_stages() {
        let release = IOS_JUSTFILE
            .lines()
            .find(|line| line.starts_with("release "))
            .expect("an ios release recipe");
        for shipping in SHIPPING {
            let name = shipping.strip_prefix("ios ").unwrap_or(shipping);
            assert!(
                release.contains(name),
                "`{release}` no longer depends on {name}, which nothing else now runs"
            );
        }
    }

    #[test]
    fn declared_recipes_ignore_settings_assignments_and_bodies() {
        let names = declared(
            "set shell := [\"sh\"]\nbounded := \"x\"\n# doc\nbuild: rust\n    cargo build\nunit *ARGS: rust\n    true\n",
        );
        assert_eq!(names, BTreeSet::from(["build", "unit"]));
    }

    #[test]
    fn ios_verify_requires_every_journey_to_exist_and_report_a_full_pass() {
        let manifest = include_str!("../../../apps/apple/Journeys/manifest.json");
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
                    baseline: "apps/apple/Perf/baselines/test-machine.json".into(),
                    baseline_present,
                };
                assert_eq!(measure_perf(&machine), hard || baseline_present);
            }
        }
    }
}
