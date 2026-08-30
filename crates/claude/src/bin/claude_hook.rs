use std::io::Read;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::var_os("CLAUDE_HOOK_SOCKET")
        .map(PathBuf::from)
        .ok_or("CLAUDE_HOOK_SOCKET is not set")?;
    let mut payload = Vec::new();
    std::io::stdin().read_to_end(&mut payload)?;
    claude::hooks::forward(&payload, &socket)?;
    Ok(())
}
