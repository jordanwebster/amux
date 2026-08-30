use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub(crate) fn assert_binary_is_current(amux: &Path) -> Result<()> {
    if !amux.exists() {
        bail!(
            "amux binary missing at {}; run `cargo build -p amux-cli` first",
            amux.display()
        );
    }
    assert_binary_is_current_from_depfile(amux, &amux.with_extension("d"))
}

pub(crate) fn assert_binary_is_current_from_depfile(amux: &Path, depfile: &Path) -> Result<()> {
    let built = std::fs::metadata(amux)
        .and_then(|meta| meta.modified())
        .with_context(|| format!("stat {}", amux.display()))?;
    let contents = std::fs::read_to_string(depfile).with_context(|| {
        format!(
            "amux dependency file missing or unreadable at {}; run `cargo build -p amux-cli` first",
            depfile.display()
        )
    })?;
    for path in parse_depfile_prerequisites(&contents)? {
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .with_context(|| {
                format!(
                    "stat prerequisite {} from {}; rebuild with `cargo build -p amux-cli`",
                    path.display(),
                    depfile.display()
                )
            })?;
        if modified > built {
            bail!(
                "{} is older than dependency {}; run `cargo build -p amux-cli` — the live suite \
                 drives the prebuilt binary and would otherwise report on code it never ran",
                amux.display(),
                path.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn parse_depfile_prerequisites(contents: &str) -> Result<Vec<PathBuf>> {
    let continued = contents.replace("\\\r\n", " ").replace("\\\n", " ");
    let Some(colon) = continued.char_indices().find_map(|(index, character)| {
        (character == ':'
            && !is_escaped(&continued, index)
            && !is_windows_drive_colon(&continued, index))
        .then_some(index)
    }) else {
        bail!("invalid amux depfile: expected `<target>: <prerequisites>`");
    };
    let dependencies = parse_make_words(&continued[colon + 1..]);
    if dependencies.is_empty() {
        bail!("invalid amux depfile: rule has no prerequisites");
    }
    Ok(dependencies.into_iter().map(PathBuf::from).collect())
}

fn is_windows_drive_colon(value: &str, byte_index: usize) -> bool {
    byte_index == 1
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value
            .as_bytes()
            .get(byte_index + 1)
            .is_some_and(|byte| matches!(byte, b'\\' | b'/'))
}

fn is_escaped(value: &str, byte_index: usize) -> bool {
    value[..byte_index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

fn parse_make_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.peek().copied() {
                Some(next) if next.is_whitespace() || matches!(next, '\\' | '#' | ':' | '$') => {
                    word.push(characters.next().expect("peeked make escape"));
                }
                _ => word.push('\\'),
            }
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}
