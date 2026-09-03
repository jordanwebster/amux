use std::fs;
use std::path::Path;

use amux_tui::replay;
use amux_ui::report::ReplayVerdict;

#[test]
fn every_committed_report_fixture_reproduces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reports");
    let mut fixtures = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .map(|entry| entry.expect("failed to read fixture entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    fixtures.sort();
    assert!(
        !fixtures.is_empty(),
        "no report fixtures in {}",
        root.display()
    );

    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    for fixture in fixtures {
        let verdict = replay::verify(&fixture)
            .unwrap_or_else(|error| panic!("failed to replay {}: {error}", fixture.display()));
        assert_eq!(
            verdict,
            ReplayVerdict::Reproduces,
            "{} no longer reproduces",
            fixture.display()
        );
        assert_redacted(&fixture, &hostname);
    }
}

fn assert_redacted(path: &Path, hostname: &str) {
    for entry in fs::read_dir(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    {
        let entry = entry.expect("failed to read fixture entry");
        let entry_path = entry.path();
        if entry_path.is_dir() {
            assert_redacted(&entry_path, hostname);
            continue;
        }
        assert_private_text_absent(&entry_path, hostname);
    }
}

fn assert_private_text_absent(path: &Path, hostname: &str) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("fixture file {} is not UTF-8: {error}", path.display()));
    for private in ["/Users/", "/home/", hostname] {
        if !private.is_empty() {
            assert!(
                !text.contains(private),
                "fixture file {} contains private text {private:?}",
                path.display()
            );
        }
    }
}
