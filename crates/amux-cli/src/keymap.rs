use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use claude::pty::keymap::{KeymapFile, KeymapSource, KeymapSources, available, load, resolve};
use claude::version::{ClaudeVersion, probe_version};

use crate::KeymapCommands;

pub async fn run(command: KeymapCommands, data_dir: &Path) -> Result<()> {
    match command {
        KeymapCommands::List => {
            let version = probe_version(Path::new("claude"))
                .await
                .context("failed to determine the installed Claude version")?;
            print!("{}", list_output(data_dir, &version)?);
        }
        KeymapCommands::Show { name } => print!("{}", show_keymap(data_dir, &name)?),
        KeymapCommands::Add { file } => println!("{}", add_keymap(data_dir, &file)?),
        KeymapCommands::Remove { name } => println!("{}", remove_keymap(data_dir, &name)?),
        KeymapCommands::Dir => println!("{}", keymap_dir(data_dir).display()),
    }
    Ok(())
}

fn keymap_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("keymaps")
}

fn sources(data_dir: &Path) -> KeymapSources {
    KeymapSources {
        baked: claude::pty::keymap::BAKED_KEYMAPS,
        user_dir: Some(keymap_dir(data_dir)),
    }
}

fn list_output(data_dir: &Path, version: &ClaudeVersion) -> Result<String> {
    let sources = sources(data_dir);
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

fn show_keymap(data_dir: &Path, name: &str) -> Result<String> {
    let file = named_keymap(data_dir, name)?;
    Ok(file.contents)
}

fn add_keymap(data_dir: &Path, input: &Path) -> Result<String> {
    let source = KeymapSource::User(input.to_path_buf());
    let keymap = load(input, source).map_err(keymap_error)?;
    validate_install_name(&keymap.name)?;
    let directory = keymap_dir(data_dir);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create keymap directory {}", directory.display()))?;
    let destination = directory.join(format!("{}.toml", keymap.name));
    if input != destination {
        std::fs::copy(input, &destination).with_context(|| {
            format!(
                "failed to copy keymap {} to {}",
                input.display(),
                destination.display()
            )
        })?;
    }
    Ok(format!(
        "Added keymap {} at {}",
        keymap.name,
        destination.display()
    ))
}

fn remove_keymap(data_dir: &Path, name: &str) -> Result<String> {
    let file = named_keymap(data_dir, name)?;
    let KeymapSource::User(path) = file.id.source else {
        return Err(anyhow!("baked keymap '{name}' cannot be removed"));
    };
    std::fs::remove_file(&path)
        .with_context(|| format!("failed to remove keymap {}", path.display()))?;
    Ok(format!("Removed keymap {name}"))
}

fn named_keymap(data_dir: &Path, name: &str) -> Result<KeymapFile> {
    available(&sources(data_dir))
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
    use super::*;

    const BAKED: &str = claude::pty::keymap::BAKED_KEYMAPS[0].1;

    #[test]
    fn keymap_list_names_source_range_and_installed_basis() {
        let dir = tempfile::tempdir().unwrap();
        let output = list_output(dir.path(), &"2.1.251".parse().unwrap()).unwrap();
        assert!(output.contains("Claude 2.1.251"));
        assert!(output.contains("claude-2.1\tbaked"));
        assert!(output.contains(">=2.1.228, <2.2.0"));
        assert!(output.contains("InRange"));
    }

    #[test]
    fn keymap_show_prints_the_effective_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(show_keymap(dir.path(), "claude-2.1").unwrap(), BAKED);
        assert!(
            show_keymap(dir.path(), "missing")
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
    }

    #[test]
    fn keymap_add_installs_a_validated_user_file_and_remove_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("override.toml");
        std::fs::write(
            &input,
            BAKED.replace("after_paste = 400", "after_paste = 401"),
        )
        .unwrap();

        let added = add_keymap(dir.path(), &input).unwrap();
        let installed = keymap_dir(dir.path()).join("claude-2.1.toml");
        assert!(added.contains(&installed.display().to_string()));
        assert!(installed.is_file());
        let output = list_output(dir.path(), &"2.1.251".parse().unwrap()).unwrap();
        assert!(output.contains("user:"));
        assert!(
            show_keymap(dir.path(), "claude-2.1")
                .unwrap()
                .contains("after_paste = 401")
        );

        assert_eq!(
            remove_keymap(dir.path(), "claude-2.1").unwrap(),
            "Removed keymap claude-2.1"
        );
        assert!(!installed.exists());
    }

    #[test]
    fn keymap_add_rejects_malformed_and_hand_verified_files() {
        let dir = tempfile::tempdir().unwrap();
        let malformed = dir.path().join("malformed.toml");
        std::fs::write(&malformed, "name = [").unwrap();
        let malformed_error = add_keymap(dir.path(), &malformed).unwrap_err().to_string();
        assert!(malformed_error.contains("Parse"));
        assert!(malformed_error.contains("malformed.toml"));

        let hand_verified = dir.path().join("hand-verified.toml");
        let claimed = BAKED.replacen(
            "verified = []",
            "verified = [{ version = \"2.1.251\", run_id = \"manual\", spec = \"prompt\" }]",
            1,
        );
        std::fs::write(&hand_verified, claimed).unwrap();
        let error = add_keymap(dir.path(), &hand_verified)
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
