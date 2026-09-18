use fold::ProviderFold;

pub const FLEET_SHAPE: u32 = 3;
#[cfg(feature = "family-definition-v2-fixture")]
pub const CHAT_SHAPE: u32 = 2;
#[cfg(not(feature = "family-definition-v2-fixture"))]
pub const CHAT_SHAPE: u32 = 3;

#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub id: u32,
    pub sql: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub enum Regime {
    Derived,
    Durable { migrations: &'static [Migration] },
}

#[derive(Clone, Copy, Debug)]
pub struct Family {
    pub name: &'static str,
    pub regime: Regime,
    pub shape: u32,
    pub ddl: &'static str,
    pub retires_with: &'static [&'static str],
    pub tables: &'static [&'static str],
}

impl Family {
    pub const fn derived(
        name: &'static str,
        shape: u32,
        ddl: &'static str,
        retires_with: &'static [&'static str],
        tables: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            regime: Regime::Derived,
            shape,
            ddl,
            retires_with,
            tables,
        }
    }

    pub const fn durable(
        name: &'static str,
        migrations: &'static [Migration],
        tables: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            regime: Regime::Durable { migrations },
            shape: migrations.len() as u32,
            ddl: "",
            retires_with: &[],
            tables,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Registry {
    families: &'static [&'static Family],
}

impl Registry {
    pub const fn new(families: &'static [&'static Family]) -> Self {
        Self { families }
    }

    pub const fn families(&self) -> &'static [&'static Family] {
        self.families
    }

    pub fn get(&self, name: &str) -> Option<&'static Family> {
        self.families
            .iter()
            .copied()
            .find(|family| family.name == name)
    }
}

pub const META_MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS migration (
    family TEXT NOT NULL,
    id INTEGER NOT NULL,
    checksum TEXT NOT NULL,
    applied_at INTEGER NOT NULL,
    PRIMARY KEY (family, id)
);
CREATE TABLE IF NOT EXISTS family_shape (
    family TEXT PRIMARY KEY,
    shape INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    applied_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS host_revision (
    host_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL,
    deleted_through INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS removal_fence (
    host_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY (host_id, agent_id)
);
CREATE TABLE IF NOT EXISTS quarantine (
    id TEXT PRIMARY KEY,
    manifest TEXT NOT NULL,
    durable_unresolved INTEGER NOT NULL
);
"#;

pub static META_MIGRATIONS: &[Migration] = &[Migration {
    id: 1,
    sql: META_MIGRATION_1,
}];

pub const VIEW_MIGRATION_1: &str = r#"
CREATE TABLE view_state (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (kind, key)
);
"#;

pub static VIEW_MIGRATIONS: &[Migration] = &[Migration {
    id: 1,
    sql: VIEW_MIGRATION_1,
}];

const FLEET_DDL: &str = r#"
CREATE TABLE host (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    online INTEGER NOT NULL,
    version TEXT,
    platform TEXT,
    capabilities BLOB,
    trust BLOB,
    dial_error TEXT,
    via BLOB NOT NULL,
    signed_in INTEGER,
    revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE agent (
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    kind BLOB NOT NULL,
    protocol INTEGER,
    name TEXT,
    command TEXT,
    working_dir TEXT,
    args BLOB,
    readonly INTEGER NOT NULL,
    parent_host_id TEXT,
    parent_id TEXT,
    created_at INTEGER NOT NULL,
    last_activity INTEGER NOT NULL,
    working_on TEXT,
    working_on_at INTEGER,
    membership INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    absent_since INTEGER,
    last_opened_at INTEGER
);
CREATE INDEX agent_host_g$GEN ON agent(host_id, id);
CREATE TABLE host_summary (
    agent_id TEXT PRIMARY KEY,
    through INTEGER NOT NULL,
    producer_version INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    stale INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    summary BLOB NOT NULL,
    unknown BLOB NOT NULL
);
CREATE TABLE progress (
    agent_id TEXT PRIMARY KEY,
    through INTEGER NOT NULL,
    at INTEGER NOT NULL,
    revision INTEGER NOT NULL
);
"#;

const CHAT_DDL: &str = r#"
CREATE TABLE chat_state (
    agent_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL,
    content_revision INTEGER NOT NULL,
    segment_high_water INTEGER NOT NULL,
    previous_through INTEGER,
    needs_baseline INTEGER NOT NULL,
    retiring INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE chat_head (
    agent_id TEXT PRIMARY KEY,
    version INTEGER NOT NULL,
    protocol INTEGER NOT NULL,
    segment INTEGER NOT NULL,
    baseline_kind INTEGER NOT NULL,
    baseline_seq INTEGER,
    through INTEGER NOT NULL,
    tip_version INTEGER NOT NULL,
    entry_version INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    tip_bytes INTEGER NOT NULL,
    summary BLOB
);
CREATE TABLE segment (
    agent_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    predecessor INTEGER,
    baseline_kind INTEGER NOT NULL,
    baseline_seq INTEGER,
    first_seq INTEGER,
    last_seq INTEGER,
    closed_by INTEGER,
    opened_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, id)
);
CREATE INDEX segment_order_g$GEN ON segment(agent_id, id);
CREATE TABLE eviction_frontier (
    agent_id TEXT PRIMARY KEY,
    segment INTEGER NOT NULL,
    order_seq INTEGER NOT NULL,
    order_slot INTEGER NOT NULL,
    key TEXT NOT NULL
);
"#;

macro_rules! provider_ddl {
    ($prefix:literal) => {
        concat!(
            "CREATE TABLE ", $prefix, "_tip (agent_id TEXT PRIMARY KEY, tip BLOB NOT NULL);",
            "CREATE TABLE ", $prefix, "_entry (agent_id TEXT NOT NULL, key TEXT NOT NULL, segment INTEGER NOT NULL, order_seq INTEGER NOT NULL, order_slot INTEGER NOT NULL, revision_seq INTEGER NOT NULL, revision_fence INTEGER NOT NULL, revision_ordinal INTEGER NOT NULL, kind TEXT NOT NULL, text TEXT, bytes INTEGER NOT NULL, body BLOB NOT NULL, PRIMARY KEY (agent_id, key));",
            "CREATE INDEX ", $prefix, "_entry_order_g$GEN ON ", $prefix, "_entry(agent_id, segment, order_seq, order_slot, key);",
            "CREATE TABLE ", $prefix, "_tombstone (agent_id TEXT NOT NULL, key TEXT NOT NULL, revision_seq INTEGER NOT NULL, revision_fence INTEGER NOT NULL, revision_ordinal INTEGER NOT NULL, PRIMARY KEY (agent_id, key));",
            "CREATE TABLE ", $prefix, "_alias (agent_id TEXT NOT NULL, from_key TEXT NOT NULL, to_key TEXT NOT NULL, revision_seq INTEGER NOT NULL, revision_fence INTEGER NOT NULL, revision_ordinal INTEGER NOT NULL, promotion INTEGER, PRIMARY KEY (agent_id, from_key));"
        )
    };
}

const PROVIDER_DDL_CLAUDE_PTY: &str = provider_ddl!("claude_pty");
const PROVIDER_DDL_CLAUDE_SDK: &str = provider_ddl!("claude_sdk");
const PROVIDER_DDL_CODEX: &str = provider_ddl!("codex");

pub static META: Family = Family::durable(
    "meta",
    META_MIGRATIONS,
    &[
        "migration",
        "family_shape",
        "host_revision",
        "removal_fence",
        "quarantine",
    ],
);
pub static FLEET: Family = Family::derived(
    "fleet",
    FLEET_SHAPE,
    FLEET_DDL,
    &[],
    &["host", "agent", "host_summary", "progress"],
);
pub static CHAT: Family = Family::derived(
    "chat",
    CHAT_SHAPE,
    CHAT_DDL,
    &["claude_pty", "claude_sdk", "codex"],
    &["chat_state", "chat_head", "segment", "eviction_frontier"],
);
pub static CLAUDE_PTY: Family = Family::derived(
    "claude_pty",
    fold::claude_pty::ClaudeFold::ENTRY_VERSION,
    PROVIDER_DDL_CLAUDE_PTY,
    &[],
    &[
        "claude_pty_tip",
        "claude_pty_entry",
        "claude_pty_tombstone",
        "claude_pty_alias",
    ],
);
pub static CLAUDE_SDK: Family = Family::derived(
    "claude_sdk",
    fold::claude_sdk::ClaudeSdkFold::ENTRY_VERSION,
    PROVIDER_DDL_CLAUDE_SDK,
    &[],
    &[
        "claude_sdk_tip",
        "claude_sdk_entry",
        "claude_sdk_tombstone",
        "claude_sdk_alias",
    ],
);
pub static CODEX: Family = Family::derived(
    "codex",
    fold::codex::CodexFold::ENTRY_VERSION,
    PROVIDER_DDL_CODEX,
    &[],
    &["codex_tip", "codex_entry", "codex_tombstone", "codex_alias"],
);
pub static VIEW: Family = Family::durable("view", VIEW_MIGRATIONS, &["view_state"]);

pub static REGISTRY: Registry = Registry::new(&[
    &META,
    &FLEET,
    &CHAT,
    &CLAUDE_PTY,
    &CLAUDE_SDK,
    &CODEX,
    &VIEW,
]);
