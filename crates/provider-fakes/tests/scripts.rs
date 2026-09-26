//! The journey scripts are scripts every fake can play.

use std::path::Path;

use provider_fakes::{Script, Step, codex, pty, sdk};

#[test]
fn every_journey_script_loads_and_every_fake_accepts_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../journeys/scripts");
    let mut seen = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let script = Script::load(&path).unwrap_or_else(|error| panic!("{error}"));
        for (provider, raises) in [
            ("headless Claude", sdk::RAISES),
            ("terminal Claude", pty::RAISES),
            ("Codex", codex::RAISES),
        ] {
            script
                .check(provider, raises)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        }
        seen += 1;
    }
    assert!(seen >= 6, "found {seen} scripts under {}", root.display());
}

#[test]
fn a_pause_is_a_step() {
    let script: Script =
        serde_json::from_str(r#"{"steps": [{"pause": {"ms": 5}}, "turn_end"]}"#).unwrap();
    assert_eq!(script.steps, [Step::Pause { ms: 5 }, Step::TurnEnd]);
}
