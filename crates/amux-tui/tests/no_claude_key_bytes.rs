//! No client authors a Claude key byte.
//!
//! Claude's PTY session is driven by semantic intents: a prompt, an
//! interrupt, a permission-mode cycle, an answer naming the ask it
//! answers. Which keystrokes carry one is chosen by the session, from a
//! keymap resolved against the Claude version actually running there. A
//! byte table on this side of the wire would be a second, unversioned
//! answer to the same question — right until Claude renumbers a menu,
//! after which it would type the wrong key into a real session and only
//! the user would find out.
//!
//! So this scans the Claude client code for the literals such a table is
//! made of and fails on any of them. Escape and CSI have no other use
//! here at all. A carriage return does — pasted text and transcript
//! content arrive carrying them — so it is allowed only where the line
//! that mentions it is taking one out.

use std::path::{Path, PathBuf};

/// Escape and the 8-bit CSI, in every literal form Rust accepts, plus the
/// bare bytes themselves.
const FORBIDDEN: &[(&str, &str)] = &[
    ("\u{1b}", "an escape byte"),
    ("\\u{1b}", "an escape byte"),
    ("\\u{1B}", "an escape byte"),
    ("\\x1b", "an escape byte"),
    ("\\x1B", "an escape byte"),
    ("\\e", "an escape byte"),
    ("\\033", "an escape byte"),
    ("\u{9b}", "a CSI byte"),
    ("\\u{9b}", "a CSI byte"),
    ("\\u{9B}", "a CSI byte"),
    ("\\x9b", "a CSI byte"),
    ("\\x9B", "a CSI byte"),
];

/// A line may name a carriage return only to strip, replace or test for
/// one — inbound normalization, never an outbound key.
const CONSUMING: &[&str] = &["replace(", "strip_suffix(", "strip_prefix(", "contains("];

fn roots() -> Vec<PathBuf> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        crate_dir.join("src/chat"),
        crate_dir.join("../amux-ui/src/claude"),
    ]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {dir:?}: {error}")) {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_claude_client_holds_no_key_bytes() {
    let mut files = Vec::new();
    for root in roots() {
        rust_files(&root, &mut files);
    }
    assert!(
        files.len() > 5,
        "the scan found almost nothing — the roots moved: {files:?}"
    );

    let mut found: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("source file is UTF-8");
        for (number, line) in text.lines().enumerate() {
            let at = format!("{}:{}", file.display(), number + 1);
            for (literal, what) in FORBIDDEN {
                if line.contains(literal) {
                    found.push(format!("{at}: {what} ({literal})"));
                }
            }
            let carriage_return = line.contains('\r') || line.contains("\\r");
            if carriage_return && !CONSUMING.iter().any(|call| line.contains(call)) {
                found.push(format!("{at}: a carriage return that is not being removed"));
            }
        }
    }

    assert!(
        found.is_empty(),
        "the Claude client must author no key bytes — the session encodes intents:\n{}",
        found.join("\n")
    );
}
