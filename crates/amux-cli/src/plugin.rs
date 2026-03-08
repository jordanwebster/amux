use amux::setup;

const PLUGIN_VERSION: u32 = 1;

/// Ensure the Claude Code amux plugin is installed and up to date
pub async fn ensure_plugin_installed() {
    let current_version = setup::claude_plugin_version();

    if current_version == Some(PLUGIN_VERSION) {
        return;
    }

    let is_update = current_version.is_some();

    if is_update {
        println!("Updating Claude Code amux plugin...");
    } else {
        println!("Installing Claude Code amux plugin...");
    }

    let result = if is_update {
        run_update().await
    } else {
        run_install().await
    };

    match result {
        Ok(()) => {
            if let Err(e) = setup::set_claude_plugin_version(PLUGIN_VERSION) {
                eprintln!("error: failed to save plugin state: {}", e);
                std::process::exit(1);
            }
            if is_update {
                println!("Updated.");
            } else {
                println!("Installed.");
            }
        }
        Err(e) => {
            eprintln!(
                "error: failed to {} Claude Code plugin: {}",
                if is_update { "update" } else { "install" },
                e
            );
            std::process::exit(1);
        }
    }
}

async fn run_install() -> Result<(), String> {
    run_claude_command(&["plugin", "marketplace", "add", "jordanwebster/amux"]).await?;
    run_claude_command(&["plugin", "install", "amux@amux", "--scope", "user"]).await
}

async fn run_update() -> Result<(), String> {
    run_claude_command(&["plugin", "marketplace", "update", "amux"]).await?;
    run_claude_command(&["plugin", "update", "amux@amux", "--scope", "user"]).await
}

async fn run_claude_command(args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new("claude")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("failed to run 'claude {}': {}", args.join(" "), e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "'claude {}' failed: {}",
            args.join(" "),
            stderr.trim()
        ))
    }
}
