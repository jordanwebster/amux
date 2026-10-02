//! A set served by `testnet serve` in a process of its own, driven through
//! the door. Its scratch root is a short directory under `/tmp` (daemons
//! bind Unix sockets under it, and those paths are limited to about a
//! hundred bytes), and its readiness and log are written under this
//! worktree's `target/tui-set/<name>/`, so sets served from parallel
//! worktrees never cross.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::set::{Beat, Set};

/// How long a settle waits for an agent to come to rest.
const PATIENCE: Duration = Duration::from_secs(60);

pub struct Served {
    process: Child,
    /// The door's address.
    pub control: String,
    pub readiness: Value,
    /// Where this set's files go: readiness, log, frames.
    pub out: PathBuf,
    scratch: PathBuf,
    /// The text each agent was last sent, so a settle waits for its turn.
    sent: std::collections::HashMap<String, String>,
}

impl Served {
    pub fn start(root: &Path, set: &Set) -> Result<Served> {
        let out = root.join("target/tui-set").join(&set.name);
        std::fs::create_dir_all(&out)?;
        let topology = out.join("topology.json");
        std::fs::write(&topology, set.raw_topology.get())?;
        let scratch = PathBuf::from(format!("/tmp/ts-{}", std::process::id()));
        std::fs::create_dir_all(&scratch)?;
        let log = std::fs::File::create(out.join("testnet.log"))?;
        let mut process = Command::new(root.join("target/debug/testnet"))
            .arg("serve")
            .arg(&topology)
            .current_dir(root)
            .env_remove("AMUX_CONFIG")
            .env_remove("AMUX_LOG")
            .env("TMPDIR", &scratch)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .context("starting testnet serve (built by `just tui-set`)")?;
        let stdout = process.stdout.take().expect("piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .context("reading the set's readiness")?;
        if line.trim().is_empty() {
            let _ = process.wait();
            bail!(
                "testnet exited before the set was ready; see {}",
                out.join("testnet.log").display()
            );
        }
        let readiness: Value = serde_json::from_str(&line)?;
        std::fs::write(
            out.join("door.json"),
            serde_json::to_vec_pretty(&readiness)?,
        )?;
        let control = readiness["control"]
            .as_str()
            .ok_or_else(|| anyhow!("readiness without a control address"))?
            .to_owned();
        // An agent's creation prompt is the first it was sent.
        let sent = set.topology["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|agent| {
                Some((
                    agent["name"].as_str()?.to_owned(),
                    agent["prompt"].as_str()?.to_owned(),
                ))
            })
            .collect();
        Ok(Served {
            process,
            control,
            readiness,
            out,
            scratch,
            sent,
        })
    }

    /// One door request; its value, or the door's error.
    pub fn request(&self, request: &Value) -> Result<Value> {
        request_at(&self.control, request)
    }

    /// The installation config of a host, which the client runs against.
    pub fn config(&self, host: &str) -> Result<PathBuf> {
        self.host(host)?["config"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("no config for host {host}"))
    }

    /// A host's work directory, where its agents start by default: the
    /// client runs there too, so a new agent never works in this checkout.
    pub fn work_dir(&self, host: &str) -> Result<PathBuf> {
        self.host(host)?;
        let root = self.readiness["root"]
            .as_str()
            .ok_or_else(|| anyhow!("readiness without a root"))?;
        Ok(Path::new(root).join(host).join("work"))
    }

    fn host(&self, host: &str) -> Result<&Value> {
        self.readiness["hosts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|h| h["name"] == host)
            .ok_or_else(|| anyhow!("the set has no host {host}"))
    }

    /// A declared agent's id, as raw bytes the client knows it by.
    pub fn agent_id(&self, name: &str) -> Option<Vec<u8>> {
        let agent = self.readiness["agents"]
            .as_array()?
            .iter()
            .find(|a| a["name"] == name)?;
        uuid_bytes(agent["id"].as_str()?)
    }

    /// A host's id, as raw bytes.
    pub fn host_id(&self, host: &str) -> Result<Vec<u8>> {
        self.host(host)?["host_id"]
            .as_str()
            .and_then(uuid_bytes)
            .ok_or_else(|| anyhow!("no id for host {host}"))
    }

    /// Plays `beats` in order.
    pub fn play(&mut self, set: &Set, beats: &[Beat]) -> Result<()> {
        for beat in beats {
            self.beat(set, beat)
                .with_context(|| format!("{}: beat {beat:?}", set.name))?;
        }
        Ok(())
    }

    fn beat(&mut self, set: &Set, beat: &Beat) -> Result<()> {
        match beat {
            Beat::Send { agent, text } => {
                self.request(&json!({ "Send": { "agent": agent, "text": text } }))?;
                self.sent.insert(agent.clone(), text.clone());
            }
            Beat::Settle(agent) => self.settle(set, agent)?,
            Beat::WaitMs(ms) => std::thread::sleep(Duration::from_millis(*ms)),
            Beat::Gate(name) => {
                self.request(&json!({ "OpenGate": { "name": name } }))?;
            }
            Beat::Door(request) => {
                self.request(request)?;
            }
            Beat::Launch => {}
        }
        Ok(())
    }

    /// Waits until the agent rests on its host: its last sent prompt is in
    /// the chat, and the chat is idle, needs the person, or has ended.
    fn settle(&self, set: &Set, agent: &str) -> Result<()> {
        let host = set
            .host_of(agent)
            .ok_or_else(|| anyhow!("no declared agent {agent}"))?;
        let deadline = Instant::now() + PATIENCE;
        let mut calm = 0;
        loop {
            if exited(&self.control, &host, agent)? {
                return Ok(());
            }
            let chat = self.request(&json!({ "Chat": { "host": host, "agent": agent } }))?;
            let phase = chat["phase"].as_str().unwrap_or_default();
            let sent = self.sent.get(agent).is_none_or(|text| {
                chat["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|item| item["text"].as_str() == Some(text))
            });
            let resting = matches!(phase, "IDLE" | "NEEDS_YOU");
            calm = if sent && resting { calm + 1 } else { 0 };
            if calm >= 2 {
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("{agent} did not come to rest; its chat is {phase}");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Stops every agent and daemon; the scratch root goes with them.
    pub fn shutdown(mut self) -> Result<()> {
        let _ = self.request(&json!("Shutdown"));
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.process.try_wait()?.is_none() {
            if Instant::now() > deadline {
                self.process.kill()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&self.scratch);
        let _ = std::fs::remove_file(self.out.join("door.json"));
        Ok(())
    }
}

/// Whether the inventory of `host` lists `agent` as exited.
pub fn exited(control: &str, host: &str, agent: &str) -> Result<bool> {
    let inventory = request_at(control, &json!({ "Inventory": { "host": host } }))?;
    Ok(inventory["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|listed| {
            listed["name"] == agent && listed["lifecycle"] == wire::Lifecycle::Exited as i32
        }))
}

/// A UUID's sixteen bytes, from its text.
fn uuid_bytes(text: &str) -> Option<Vec<u8>> {
    let hex = text.replace('-', "");
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

/// One door request at `control`.
pub fn request_at(control: &str, request: &Value) -> Result<Value> {
    let mut stream = TcpStream::connect(control).context("connecting to the set's door")?;
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply).context("the door's reply")?;
    match reply.get("ok") {
        Some(value) => Ok(value.clone()),
        None => bail!("the door refused {request}: {reply}"),
    }
}

/// Merges `patch` into the YAML installation config at `path`.
pub fn patch_config(path: &Path, patch: &Value) -> Result<()> {
    if patch.is_null() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let mut config: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let patch: serde_yaml::Value = serde_yaml::to_value(patch)?;
    merge(&mut config, patch);
    std::fs::write(path, serde_yaml::to_string(&config)?)?;
    Ok(())
}

fn merge(into: &mut serde_yaml::Value, patch: serde_yaml::Value) {
    match (into, patch) {
        (serde_yaml::Value::Mapping(into), serde_yaml::Value::Mapping(patch)) => {
            for (key, value) in patch {
                match into.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        into.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}
