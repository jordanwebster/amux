//! A set: a declared world with no story and no assertions. Its topology
//! is the test network's own format (hosts, links, agents and the scripts
//! their fake providers play); its timeline is door requests, played
//! before the client starts and, after a `launch` beat, while it runs.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::Value;

/// Where sets live, from the repository root.
pub const DIR: &str = "journeys/sets";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Set {
    #[serde(skip)]
    pub name: String,
    /// What the set shows and how to look at it.
    pub description: String,
    /// The host whose install the terminal client runs on.
    pub client: String,
    /// The agent whose chat frames open on, by its declared name.
    #[serde(default)]
    pub open: Option<String>,
    /// Merged into the client host's installation config before the client
    /// starts (`{"ui": {"chat_in": "terminal"}}`).
    #[serde(default)]
    pub config: Value,
    /// A test network topology, as `testnet serve` reads it, with its
    /// scripts inline. Kept as written: a form schema's keys are in the
    /// order its form asks for them.
    #[serde(rename = "topology")]
    pub raw_topology: Box<serde_json::value::RawValue>,
    /// The same topology parsed, for looking things up in it.
    #[serde(skip)]
    pub topology: Value,
    #[serde(default)]
    pub timeline: Vec<Beat>,
}

/// One beat of a set's timeline.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Beat {
    /// A prompt through the agent's own host, as another client sends one.
    Send { agent: String, text: String },
    /// Wait until the agent rests: idle, needing the person, or exited.
    Settle(String),
    /// Let time pass.
    WaitMs(u64),
    /// Release the scripts' `wait_for` steps waiting on this gate.
    Gate(String),
    /// Any other door request, as the door takes it
    /// (`{"Sever": {"a": "desk", "b": "laptop"}}`).
    Door(Value),
    /// The client starts here; later beats play while it runs.
    Launch,
}

impl Set {
    pub fn load(root: &Path, name: &str) -> Result<Set> {
        let path = root.join(DIR).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut set: Set =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        set.name = name.to_owned();
        set.topology = serde_json::from_str(set.raw_topology.get())?;
        let files = set.topology["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|agent| agent.get("script_file").is_some());
        if files {
            bail!("{name}: a set's scripts are inline, not in script files");
        }
        if set
            .timeline
            .iter()
            .filter(|b| matches!(b, Beat::Launch))
            .count()
            > 1
        {
            bail!("{name}: one launch beat at most");
        }
        Ok(set)
    }

    /// Every set's name and description, sorted by name.
    pub fn list(root: &Path) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(root.join(DIR))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let set = Set::load(root, name)?;
            out.push((set.name, set.description));
        }
        out.sort();
        Ok(out)
    }

    /// The beats before the client starts, and those while it runs.
    pub fn beats(&self) -> (Vec<Beat>, Vec<Beat>) {
        match self.timeline.iter().position(|b| matches!(b, Beat::Launch)) {
            Some(at) => (
                self.timeline[..at].to_vec(),
                self.timeline[at + 1..].to_vec(),
            ),
            None => (self.timeline.clone(), Vec::new()),
        }
    }

    /// Which host each declared agent runs on.
    pub fn host_of(&self, agent: &str) -> Option<String> {
        self.topology
            .get("agents")?
            .as_array()?
            .iter()
            .find(|decl| decl.get("name").and_then(Value::as_str) == Some(agent))?
            .get("host")?
            .as_str()
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::{Beat, Set};

    /// Every set loads, runs its client on a host it declares, and names
    /// only agents it declares or spawns.
    #[test]
    fn every_set_is_well_formed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sets = Set::list(&root).unwrap();
        assert!(!sets.is_empty());
        for (name, _) in sets {
            let set = Set::load(&root, &name).unwrap();
            let hosts: Vec<&str> = set.topology["hosts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|host| host["name"].as_str())
                .collect();
            assert!(hosts.contains(&set.client.as_str()), "{name}: client host");
            let mut agents: Vec<String> = set.topology["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|agent| agent["name"].as_str().map(str::to_owned))
                .collect();
            for beat in &set.timeline {
                if let Beat::Door(door) = beat
                    && let Some(spawned) = door.pointer("/Spawn/agent/name").and_then(Value::as_str)
                {
                    agents.push(spawned.to_owned());
                }
            }
            for named in set
                .open
                .iter()
                .chain(set.timeline.iter().filter_map(|beat| match beat {
                    Beat::Send { agent, .. } | Beat::Settle(agent) => Some(agent),
                    _ => None,
                }))
            {
                assert!(agents.contains(named), "{name}: no agent {named}");
            }
        }
    }

    /// Plan mode's agents each propose their plan as a plan, which is what
    /// opens its decision; a plan written as an ordinary message would read
    /// as one and leave nothing to decide.
    #[test]
    fn every_planner_proposes_a_plan() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let set = Set::load(&root, "plan-mode").unwrap();
        for agent in set.topology["agents"].as_array().unwrap() {
            let steps = agent["script"]["steps"].as_array().unwrap();
            assert!(
                steps.iter().any(|step| step.pointer("/ask/plan").is_some()),
                "{} proposes no plan",
                agent["name"]
            );
        }
    }
}
