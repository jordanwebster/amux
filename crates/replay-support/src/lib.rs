pub mod matcher;
pub mod spec;
pub mod transport;

pub use matcher::match_value;
pub use spec::{Postconditions, Setup, Spec, Step};
pub use transport::{
    ReplayAdvance, ReplayClock, ReplayController, ReplayOptions, ReplayPeek, ReplayTiming,
    replay_transport, replay_transport_with_controller, replay_transport_with_options,
};

use std::path::Path;

/// A single IO event from a recorded session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoEvent {
    pub us: u64,
    pub direction: IoDirection,
    pub line: String,
    pub transport_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoDirection {
    Write,
    Read,
}

/// Load an `io.jsonl` file into a timestamped event list.
pub fn load_script(path: impl AsRef<Path>) -> Vec<IoEvent> {
    let path = path.as_ref();
    let display = path.display().to_string();
    let content =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {display}: {e}"));
    let mut events = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("bad io.jsonl line: {e}\n  line: {line}"));
        let us = value.get("us").and_then(|v| v.as_u64()).unwrap_or(0);
        let dir = value.get("dir").and_then(|v| v.as_str()).unwrap_or("");
        let payload = value
            .get("line")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let direction = match dir {
            "stdin" => IoDirection::Write,
            "stdout" => IoDirection::Read,
            _ => continue,
        };
        let transport_id = value
            .get("transport_id")
            .or_else(|| value.get("spawn_id"))
            .or_else(|| value.get("process_id"))
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let session_id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        events.push(IoEvent {
            us,
            direction,
            line: payload,
            transport_id,
            session_id,
        });
    }
    events
}
