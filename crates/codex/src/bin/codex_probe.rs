use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use codex::specs::{CAPTURE_MODEL, SpecSource, execute, fixtures_root, live_io_path, registry};
use replay_support::{
    Manifest, ProbeAttempt, ProbeRun, Redaction, SourceKind, SpecEntry, append_verification,
    load_recording, migrate_legacy_manifest, probe, registry_rows, sanitize,
};
use semver::Version;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "list" => list(),
        [command, names @ ..] if command == "record" && !names.is_empty() => {
            record_selected(names).await
        }
        [command] if command == "probe" => run_probe(default_probe_dir()).await,
        [command, flag, out] if command == "probe" && flag == "--out" => {
            run_probe(PathBuf::from(out)).await
        }
        _ => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage: codex-probe list | record <spec>... | probe [--out <dir>]"
}

fn list() -> Result<(), Box<dyn std::error::Error>> {
    for row in registry_rows(&fixtures_root(), registry())? {
        let ledger = row
            .verified
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "spec={} recording={} recorded={} verified=[{}]",
            row.spec, row.recording, row.recorded, ledger
        );
    }
    Ok(())
}

async fn record_selected(names: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    for name in names {
        let entry = registry()
            .iter()
            .copied()
            .find(|entry| entry.name == name)
            .ok_or_else(|| format!("unknown Codex specification {name}; model={CAPTURE_MODEL}"))?;
        record_one(entry, &fixtures_root()).await?;
        println!("recorded {} model={CAPTURE_MODEL}", entry.name);
    }
    Ok(())
}

async fn run_probe(out: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let version = installed_version()?;
    let run_id = Uuid::new_v4().to_string();
    let fixtures = Arc::new(fixtures_root());
    let live_fixtures = Arc::clone(&fixtures);
    let pass_fixtures = Arc::clone(&fixtures);
    let record_fixtures = Arc::clone(&fixtures);
    let mut run = ProbeRun {
        run_id,
        provider: "codex".to_string(),
        version,
        dir: out,
        results: Vec::new(),
    };

    let (probe_path, drift_path) = probe(
        &mut run,
        registry(),
        move |entry| {
            let fixtures = Arc::clone(&live_fixtures);
            async move {
                let recording =
                    load_recording(&fixtures.join(entry.recording)).map_err(io::Error::other)?;
                match live_attempt(entry).await {
                    Ok(report) => Ok(ProbeAttempt {
                        claim: Ok(()),
                        recorded: recording.manifest.observed,
                        live: report.observed,
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
        println!("spec={} outcome={:?}", result.spec, result.outcome);
    }
    println!("probe={}", probe_path.display());
    println!("drift={}", drift_path.display());
    Ok(())
}

async fn live_attempt(
    entry: SpecEntry,
) -> Result<codex::specs::RunReport, Box<dyn std::error::Error>> {
    let scratch = isolated_home()?;
    execute(
        &entry,
        SpecSource::Live {
            codex_home: scratch.path().join("codex-home"),
            model: CAPTURE_MODEL.to_string(),
        },
    )
    .await
    .map_err(Into::into)
}

async fn record_one(entry: SpecEntry, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = isolated_home()?;
    let codex_home = scratch.path().join("codex-home");
    let report = execute(
        &entry,
        SpecSource::Live {
            codex_home: codex_home.clone(),
            model: CAPTURE_MODEL.to_string(),
        },
    )
    .await
    .map_err(|error| {
        format!(
            "{}; Codex rejected or failed capture model {}",
            error, CAPTURE_MODEL
        )
    })?;

    let source_io = live_io_path(&codex_home);
    let mut events = replay_support::load_script(&source_io);
    let redaction = sanitize(
        &mut events,
        &Redaction {
            home: owner_home(),
            extra_paths: vec![scratch.path().to_path_buf()],
            secret_env: secret_values(),
        },
    );
    let destination = root.join(entry.recording);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::create_dir_all(&destination)?;
    write_events(&destination.join("io.jsonl"), &events)?;
    write_spawn(&destination.join("spawn.jsonl"))?;

    let provider_version = report.provider_version.unwrap_or(installed_version()?);
    if report.server_model.is_empty() {
        return Err(format!(
            "{} did not report a server model for requested model {}",
            entry.name, CAPTURE_MODEL
        )
        .into());
    }
    let legacy = serde_json::json!({
        "schema_version": 1,
        "spec": entry.name,
        "codex_version": provider_version.to_string(),
        "model": report.server_model,
        "recorded_at": Utc::now().to_rfc3339(),
    });
    let mut manifest = migrate_legacy_manifest(&legacy, "codex", &destination)?;
    manifest.recorded.source_kind = SourceKind::LiveCapture;
    manifest.observed = replay_support::observe(&events);
    manifest.redaction = redaction;
    manifest.session_ids = report.session_ids;
    manifest.provider_extra.insert(
        "capture_model".to_string(),
        serde_json::Value::String(CAPTURE_MODEL.to_string()),
    );
    write_manifest(&destination.join("manifest.json"), &manifest)?;
    Ok(())
}

fn isolated_home() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let scratch = tempfile::Builder::new().prefix("codex-spec-").tempdir()?;
    let codex_home = scratch.path().join("codex-home");
    let project = scratch.path().join("project");
    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&project)?;

    let source_auth = owner_codex_home().join("auth.json");
    if !source_auth.is_file() {
        return Err(format!(
            "Codex authentication is unavailable at {}; run `codex login` first",
            source_auth.display()
        )
        .into());
    }
    std::fs::copy(&source_auth, codex_home.join("auth.json"))?;
    std::fs::write(codex_home.join(".personality_migration"), "v1\n")?;
    std::fs::write(codex_home.join(".sandbox_migration"), "v1\n")?;
    std::fs::write(
        codex_home.join("installation_id"),
        format!("{}\n", Uuid::new_v4()),
    )?;
    let project_key = serde_json::to_string(&project.to_string_lossy())?;
    std::fs::write(
        codex_home.join("config.toml"),
        format!("[projects.{project_key}]\ntrust_level = \"trusted\"\n"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&codex_home, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(
            codex_home.join("auth.json"),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    Ok(scratch)
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

fn write_spawn(path: &Path) -> io::Result<()> {
    let row = serde_json::json!({
        "command": "codex",
        "args": ["--model", CAPTURE_MODEL, "app-server", "--listen", "stdio://"],
        "model": CAPTURE_MODEL,
    });
    std::fs::write(path, format!("{}\n", row))
}

fn write_manifest(path: &Path, manifest: &Manifest) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

fn installed_version() -> Result<Version, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("codex")
        .arg("--version")
        .output()?;
    if !output.status.success() {
        return Err(format!("codex --version exited with {}", output.status).into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    stdout
        .split_whitespace()
        .find_map(|part| Version::parse(part).ok())
        .ok_or_else(|| format!("could not parse Codex version from {stdout:?}").into())
}

fn owner_codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn owner_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn secret_values() -> Vec<String> {
    ["OPENAI_API_KEY", "CODEX_API_KEY"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}

fn default_probe_dir() -> PathBuf {
    Path::new("target")
        .join("codex-probe")
        .join(Uuid::new_v4().to_string())
}
