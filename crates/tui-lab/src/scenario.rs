//! A scenario: the hosts and agents a lab session starts with, what each
//! agent has already said, and a timeline of what happens next. Scenarios
//! are YAML files under `crates/tui-lab/scenarios/`, read at launch so an
//! edit needs no rebuild.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The host this terminal runs on.
    #[serde(default = "laptop")]
    pub local_host: String,
    #[serde(default)]
    pub hosts: Vec<HostSpec>,
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    #[serde(default)]
    pub timeline: Vec<Beat>,
    /// A chat to open at launch.
    #[serde(default)]
    pub open: Option<String>,
    /// Where new agents work.
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub repositories: Vec<String>,
    /// Where the person chats with their agents: `amux` (the default) or
    /// `terminal`, which starts a new agent from a form and attaches to it.
    #[serde(default)]
    pub chat_in: Option<String>,
}

fn laptop() -> String {
    "laptop".into()
}

fn default_cwd() -> String {
    "~/source/amux".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub presence: Presence,
    #[serde(default)]
    pub via: Via,
    #[serde(default)]
    pub signed_in: Option<bool>,
    #[serde(default)]
    pub revoked: bool,
    /// Found by discovery, not yet paired.
    #[serde(default)]
    pub candidate: bool,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub dial_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    #[default]
    Online,
    Offline,
    Away,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    #[default]
    Direct,
    Relay,
    Ssh,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KindSpec {
    /// Claude in its own terminal.
    Claude,
    /// Claude through the SDK, headless.
    #[default]
    ClaudeSdk,
    Codex,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseSpec {
    Starting,
    Idle,
    Working,
    NeedsYou,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: KindSpec,
    /// Defaults to the local host.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    /// Derived from the transcript when absent: an open ask needs you, an
    /// unfinished turn works, anything else is idle.
    #[serde(default)]
    pub phase: Option<PhaseSpec>,
    /// Set to the exit cause for an agent that has exited.
    #[serde(default)]
    pub exited: Option<String>,
    #[serde(default)]
    pub working_on: Option<String>,
    /// How long ago the agent last did anything.
    #[serde(default)]
    pub ago: Option<Dur>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub context: Option<ContextSpec>,
    #[serde(default)]
    pub usage: Option<UsageSpec>,
    #[serde(default)]
    pub tasks: Vec<TaskSpec>,
    #[serde(default)]
    pub sign_in: Option<SignInSpec>,
    #[serde(default)]
    pub background: Option<u32>,
    #[serde(default)]
    pub failed_servers: Vec<ServerSpec>,
    /// Prompts waiting behind the running turn.
    #[serde(default)]
    pub queue: Vec<QueueSpec>,
    #[serde(default)]
    pub transcript: Vec<Entry>,
    /// The working tree's unified diff, for the review page.
    #[serde(default)]
    pub diff: Option<String>,
    /// What the agent does with a prompt sent from the lab. `{prompt}` in
    /// a `say` is replaced with the prompt's text.
    #[serde(default)]
    pub reply: Option<Vec<Entry>>,
    /// What a Codex agent does when told to implement its plan.
    #[serde(default)]
    pub implement: Option<Vec<Entry>>,
    /// How the lab's link treats a prompt sent to this agent.
    #[serde(default)]
    pub send: SendSpec,
    /// What sits in the composer the first time the chat opens: words,
    /// then attachments as if pasted.
    #[serde(default)]
    pub draft: Option<DraftSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftSpec {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attach: Vec<AttachSpec>,
}

/// A pasted file: its name (an image by its extension) and its size in
/// bytes; or, with `text`, pasted text under that name.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachSpec {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub text: Option<String>,
}

/// How the lab's link treats a prompt sent to an agent, to show the
/// composer's sending states.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendSpec {
    /// Held this long before the agent answers, so the prompt shows on
    /// its way.
    #[serde(default)]
    pub delay: Option<Dur>,
    /// Refused with this reason (the wire's: draining, exiting,
    /// unsupported, or any words).
    #[serde(default)]
    pub reject: Option<String>,
    /// The link drops as the prompt goes: the agent's host is away this
    /// long, and the prompt never arrives.
    #[serde(default)]
    pub lose: Option<Dur>,
}

/// A queued prompt: its words, or its words with who queued it (another
/// agent) and whether it is already being sent into the running turn.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum QueueSpec {
    Text(String),
    Full {
        text: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        steer: bool,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSpec {
    pub used: u64,
    #[serde(default)]
    pub window: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSpec {
    pub state: UsageStateSpec,
    #[serde(default)]
    pub windows: Vec<UsageWindowSpec>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStateSpec {
    Ok,
    Near,
    Blocked,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageWindowSpec {
    pub name: String,
    pub used: f64,
    #[serde(default)]
    pub resets_in: Option<Dur>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub subject: String,
    #[serde(default)]
    pub status: TaskStatus,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Active,
    Done,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignInSpec {
    pub state: SignInStateSpec,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInStateSpec {
    SignedIn,
    SignedOut,
    Expired,
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    pub name: String,
    #[serde(default)]
    pub error: String,
}

/// One thing in a transcript, a reply or a timeline beat. Written in YAML
/// as a one-key map: `- say: "..."`, `- read: src/lib.rs`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// A person's prompt.
    User(String),
    /// The agent's prose. Streams in word by word when played live.
    Say(String),
    Think(String),
    Read(String),
    Grep(String),
    Glob(String),
    Fetch(String),
    Search(String),
    Bash(BashSpec),
    Edit(EditSpec),
    Write(WriteSpec),
    /// Any tool call, in the provider's own names.
    Tool(ToolSpec),
    /// A subagent call.
    Subagent(SubagentSpec),
    /// Opens an ask; the agent needs you until it is answered.
    Ask(AskSpec),
    /// The turn ends.
    Turn(TurnSpec),
    Error(ErrorSpec),
    /// A message from or to another agent.
    Message(MessageSpec),
    Boundary(BoundarySpec),
    /// A pause before the next entry.
    Wait(Dur),
    Phase(PhaseSpec),
    WorkingOn(String),
    /// The agent's process ends with this cause.
    Exit(String),
    Tasks(Vec<TaskSpec>),
    Context(ContextSpec),
    /// A chunk of the reply being streamed, opening one if none is; beats
    /// of these let a frame catch a reply part way.
    Stream(String),
    /// The streamed reply is complete.
    Said,
    /// A plan, written the way the agent writes one: Claude to its plan
    /// file (streamed when headless, whole in a terminal) and then
    /// ExitPlanMode's ask, Codex as a streamed `<proposed_plan>` message.
    /// In a Claude script, what follows is what it does once approved.
    Plan(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum BashSpec {
    Command(String),
    Full {
        command: String,
        #[serde(default)]
        output: String,
        #[serde(default)]
        exit: Option<i32>,
        #[serde(default)]
        took: Option<Dur>,
        #[serde(default)]
        background: bool,
        #[serde(default)]
        running: bool,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditSpec {
    pub path: String,
    #[serde(default)]
    pub old: String,
    #[serde(default)]
    pub new: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteSpec {
    pub path: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub state: ToolStateSpec,
    #[serde(default)]
    pub took: Option<Dur>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub decision: Option<DecisionSpec>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStateSpec {
    Pending,
    Running,
    #[default]
    Ok,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSpec {
    pub outcome: DecisionOutcomeSpec,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcomeSpec {
    Allowed,
    Denied,
    Auto,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentSpec {
    pub description: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tools: u32,
    #[serde(default)]
    pub last: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub result: String,
}

/// An ask, by what it asks. Each has `then`: what the agent does once it
/// is answered (a short acknowledgement and a turn end when absent).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskSpec {
    /// Permission to run a tool call.
    Permission(PermissionSpec),
    Question(QuestionSpec),
    Plan(PlanSpec),
    Form(FormSpec),
    Link(LinkSpec),
    /// Something the provider shows that no answer from here can reach.
    Unanswerable(UnanswerableSpec),
    /// Codex asks for files or the network beyond its sandbox.
    Access(AccessSpec),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSpec {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSpec {
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub reason: String,
    /// The scopes offered beyond "once", as labels.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// What the call returns once allowed.
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSpec {
    pub questions: Vec<QuestionItem>,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionItem {
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub multi: bool,
    #[serde(default = "yes")]
    pub other: bool,
    /// The typed answer is a secret (Codex): shown as dots, never echoed.
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum OptionSpec {
    Label(String),
    Full {
        label: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        recommended: bool,
        #[serde(default)]
        preview: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSpec {
    pub plan: String,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormSpec {
    pub server: String,
    pub message: String,
    #[serde(default)]
    pub schema: Value,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    pub server: String,
    pub message: String,
    pub url: String,
    #[serde(default)]
    pub then: Option<Vec<Entry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnanswerableSpec {
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSpec {
    /// How long the turn ran; the time since its prompt when absent.
    #[serde(default)]
    pub took: Option<Dur>,
    #[serde(default)]
    pub outcome: TurnOutcomeSpec,
    #[serde(default)]
    pub cost: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcomeSpec {
    #[default]
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorSpec {
    pub message: String,
    #[serde(default)]
    pub retry: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSpec {
    /// The other agent's id in this scenario.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub text: String,
    #[serde(default)]
    pub finished: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundarySpec {
    Started,
    Cleared,
    Compacted,
    Resumed,
    Restarted,
}

/// Something that happens a while after launch.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Beat {
    /// Seconds after the previous beat.
    pub after: Dur,
    /// Whose transcript `play` runs in.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub play: Vec<Entry>,
    /// A new agent appears.
    #[serde(default)]
    pub create: Option<Box<AgentSpec>>,
    #[serde(default)]
    pub remove: Option<String>,
    /// A host changes presence.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub presence: Option<Presence>,
}

/// A duration written `250ms`, `3s`, `5m`, `2h` or `4d`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dur(pub i64);

impl Dur {
    pub fn ms(self) -> i64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Dur, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Text(String),
        }
        match Raw::deserialize(de)? {
            Raw::Number(seconds) => Ok(Dur((seconds * 1000.0) as i64)),
            Raw::Text(text) => parse_dur(&text).map_err(serde::de::Error::custom),
        }
    }
}

pub fn parse_dur(text: &str) -> Result<Dur, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("not a duration: {text:?}"))?;
    let scale = match unit.trim() {
        "ms" => 1.0,
        "" | "s" => 1_000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        other => return Err(format!("unknown duration unit {other:?} in {text:?}")),
    };
    Ok(Dur((number * scale) as i64))
}

pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

/// Every scenario's name and description, sorted.
pub fn list() -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir())? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "yaml") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let description =
                load(&name).map_or_else(|e| format!("(broken: {e:#})"), |s| s.description);
            out.push((name, description));
        }
    }
    out.sort();
    Ok(out)
}

pub fn load(name: &str) -> Result<Scenario> {
    let path = if name.ends_with(".yaml") {
        PathBuf::from(name)
    } else {
        dir().join(format!("{name}.yaml"))
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    // Entries are written as one-key maps (`- say: hi`), which serde_yaml
    // reads as enums only through its singleton-map adapter.
    let value: serde_yaml::Value =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut scenario: Scenario = serde_yaml::with::singleton_map_recursive::deserialize(value)
        .with_context(|| format!("reading {}", path.display()))?;
    scenario.name = path.file_stem().unwrap().to_string_lossy().into_owned();
    check(&scenario)?;
    Ok(scenario)
}

fn check(scenario: &Scenario) -> Result<()> {
    let hosts: Vec<&str> = scenario.hosts.iter().map(|h| h.id.as_str()).collect();
    let mut agents: Vec<&str> = scenario.agents.iter().map(|a| a.id.as_str()).collect();
    for beat in &scenario.timeline {
        if let Some(created) = &beat.create {
            agents.push(&created.id);
        }
    }
    let host_ok = |id: &str| id == scenario.local_host || hosts.contains(&id);
    for agent in &scenario.agents {
        if let Some(host) = &agent.host
            && !host_ok(host)
        {
            bail!("agent {} names unknown host {host}", agent.id);
        }
        if let Some(parent) = &agent.parent
            && !agents.contains(&parent.as_str())
        {
            bail!("agent {} names unknown parent {parent}", agent.id);
        }
    }
    for beat in &scenario.timeline {
        if let Some(agent) = &beat.agent
            && !agents.contains(&agent.as_str())
        {
            bail!("a beat names unknown agent {agent}");
        }
        if let Some(host) = &beat.host
            && !host_ok(host)
        {
            bail!("a beat names unknown host {host}");
        }
    }
    if let Some(open) = &scenario.open
        && !agents.contains(&open.as_str())
    {
        bail!("open names unknown agent {open}");
    }
    Ok(())
}
