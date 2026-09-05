use std::path::Path;

use anyhow::{Context, Result, anyhow};
use claude::pty::keymap::{
    KeymapError, KeymapFile, KeymapSource, KeymapSources, available, load_str, resolve,
};
use claude::version::{ClaudeVersion, probe_version};

use crate::KeymapCommands;

pub async fn run(command: KeymapCommands, directory: &Path) -> Result<()> {
    match command {
        KeymapCommands::List => {
            let version = probe_version(Path::new("claude"))
                .await
                .context("failed to determine the installed Claude version")?;
            print!("{}", list_output(directory, &version)?);
        }
        KeymapCommands::Show { name } => print!("{}", show_keymap(directory, &name)?),
        KeymapCommands::Add { file } => println!("{}", add_keymap(directory, &file)?),
        KeymapCommands::Remove { name } => println!("{}", remove_keymap(directory, &name)?),
        KeymapCommands::Dir => println!("{}", directory.display()),
    }
    Ok(())
}

fn sources(directory: &Path) -> KeymapSources {
    KeymapSources {
        baked: claude::pty::keymap::BAKED_KEYMAPS,
        user_dir: Some(directory.to_path_buf()),
    }
}

fn list_output(directory: &Path, version: &ClaudeVersion) -> Result<String> {
    let sources = sources(directory);
    let selected = resolve(&sources, version).map_err(keymap_error)?;
    let mut output = format!("Claude {version}\nNAME\tSOURCE\tAPPLICABLE RANGE\tBASIS\n");
    for file in available(&sources).map_err(keymap_error)? {
        let basis = if file.id == selected.keymap {
            selected.basis.to_string()
        } else {
            "NotSelected".to_owned()
        };
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            file.id.name,
            source_label(&file.id.source),
            file.applies_to,
            basis
        ));
    }
    Ok(output)
}

fn show_keymap(directory: &Path, name: &str) -> Result<String> {
    let file = named_keymap(directory, name)?;
    Ok(file.contents)
}

fn add_keymap(directory: &Path, input: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(input)
        .with_context(|| format!("failed to read keymap {}", input.display()))?;
    let origin = input.display().to_string();
    let candidate = load_str(&contents, &origin, KeymapSource::Baked).map_err(keymap_error)?;
    let inherited_verification = claude::pty::keymap::BAKED_KEYMAPS
        .iter()
        .filter_map(|(baked_origin, baked_contents)| {
            load_str(baked_contents, baked_origin, KeymapSource::Baked).ok()
        })
        .find(|baked| baked.name == candidate.name)
        .is_some_and(|baked| baked.verified == candidate.verified);
    if !candidate.verified.is_empty() && !inherited_verification {
        return Err(keymap_error(KeymapError::HandVerified { origin }));
    }

    let installed_contents = if candidate.verified.is_empty() {
        contents
    } else {
        strip_inherited_verification(&contents).ok_or_else(|| {
            anyhow!("Parse: could not find the top-level verified ledger in '{origin}'")
        })?
    };
    let source = KeymapSource::User(input.to_path_buf());
    let keymap = load_str(&installed_contents, &origin, source).map_err(keymap_error)?;
    validate_install_name(&keymap.name)?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create keymap directory {}", directory.display()))?;
    let destination = directory.join(format!("{}.toml", keymap.name));
    std::fs::write(&destination, installed_contents).with_context(|| {
        format!(
            "failed to write keymap {} to {}",
            input.display(),
            destination.display()
        )
    })?;
    Ok(format!(
        "Added keymap {} at {}",
        keymap.name,
        destination.display()
    ))
}

// Walks the raw bytes rather than `str::lines()`, which discards the line
// terminator and so cannot say how many bytes it consumed. A keymap authored on
// Windows arrives with CRLF, and assuming a one-byte terminator drifts the
// replacement window one byte per preceding line: the real ledger survives and a
// second `verified` key appears ahead of it, which TOML then rejects.
fn strip_inherited_verification(contents: &str) -> Option<String> {
    let mut offset = 0;
    while offset < contents.len() {
        let rest = &contents[offset..];
        let line = &rest[..rest.find('\n').map_or(rest.len(), |index| index + 1)];
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            // A section header ends the document root, where the ledger lives.
            return None;
        }
        if trimmed.starts_with("verified =") {
            let indentation = &line[..line.len() - trimmed.len()];
            let value_end = offset + line.trim_end_matches(['\r', '\n']).len();
            let mut stripped = contents.to_owned();
            stripped.replace_range(offset..value_end, &format!("{indentation}verified = []"));
            return Some(stripped);
        }
        offset += line.len();
    }
    None
}

fn remove_keymap(directory: &Path, name: &str) -> Result<String> {
    let file = named_keymap(directory, name)?;
    let KeymapSource::User(path) = file.id.source else {
        return Err(anyhow!("baked keymap '{name}' cannot be removed"));
    };
    std::fs::remove_file(&path)
        .with_context(|| format!("failed to remove keymap {}", path.display()))?;
    Ok(format!("Removed keymap {name}"))
}

fn named_keymap(directory: &Path, name: &str) -> Result<KeymapFile> {
    available(&sources(directory))
        .map_err(keymap_error)?
        .into_iter()
        .find(|file| file.id.name == name)
        .ok_or_else(|| anyhow!("unknown keymap '{name}'"))
}

fn validate_install_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!(
            "keymap name '{name}' must contain only letters, digits, '.', '-' or '_'"
        ));
    }
    Ok(())
}

fn source_label(source: &KeymapSource) -> String {
    match source {
        KeymapSource::Baked => "baked".to_owned(),
        KeymapSource::User(path) => format!("user:{}", path.display()),
    }
}

fn keymap_error(error: claude::pty::keymap::KeymapError) -> anyhow::Error {
    anyhow!("{}: {error}", error.kind())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use amux::keymap_dir;

    use super::*;

    const BAKED: &str = claude::pty::keymap::BAKED_KEYMAPS[0].1;

    #[test]
    fn keymap_list_names_source_range_and_installed_basis() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("keymaps");
        let output = list_output(&directory, &"2.1.251".parse().unwrap()).unwrap();
        assert!(output.contains("Claude 2.1.251"));
        assert!(output.contains("claude-2.1\tbaked"));
        assert!(output.contains(">=2.1.228, <2.2.0"));
        assert!(output.contains("Verified(2.1.251)"));
    }

    #[test]
    fn keymap_show_prints_the_effective_file() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("keymaps");
        assert_eq!(show_keymap(&directory, "claude-2.1").unwrap(), BAKED);
        assert!(
            show_keymap(&directory, "missing")
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
    }

    #[test]
    fn keymap_add_installs_a_validated_user_file_and_remove_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("keymaps");
        let input = dir.path().join("override.toml");
        std::fs::write(
            &input,
            BAKED.replace("after_paste = 400", "after_paste = 401"),
        )
        .unwrap();

        let added = add_keymap(&directory, &input).unwrap();
        let installed = directory.join("claude-2.1.toml");
        assert!(added.contains(&installed.display().to_string()));
        assert!(installed.is_file());
        assert!(
            std::fs::read_to_string(&installed)
                .unwrap()
                .contains("verified = []")
        );
        let output = list_output(&directory, &"2.1.251".parse().unwrap()).unwrap();
        assert!(output.contains(&format!(
            "claude-2.1\tuser:{}\t>=2.1.228, <2.2.0\tInRange",
            installed.display()
        )));
        assert!(
            show_keymap(&directory, "claude-2.1")
                .unwrap()
                .contains("after_paste = 401")
        );

        assert_eq!(
            remove_keymap(&directory, "claude-2.1").unwrap(),
            "Removed keymap claude-2.1"
        );
        assert!(!installed.exists());
    }

    #[test]
    fn keymap_add_strips_the_inherited_ledger_from_a_crlf_authored_file() {
        // Windows checkouts and editors hand us CRLF. The ledger must still be
        // replaced in place, leaving exactly one `verified` key in the root.
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("keymaps");
        let input = dir.path().join("override.toml");
        // The baked file's own terminators follow the checkout, so normalize to
        // LF before converting; doubling them would author a file no editor
        // produces and TOML rejects outright.
        let crlf = BAKED.replace("\r\n", "\n").replace('\n', "\r\n");
        assert!(!crlf.contains("\r\r"));
        std::fs::write(&input, &crlf).unwrap();

        add_keymap(&directory, &input).unwrap();

        let installed = std::fs::read_to_string(directory.join("claude-2.1.toml")).unwrap();
        assert!(installed.contains("verified = []"));
        assert_eq!(installed.matches("verified =").count(), 1);
    }

    #[test]
    fn keymap_add_rejects_malformed_and_hand_verified_files() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("keymaps");
        let malformed = dir.path().join("malformed.toml");
        std::fs::write(&malformed, "name = [").unwrap();
        let malformed_error = add_keymap(&directory, &malformed).unwrap_err().to_string();
        assert!(malformed_error.contains("Parse"));
        assert!(malformed_error.contains("malformed.toml"));

        let hand_verified = dir.path().join("hand-verified.toml");
        let claimed = BAKED.replacen(
            "verified = [",
            "verified = [{ version = \"2.1.251\", run_id = \"manual\", spec = \"prompt\" }, ",
            1,
        );
        std::fs::write(&hand_verified, claimed).unwrap();
        let error = add_keymap(&directory, &hand_verified)
            .unwrap_err()
            .to_string();
        assert!(error.contains("HandVerified"));
        assert!(error.contains("hand-authored verified versions"));
    }

    #[test]
    fn keymap_dir_is_below_the_configured_data_directory() {
        assert_eq!(
            keymap_dir(Path::new("/var/lib/amux-test")),
            PathBuf::from("/var/lib/amux-test/keymaps")
        );
    }
}
