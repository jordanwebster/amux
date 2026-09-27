//! Component goldens: each piece of the chat vocabulary drawn from authored
//! view values into the test backend, as text and as a map of semantic
//! style classes (see `Theme::classify`), in the light and dark themes.
//!
//! Rewrite with `UPDATE_GOLDENS=1 just test-tui` and review the diff like
//! code; CI refuses to rewrite.

#![cfg(feature = "fixtures")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use tui::vocabulary::components;
use tui::{ColorMode, Theme};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn themes() -> [(&'static str, Theme); 2] {
    [
        ("light", Theme::light(ColorMode::TrueColor)),
        ("dark", Theme::dark(ColorMode::TrueColor)),
    ]
}

/// The buffer's text, then its style classes, one character per cell.
fn capture(buffer: &Buffer, theme: Theme) -> String {
    let area = buffer.area;
    let mut text = String::new();
    let mut styles = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            line.push_str(cell.symbol());
            styles.push(theme.classify(cell.style()));
        }
        text.push_str(line.trim_end());
        text.push('\n');
        styles.push('\n');
    }
    format!("{text}--- styles\n{styles}")
}

/// Compares `rendered` with the golden `name`, or rewrites it under
/// UPDATE_GOLDENS outside CI.
fn assert_golden(name: &str, rendered: &str) {
    let path = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "UPDATE_GOLDENS is refused in CI: rewrite goldens locally and review them"
        );
        std::fs::create_dir_all(golden_dir()).expect("golden directory");
        std::fs::write(&path, rendered).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run with UPDATE_GOLDENS=1 and review"));
    assert!(
        rendered == expected,
        "{name} differs from its golden; if intended, rerun with UPDATE_GOLDENS=1 and review.\n\
         rendered:\n{rendered}"
    );
}

#[test]
fn every_vocabulary_component_matches_its_golden_in_both_themes() {
    for (variant, theme) in themes() {
        for component in components(theme) {
            assert_golden(
                &format!("{}.{variant}", component.name),
                &capture(&component.buffer, theme),
            );
        }
    }
}

#[test]
fn every_golden_file_belongs_to_a_component() {
    let mut expected = BTreeSet::new();
    for (variant, theme) in themes() {
        for component in components(theme) {
            expected.insert(format!("{}.{variant}.txt", component.name));
        }
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(golden_dir())
        .expect("golden directory")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".txt") && !name.starts_with("frame_"))
        .collect();
    let stale: Vec<_> = on_disk.difference(&expected).collect();
    assert!(stale.is_empty(), "goldens no component draws: {stale:?}");
}

#[test]
fn a_component_name_is_used_once() {
    let names: Vec<_> = components(Theme::default())
        .into_iter()
        .map(|component| component.name)
        .collect();
    let unique: BTreeSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
}

#[test]
fn every_row_kind_has_a_component() {
    // A new row kind must earn a drawing here; this list is the catalogue.
    let names: BTreeSet<_> = components(Theme::default())
        .into_iter()
        .map(|component| component.name)
        .collect();
    for kind in [
        "prompt",
        "prose",
        "thinking",
        "tool_call",
        "file_change",
        "command",
        "explore",
        "subagent",
        "background",
        "image",
        "slash_output",
        "ask",
        "turn_end",
        "stopped",
        "compaction",
        "error",
        "model_switch",
        "boundary",
        "agent_message",
        "auto_review",
        "unrecognized",
    ] {
        assert!(names.contains(format!("row_{kind}").as_str()), "row_{kind}");
    }
}
