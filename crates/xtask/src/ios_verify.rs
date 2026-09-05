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

pub fn run() -> Result<(), Box<dyn Error>> {
    let selected = recipes(&std::fs::read_to_string(".wt.toml")?)?;
    eprintln!("iOS verification: {}", selected.join(", "));
    for recipe in selected {
        eprintln!("Running wt run {recipe}");
        let status = Command::new("timeout")
            .args(["1800", "wt", "run", recipe])
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

    #[test]
    fn ios_verify_grows_with_recipes_without_recursing_or_updating_goldens() {
        let selected = recipes("[task.mobile-check]\nrun='rust-check'\n[task.ios-verify]\nrun='verify'\n[task.ios-goldens]\nrun='goldens'\n[task.ci-gate]\nrun='push'\n[task.ios-goldens-perturb]\nrun='perturb'\n[task.ios-unit]\nrun='unit'").unwrap();
        assert_eq!(selected, ["mobile-check", "ios-unit", "ios-goldens"]);
    }
}
