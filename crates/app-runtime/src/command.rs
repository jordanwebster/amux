//! What a client asks of the runtime, as the values it sends.

use std::path::PathBuf;

use model::{AgentType, ClaudeDriver, HostId};
use serde::{Deserialize, Serialize};
use ui_state::{AgentId, Command};

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandDto {
    Subscription(SubscriptionCommand),
    Pairing(PairingCommand),
    Connection(ConnectionCommand),
    Devices(DevicesCommand),
    Creation(CreationCommand),
    Accounts(AccountsCommand),
    Shared(Command),
}

/// Something asked of the set of accounts this client is signed in to.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountsCommand {
    /// Read a different account. Every screen starts again from that account's
    /// own machines; nothing the previous one showed is carried across.
    Select { account: String },
}

/// Starting an agent on a machine, and finding somewhere to start it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum CreationCommand {
    /// What a machine has to offer as a working directory: the projects it was
    /// used in recently, the repositories under its roots, and the roots.
    ListRepositories {
        host: HostId,
        #[serde(default)]
        query: Option<String>,
        /// A combined maximum, recent first. The host caps it; zero asks for
        /// the roots alone.
        limit: u32,
    },
    /// Start an agent on a machine, in a directory, under a named layer.
    CreateAgent {
        host: HostId,
        directory: PathBuf,
        name: String,
        agent: NewAgent,
    },
}

/// Which layer a new agent runs under, said in full.
///
/// Claude's driver is a required field with no default anywhere on the way in.
/// A rich client drives Claude through the SDK, and the failure a default would
/// allow is silent: a request that left the driver unsaid would start a PTY
/// session that looks like every other agent until somebody tries to do
/// something only the SDK can do. Naming it costs one word and removes the
/// whole class.
#[derive(Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewAgent {
    Claude {
        driver: ClaudeDriver,
    },
    Codex {
        #[serde(default)]
        model: Option<String>,
    },
}

impl NewAgent {
    fn agent_type(self) -> AgentType {
        match self {
            NewAgent::Claude { driver } => AgentType::Claude { driver },
            NewAgent::Codex { model } => AgentType::Codex {
                model,
                approval_policy: None,
                sandbox_policy: None,
                resume_thread_id: None,
            },
        }
    }
}

/// The shared command a creation asks for, so what the runtime hands the
/// client can be read without a running runtime.
pub fn creation(command: CreationCommand) -> Option<Command> {
    match command {
        CreationCommand::CreateAgent {
            host,
            directory,
            name,
            agent,
        } => Some(Command::CreateAgent {
            host: Some(host),
            name,
            agent_type: agent.agent_type(),
            working_dir: directory,
        }),
        CreationCommand::ListRepositories { .. } => None,
    }
}

/// Something asked of this device's own trust store.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum DevicesCommand {
    /// Stop trusting a machine. Every link this device holds to it is closed
    /// before the answer comes back, so nothing that was already open outlives
    /// the revocation.
    Revoke { host: HostId },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubscriptionCommand {
    Subscribe { agent: AgentId },
    Unsubscribe { agent: AgentId },
}

/// Something asked of this device's own link to the relay, rather than of a
/// machine on the other side of it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectionCommand {
    /// Stop waiting out the backoff and dial the relay now.
    RetryNow,
    /// Ask the account service again what this account buys.
    ///
    /// What a purchase that has just gone through is waiting on: the receipt
    /// is settled somewhere else entirely, and nothing on this device knows
    /// it happened until somebody asks.
    RefreshEntitlement,
}

/// One step of pairing this device with a machine.
///
/// Two phases, never one. Beginning authenticates the secret and answers with
/// the machine's own account of itself; nothing is trusted until a separate
/// confirmation naming that attempt arrives. A caller that begins and never
/// answers has paired with nobody: the attempt expires on the machine.
#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairingCommand {
    /// A six-digit code, against the one machine that issued it.
    BeginPairPin {
        host: HostId,
        pin: String,
    },
    /// The payload an `amux://pair` link carries, which names its own machine.
    BeginPairLink {
        payload: String,
    },
    Confirm {
        pending: String,
    },
    Abandon {
        pending: String,
    },
}
