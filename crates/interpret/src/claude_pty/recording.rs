//! Recorded Claude terminal sessions as interpreter events.
//!
//! Two formats:
//!
//! - `claude_pty_io`: a claude-specs provider recording (`io.jsonl`). Each
//!   line is `{dir, line, transport_id, us}`; hook lines carry the payload,
//!   transcript lines `{path, row}`, and PTY bytes are skipped. Time is
//!   microseconds since the recording began, placed on the wall clock the
//!   transcript's own timestamps use.
//! - `transcript_rows`: one transcript row per line, hook payloads mixed in
//!   (they carry `hook_event_name`); time comes from row timestamps.
//!
//! Every fact is preceded by a tick when the clock moved.

use serde_json::Value;

use crate::{Channel, Event, Fact};

/// Written into transcripts by an earlier tailer; not Claude's.
const TAILER_MARKER: &str = "amux.transcript_ready";

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match format {
        "claude_pty_io" => Ok(io(&lines)),
        "transcript_rows" => Ok(rows(&lines)),
        other => Err(format!(
            "claude_pty reads claude_pty_io and transcript_rows, not {other:?}"
        )),
    }
}

fn timestamp_ms(row: &Value) -> Option<i64> {
    let stamp = row.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.timestamp_millis())
}

struct Clock {
    events: Vec<Event>,
    now: Option<i64>,
}

impl Clock {
    fn push(&mut self, at_ms: Option<i64>, channel: Channel, payload: Vec<u8>) {
        if let Some(at_ms) = at_ms
            && self.now.is_none_or(|now| at_ms > now)
        {
            self.now = Some(at_ms);
            self.events.push(Event::Tick { at_ms });
        }
        self.events.push(Event::Fact(Fact { channel, payload }));
    }
}

fn io(lines: &[Value]) -> Vec<Event> {
    let us = |line: &Value| line.get("us").and_then(Value::as_i64).unwrap_or(0);
    let parsed = |line: &Value| {
        line.get("line")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
    };
    let transport = |line: &Value| {
        line.get("transport_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    // Where the recording's zero sits on the wall clock: the first row
    // with a timestamp, less its offset.
    let origin = lines
        .iter()
        .filter(|line| transport(line) == "transcript")
        .find_map(|line| {
            let row = parsed(line)?.get("row").cloned()?;
            Some(timestamp_ms(&row)? - us(line) / 1000)
        })
        .unwrap_or(0);
    let mut clock = Clock {
        events: Vec::new(),
        now: None,
    };
    for line in lines {
        let at_ms = Some(origin + us(line) / 1000);
        match transport(line).as_str() {
            "hook" => {
                let Some(payload) = line.get("line").and_then(Value::as_str) else {
                    continue;
                };
                clock.push(at_ms, Channel::Hook, payload.as_bytes().to_vec());
            }
            "transcript" => {
                let Some(row) = parsed(line).and_then(|wrapped| wrapped.get("row").cloned()) else {
                    continue;
                };
                if row.get("type").and_then(Value::as_str) == Some(TAILER_MARKER) {
                    continue;
                }
                clock.push(at_ms, Channel::Transcript, row.to_string().into_bytes());
            }
            _ => {}
        }
    }
    clock.events
}

fn rows(lines: &[Value]) -> Vec<Event> {
    let mut clock = Clock {
        events: Vec::new(),
        now: None,
    };
    for row in lines {
        if row.get("type").and_then(Value::as_str) == Some(TAILER_MARKER) {
            continue;
        }
        let channel = if row.get("hook_event_name").is_some() {
            Channel::Hook
        } else {
            Channel::Transcript
        };
        clock.push(timestamp_ms(row), channel, row.to_string().into_bytes());
    }
    clock.events
}
