//! The amux binary. Its hidden subcommands are what an agent's harness
//! runs from the canonical install path: the agent process itself, the
//! tool server its provider launches, and the hook command terminal Claude
//! runs for each hook event.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "amux", version, about = "Agent multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Host one agent: the directory holds its lock, specs and sockets.
    #[command(hide = true)]
    Agent { dir: PathBuf },
    /// Serve amux's tools over stdio to the agent in the directory.
    #[command(hide = true)]
    Mcp { dir: PathBuf },
    /// Hand a provider's hook payload on stdin to its agent.
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        provider: Hooks,
    },
}

#[derive(Debug, Subcommand)]
enum Hooks {
    Claude,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Agent { dir } => ExitCode::from(agent::main(dir).clamp(0, 255) as u8),
        Command::Mcp { dir } => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("amux mcp: {error}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(agent::serve_tools(dir)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("amux mcp: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Hooks {
            provider: Hooks::Claude,
        } => {
            // Claude reads a hook's exit code as a verdict (2 blocks the
            // action), so a payload that cannot be delivered is reported
            // and the hook still succeeds.
            if let Err(error) = claude_hook() {
                eprintln!("amux hooks claude: {error}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// Forwards the payload on stdin to the agent's hook socket, which the
/// agent names in the environment it gives Claude.
fn claude_hook() -> std::io::Result<()> {
    let socket = std::env::var_os(claude::hooks::HOOK_SOCKET_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::other(format!("{} is not set", claude::hooks::HOOK_SOCKET_ENV))
        })?;
    let mut payload = Vec::new();
    std::io::stdin().read_to_end(&mut payload)?;
    claude::hooks::forward_from_env(&payload, &socket)
}
