use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

const HOOKS_LOG_FILE: &str = "claude_hooks.jsonl";

/// Claude Code hook event data
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeHook {
    SessionStart(ClaudeSessionStart),
}

/// SessionStart hook data from Claude Code
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeSessionStart {
    pub cwd: String,
    pub session_id: String,
    pub source: String,
    pub transcript_path: String,
}

#[derive(Serialize)]
struct HookLogEntry {
    timestamp: u64,
    provider: String,
    event: String,
    data: ClaudeHook,
}

/// Handle Claude Code SessionStart hook.
/// Reads JSON from stdin, wraps with metadata, appends to claude_hooks.jsonl.
/// Fails silently (logs errors but returns 0) to not block Claude Code.
pub fn handle_claude_session_start() {
    if let Err(e) = handle_claude_session_start_inner() {
        log!("hooks: claude SessionStart error: {}", e);
    }
}

fn handle_claude_session_start_inner() -> io::Result<()> {
    // Read all input from stdin
    let stdin = io::stdin();
    let mut input = String::new();

    for line in stdin.lock().lines() {
        input.push_str(&line?);
    }

    // Parse into typed struct
    let data: ClaudeHook =
        serde_json::from_str(&input).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Create log entry with metadata
    let entry = HookLogEntry {
        timestamp: get_unix_timestamp(),
        provider: "claude".to_string(),
        event: "SessionStart".to_string(),
        data,
    };

    // Append to JSONL file
    append_to_log(&entry)
}

fn get_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn append_to_log(entry: &HookLogEntry) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(HOOKS_LOG_FILE)?;

    let json =
        serde_json::to_string(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    writeln!(file, "{}", json)?;
    file.flush()?;

    Ok(())
}
