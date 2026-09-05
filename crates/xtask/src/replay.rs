//! Rebuilding a recorded screen on the Mac.
//!
//! A report bundle from a phone holds the shared runtime's own recording and
//! the view-state trace beside it. This hands both to a debug build on the
//! pinned simulator, which folds the first into stores and applies the second,
//! then photographs what came back and compares it with the picture the bundle
//! was written with. A bundle that no longer replays to its own screen is a
//! projection or a view that has changed under it, and this is where that is
//! noticed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::door;
use crate::golden::{GoldenVerdict, diff};

/// What the bundle was written with, and what a replay of it is compared with.
const SCREEN: &str = "screen.png";
/// What the bundle's recording rebuilds: its fleet, its conversations and
/// whether it had been confirmed by a host. Pinned beside the picture because
/// a screen that does not draw the fleet yet would look identical whether the
/// recording rebuilt it or nothing at all.
const REBUILT: &str = "replayed.json";
const MESSAGES: &str = "msgs.jsonl";
const TRACE: &str = "trace.jsonl";
const OUT: &str = "target/ios/replay";
/// The same latitude a golden run gives a capture of the same screen.
const TOLERANCE: u8 = 2;
const MAX_DIFFERING_PIXELS: u64 = 64;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(2).collect();
    let mut simulator = "amux-golden".to_string();
    let mut bundle_id = "sh.amux.Amux".to_string();
    let mut install: Option<PathBuf> = None;
    let mut update = false;
    let mut bundle: Option<PathBuf> = None;

    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--simulator" => simulator = arguments.next().unwrap_or_default(),
            "--bundle-id" => bundle_id = arguments.next().unwrap_or_default(),
            "--install" => install = arguments.next().map(PathBuf::from),
            // Writes the screen the bundle replays to, for a bundle that has
            // never had one. The commit message says why it was rewritten.
            "--update" => update = true,
            other => bundle = Some(PathBuf::from(other)),
        }
    }
    let bundle = bundle.ok_or("a bundle directory to replay")?;
    for part in [MESSAGES, TRACE] {
        if !bundle.join(part).is_file() {
            return Err(format!("{} has no {part}", bundle.display()).into());
        }
    }

    if let Some(application) = install {
        door::install(&door::simulator_udid(&simulator)?, &application)?;
    }

    let out = Path::new(OUT);
    std::fs::create_dir_all(out)?;
    let taken = out.join(SCREEN);
    std::fs::remove_file(&taken).ok();
    let replies = door::door(
        &simulator,
        &bundle_id,
        vec![
            json!({"kind": "replay", "path": bundle.to_string_lossy()}),
            json!({"kind": "settle"}),
            json!({"kind": "capture", "path": taken.to_string_lossy()}),
            json!({"kind": "shutdown"}),
        ],
        Duration::from_secs(300),
    )?;
    let replayed = replies
        .first()
        .and_then(|reply| reply.get("replayed"))
        .ok_or_else(|| format!("the door did not replay the bundle: {replies:?}"))?;
    describe(&bundle, replayed);

    let screen = bundle.join(SCREEN);
    let rebuilt = bundle.join(REBUILT);
    if update {
        std::fs::copy(&taken, &screen)?;
        std::fs::write(&rebuilt, serde_json::to_string_pretty(replayed)? + "\n")?;
        println!("wrote {} and {}", screen.display(), rebuilt.display());
        return Ok(());
    }
    if rebuilt.is_file() {
        let pinned: Value = serde_json::from_str(&std::fs::read_to_string(&rebuilt)?)?;
        if &pinned != replayed {
            return Err(format!(
                "the bundle no longer rebuilds what it recorded.\n{}: {}\nreplayed: {}",
                rebuilt.display(),
                serde_json::to_string(&pinned)?,
                serde_json::to_string(replayed)?
            )
            .into());
        }
        println!("{}: rebuilt what it recorded", rebuilt.display());
    }
    if !screen.is_file() {
        return Err(format!(
            "{} has no {SCREEN} to compare the replay with; `--update` writes one",
            bundle.display()
        )
        .into());
    }
    let verdict = diff(
        &screen,
        &taken,
        &out.join("screen"),
        TOLERANCE,
        MAX_DIFFERING_PIXELS,
    )?;
    println!("{}: {verdict}", screen.display());
    match verdict {
        GoldenVerdict::Same => Ok(()),
        other => Err(format!(
            "the replayed screen is not the one the bundle was written with: {other}; \
             expected, actual and difference under {}",
            out.join("screen").display()
        )
        .into()),
    }
}

/// What came out of the recording, in the words the bundle would be discussed
/// in: which agents, which machines, how much transcript, and where the trace
/// left the screen.
fn describe(bundle: &Path, replayed: &Value) {
    let names = |key: &str| {
        replayed
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let number = |key: &str| {
        replayed
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or_default()
    };
    let entries: u64 = replayed
        .get("entries")
        .and_then(Value::as_object)
        .map(|rows| rows.values().filter_map(Value::as_u64).sum())
        .unwrap_or_default();
    println!(
        "{}: {} projected events rebuilt {} agents and {} machines with {entries} transcript \
         entries, reconciled {}",
        bundle.display(),
        number("events"),
        replayed
            .get("agents")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        replayed
            .get("hosts")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        replayed
            .get("reconciled")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
    );
    if !names("agents").is_empty() {
        println!("  agents: {}", names("agents"));
    }
    if !names("hosts").is_empty() {
        println!("  machines: {}", names("hosts"));
    }
    println!(
        "  {} trace events left {} showing",
        number("trace"),
        replayed
            .get("screen")
            .and_then(Value::as_str)
            .unwrap_or("none"),
    );
}
