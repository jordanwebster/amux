use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use claude::specs::{SpecSource, execute, fixtures_root, sdk_registry};
use replay_support::{
    Manifest, ProbeAttempt, ProbeRun, Redaction, SourceKind, SpecEntry, append_verification,
    load_recording, migrate_legacy_manifest, probe, registry_rows, sanitize,
};
use semver::Version;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CLAUDE_CAPTURE_PROXY").is_some() {
        return run_capture_proxy().await;
    }
    if std::env::var_os("CLAUDE_SPEC_MCP_SERVER").is_some()
        || std::env::current_exe()?
            .file_stem()
            .is_some_and(|name| name == "spec-mcp-server")
    {
        run_spec_mcp_server();
        return Ok(());
    }

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "list" => list(),
        [command, flag] if command == "list" && flag == "--sdk" => list(),
        [command, rest @ ..] if command == "record" => record_command(rest).await,
        [command, rest @ ..] if command == "probe" => probe_command(rest).await,
        _ => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage: claude-probe list [--sdk] | record --sdk <spec>... | probe --sdk [--out <dir>]"
}

fn list() -> Result<(), Box<dyn std::error::Error>> {
    for row in registry_rows(&fixtures_root(), sdk_registry())? {
        let ledger = row
            .verified
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "driver=sdk spec={} recording={} recorded={} verified=[{}]",
            row.spec, row.recording, row.recorded, ledger
        );
    }
    Ok(())
}

async fn record_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let names = parse_sdk_names(args)?;
    if names.is_empty() {
        return Err(usage().into());
    }
    for name in names {
        let entry = find_entry(name)?;
        record_one(entry, &fixtures_root()).await?;
        println!("driver=sdk spec={} outcome=recorded", entry.name);
    }
    Ok(())
}

async fn probe_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut sdk = false;
    let mut out = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--sdk" => sdk = true,
            "--out" if index + 1 < args.len() => {
                index += 1;
                out = Some(PathBuf::from(&args[index]));
            }
            _ => return Err(usage().into()),
        }
        index += 1;
    }
    if !sdk {
        return Err("probe requires --sdk until the PTY registry lands".into());
    }
    run_probe(out.unwrap_or_else(default_probe_dir)).await
}

fn parse_sdk_names(args: &[String]) -> Result<Vec<&str>, Box<dyn std::error::Error>> {
    let mut sdk = false;
    let mut names = Vec::new();
    for arg in args {
        if arg == "--sdk" {
            sdk = true;
        } else if arg.starts_with('-') {
            return Err(usage().into());
        } else {
            names.push(arg.as_str());
        }
    }
    if !sdk {
        return Err("record requires --sdk".into());
    }
    Ok(names)
}

fn find_entry(name: &str) -> Result<SpecEntry, Box<dyn std::error::Error>> {
    sdk_registry()
        .iter()
        .copied()
        .find(|entry| entry.name == name || entry.recording == name)
        .ok_or_else(|| format!("unknown Claude SDK specification {name}").into())
}

async fn run_probe(out: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let binary = claude_binary();
    let version = claude::version::probe_version(&binary).await?.0;
    let fixtures = Arc::new(fixtures_root());
    let live_fixtures = Arc::clone(&fixtures);
    let pass_fixtures = Arc::clone(&fixtures);
    let record_fixtures = Arc::clone(&fixtures);
    let mut run = ProbeRun {
        run_id: Uuid::new_v4().to_string(),
        provider: "claude".to_string(),
        version,
        dir: out,
        results: Vec::new(),
    };

    let (probe_path, drift_path) = probe(
        &mut run,
        sdk_registry(),
        move |entry| {
            let fixtures = Arc::clone(&live_fixtures);
            async move {
                let recording =
                    load_recording(&fixtures.join(entry.recording)).map_err(io::Error::other)?;
                match capture(entry).await {
                    Ok(capture) => Ok(ProbeAttempt {
                        claim: Ok(()),
                        recorded: recording.manifest.observed,
                        live: replay_support::observe(&capture.events),
                        raw_payloads: 0,
                    }),
                    Err(error) => Ok(ProbeAttempt {
                        claim: Err(error.to_string()),
                        recorded: recording.manifest.observed,
                        live: replay_support::Observed::default(),
                        raw_payloads: 0,
                    }),
                }
            }
        },
        move |entry, verification| {
            let fixtures = Arc::clone(&pass_fixtures);
            async move {
                append_verification(&fixtures.join(entry.recording), verification)
                    .map_err(io::Error::other)
            }
        },
        move |entry| {
            let fixtures = Arc::clone(&record_fixtures);
            async move {
                record_one(entry, &fixtures)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))
            }
        },
    )
    .await?;

    for result in &run.results {
        println!(
            "driver=sdk spec={} outcome={:?}",
            result.spec, result.outcome
        );
    }
    println!("probe={}", probe_path.display());
    println!("drift={}", drift_path.display());
    println!("keymap=not-verified reason=sdk-only-probe");
    Ok(())
}

struct Capture {
    report: claude::specs::RunReport,
    version: Version,
    events: Vec<replay_support::IoEvent>,
    spawn: Vec<u8>,
    scratch: PathBuf,
}

async fn capture(entry: SpecEntry) -> Result<Capture, Box<dyn std::error::Error>> {
    let binary = claude_binary();
    let version = claude::version::probe_version(&binary).await?.0;
    let scratch = tempfile::Builder::new()
        .prefix("claude-sdk-spec-")
        .tempdir()?;
    let work = scratch.path().join("work");
    let capture_dir = scratch.path().join("capture");
    let helpers = scratch.path().join("bin");
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(&capture_dir)?;
    std::fs::create_dir_all(&helpers)?;
    install_mcp_helper(&helpers)?;

    let old_path = std::env::var_os("PATH");
    let mut paths = vec![helpers];
    paths.extend(std::env::split_paths(old_path.as_deref().unwrap_or_default()));
    let helper_path = std::env::join_paths(paths)?;
    let proxy = std::env::current_exe()?;
    // Capture is deliberately serial: these variables are inherited by the
    // one specification's subprocesses and restored before the next starts.
    unsafe {
        std::env::set_var("PATH", &helper_path);
        std::env::set_var("CLAUDE_CAPTURE_PROXY", "1");
        std::env::set_var("CLAUDE_CAPTURE_DIR", &capture_dir);
        std::env::set_var("CLAUDE_REAL_PATH", &binary);
    }
    let outcome = execute(
        &entry,
        SpecSource::Live {
            binary: proxy,
            cwd: work,
            environment: None,
        },
    )
    .await;
    unsafe {
        std::env::remove_var("CLAUDE_CAPTURE_PROXY");
        std::env::remove_var("CLAUDE_CAPTURE_DIR");
        std::env::remove_var("CLAUDE_REAL_PATH");
        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
    let report = outcome?;
    let events = replay_support::load_script(capture_dir.join("io.jsonl"));
    let spawn = std::fs::read(capture_dir.join("spawn.jsonl"))?;
    Ok(Capture {
        report,
        version,
        events,
        spawn,
        scratch: scratch.path().to_path_buf(),
    })
}

async fn record_one(
    entry: SpecEntry,
    root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut capture = capture(entry).await?;
    let redaction = sanitize(
        &mut capture.events,
        &Redaction {
            home: owner_home(),
            extra_paths: vec![capture.scratch.clone()],
            secret_env: secret_values(),
            personal_identifier_keys: Vec::new(),
        },
    );
    let stage = root.join(format!(".{}.{}", entry.recording, Uuid::new_v4()));
    std::fs::create_dir_all(&stage)?;
    write_events(&stage.join("io.jsonl"), &capture.events)?;
    let sanitized_spawn = sanitize_json_lines(&capture.spawn, &capture.scratch)?;
    std::fs::write(stage.join("spawn.jsonl"), sanitized_spawn)?;

    let legacy = serde_json::json!({
        "schema_version": 1,
        "spec": entry.name,
        "claude_code_version": capture.version.to_string(),
        "model": capture.report.model,
        "recorded_at": Utc::now().to_rfc3339(),
        "session_ids": capture.report.session_ids,
    });
    let mut manifest = migrate_legacy_manifest(&legacy, "claude", &stage)?;
    manifest.recorded.source_kind = SourceKind::LiveCapture;
    manifest.observed = replay_support::observe(&capture.events);
    manifest.redaction = redaction;
    write_manifest(&stage.join("manifest.json"), &manifest)?;

    let destination = root.join(entry.recording);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(stage, destination)?;
    Ok(())
}

fn write_events(path: &Path, events: &[replay_support::IoEvent]) -> io::Result<()> {
    let mut output = String::new();
    for event in events {
        let mut row = serde_json::json!({
            "us": event.us,
            "dir": match event.direction {
                replay_support::IoDirection::Write => "stdin",
                replay_support::IoDirection::Read => "stdout",
            },
            "line": event.line,
        });
        if let Some(transport_id) = &event.transport_id {
            row["transport_id"] = serde_json::Value::String(transport_id.clone());
        }
        if let Some(session_id) = &event.session_id {
            row["session_id"] = serde_json::Value::String(session_id.clone());
        }
        output.push_str(&serde_json::to_string(&row).map_err(io::Error::other)?);
        output.push('\n');
    }
    std::fs::write(path, output)
}

fn write_manifest(path: &Path, manifest: &Manifest) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

fn sanitize_json_lines(bytes: &[u8], scratch: &Path) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let home = owner_home().to_string_lossy().into_owned();
    let scratch = scratch.to_string_lossy().into_owned();
    let secrets = secret_values();
    let mut output = String::new();
    for line in text.lines() {
        let mut value: serde_json::Value = serde_json::from_str(line).map_err(io::Error::other)?;
        redact_value(&mut value, &home, &scratch, &secrets);
        output.push_str(&serde_json::to_string(&value).map_err(io::Error::other)?);
        output.push('\n');
    }
    Ok(output.into_bytes())
}

fn redact_value(value: &mut serde_json::Value, home: &str, scratch: &str, secrets: &[String]) {
    match value {
        serde_json::Value::String(text) => {
            *text = text.replace(scratch, "<MACHINE_PATH>");
            if !home.is_empty() {
                *text = text.replace(home, "<HOME>");
            }
            for secret in secrets {
                if !secret.is_empty() {
                    *text = text.replace(secret, "<REDACTED>");
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_value(value, home, scratch, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, home, scratch, secrets);
            }
        }
        _ => {}
    }
}

async fn run_capture_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let capture_dir = PathBuf::from(std::env::var("CLAUDE_CAPTURE_DIR")?);
    let real = std::env::var_os("CLAUDE_REAL_PATH").unwrap_or_else(|| "claude".into());
    std::fs::create_dir_all(&capture_dir)?;
    let start = Instant::now();
    let transport_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    append_json(
        &capture_dir.join("spawn.jsonl"),
        &serde_json::json!({
            "us": 0,
            "transport_id": transport_id,
            "argv": args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        }),
    )?;
    let mut child = tokio::process::Command::new(real)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let child_stdin = child.stdin.take().ok_or("child stdin unavailable")?;
    let child_stdout = child.stdout.take().ok_or("child stdout unavailable")?;
    let mut child_stderr = child.stderr.take().ok_or("child stderr unavailable")?;
    let (io_tx, mut io_rx) = mpsc::channel::<String>(1024);
    let io_path = capture_dir.join("io.jsonl");
    let io_writer = tokio::spawn(async move {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(io_path)
            .await?;
        while let Some(line) = io_rx.recv().await {
            file.write_all(line.as_bytes()).await?;
            file.write_all(b"\n").await?;
        }
        file.flush().await
    });
    let stdin_tx = io_tx.clone();
    let stdin_transport = transport_id.clone();
    let stdin_task = tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        let mut child_input = child_stdin;
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line).await? == 0 {
                break;
            }
            let content = line.trim_end_matches('\n');
            let row = io_row(start.elapsed().as_micros() as u64, "stdin", content, &stdin_transport);
            stdin_tx.send(row.to_string()).await.map_err(io::Error::other)?;
            child_input.write_all(line.as_bytes()).await?;
            child_input.flush().await?;
        }
        Ok::<(), io::Error>(())
    });
    let stdout_tx = io_tx.clone();
    let stdout_transport = transport_id;
    let stdout_task = tokio::spawn(async move {
        let mut child_output = BufReader::new(child_stdout);
        let mut output = tokio::io::stdout();
        let mut line = String::new();
        loop {
            line.clear();
            if child_output.read_line(&mut line).await? == 0 {
                break;
            }
            let content = line.trim_end_matches('\n');
            let row = io_row(start.elapsed().as_micros() as u64, "stdout", content, &stdout_transport);
            stdout_tx.send(row.to_string()).await.map_err(io::Error::other)?;
            output.write_all(line.as_bytes()).await?;
            output.flush().await?;
        }
        Ok::<(), io::Error>(())
    });
    let stderr_task = tokio::spawn(async move {
        let mut stderr = tokio::io::stderr();
        tokio::io::copy(&mut child_stderr, &mut stderr).await
    });
    drop(io_tx);
    let status = child.wait().await?;
    stdin_task.await??;
    stdout_task.await??;
    let _ = stderr_task.await?;
    io_writer.await??;
    if !status.success() {
        return Err(format!("Claude exited with {status}").into());
    }
    Ok(())
}

fn io_row(us: u64, direction: &str, line: &str, transport: &str) -> serde_json::Value {
    let mut row = serde_json::json!({
        "us": us,
        "dir": direction,
        "line": line,
        "transport_id": transport,
    });
    if let Some(session_id) = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("session_id").and_then(serde_json::Value::as_str).map(str::to_owned))
    {
        row["session_id"] = serde_json::Value::String(session_id);
    }
    row
}

fn append_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{value}")
}

fn install_mcp_helper(dir: &Path) -> io::Result<()> {
    let destination = dir.join("spec-mcp-server");
    #[cfg(unix)]
    std::os::unix::fs::symlink(std::env::current_exe()?, destination)?;
    #[cfg(not(unix))]
    std::fs::copy(std::env::current_exe()?, destination)?;
    Ok(())
}

fn run_spec_mcp_server() {
    const PROTOCOL_VERSION: &str = "2025-11-25";
    const TOOL: &str = "ask_the_operator";
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = std::io::stdout();
    let mut outgoing_id = 1000i64;
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let method = message.get("method").and_then(serde_json::Value::as_str);
        let id = message.get("id").cloned();
        let result = match (method, &id) {
            (Some(_), None) => continue,
            (Some("initialize"), Some(_)) => serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "spec-external", "version": "1.0.0"},
            }),
            (Some("tools/list"), Some(_)) => serde_json::json!({
                "tools": [{
                    "name": TOOL,
                    "description": "Ask the operator to confirm a word, then return it.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"word": {"type": "string"}},
                        "required": ["word"],
                    },
                }],
            }),
            (Some("tools/call"), Some(_)) => {
                let word = message.pointer("/params/arguments/word")
                    .and_then(serde_json::Value::as_str).unwrap_or("nothing").to_owned();
                outgoing_id += 1;
                let answer = elicit(&mut stdout, &mut lines, outgoing_id, &word);
                serde_json::json!({"content": [{"type": "text", "text": answer}]})
            }
            _ => {
                respond(&mut stdout, serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "method not found"},
                }));
                continue;
            }
        };
        respond(&mut stdout, serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
}

fn elicit(
    stdout: &mut std::io::Stdout,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    id: i64,
    word: &str,
) -> String {
    respond(stdout, serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "elicitation/create",
        "params": {
            "message": format!("Confirm the word {word}."),
            "requestedSchema": {
                "type": "object",
                "properties": {"confirmed": {"type": "string"}},
                "required": ["confirmed"],
            },
        },
    }));
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if message.get("id").and_then(serde_json::Value::as_i64) != Some(id) {
            continue;
        }
        return match message.pointer("/result/action").and_then(serde_json::Value::as_str) {
            Some("accept") => message.pointer("/result/content/confirmed")
                .and_then(serde_json::Value::as_str).unwrap_or(word).to_owned(),
            other => format!("elicitation {}", other.unwrap_or("unanswered")),
        };
    }
    "elicitation abandoned".to_owned()
}

fn respond(stdout: &mut std::io::Stdout, message: serde_json::Value) {
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn claude_binary() -> PathBuf {
    std::env::var_os("CLAUDE_REAL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"))
}

fn owner_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn secret_values() -> Vec<String> {
    ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}

fn default_probe_dir() -> PathBuf {
    Path::new("target")
        .join("claude-probe")
        .join(Uuid::new_v4().to_string())
}
