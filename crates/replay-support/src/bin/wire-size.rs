//! `wire-size RECORDING`: replays a headless Claude `io.jsonl` recording
//! through the interpreter and prints, as JSON, how many snapshots it
//! journaled, the largest and median snapshot's encoded size, and per tool
//! call the bytes its items and appends took.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(recording), None) = (args.next().map(PathBuf::from), args.next()) else {
        eprintln!("usage: wire-size RECORDING");
        return ExitCode::from(2);
    };
    match replay_support::wire_size::wire_size(&recording) {
        Ok(size) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&size).expect("sizes serialize")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("wire-size: {}: {error}", recording.display());
            ExitCode::FAILURE
        }
    }
}
