//! Defaults for fields Claude always writes.
//!
//! amux's own test inputs are written by hand and leave out fields a real
//! line always carries. Production reads such a line as it always has,
//! with the field at its default (empty, zero, or none for a field amux
//! treats as optional); a strict decode refuses it, so a recording that
//! drops one of these fields still reads as drift.

macro_rules! absent {
    ($($name:ident => $wire:literal,)*) => {
        $(
            pub(crate) fn $name<T: Default>() -> T {
                crate::strictness::unknown(|| concat!("missing field `", $wire, "`").to_owned());
                T::default()
            }
        )*
    };
}

absent! {
    agents => "agents",
    api_usage => "apiUsage",
    color => "color",
    grid_rows => "gridRows",
    is_auto_compact_enabled => "isAutoCompactEnabled",
    mcp_tools => "mcpTools",
    memory_files => "memoryFiles",
    raw_max_tokens => "rawMaxTokens",
    api_key_source => "apiKeySource",
    claude_code_version => "claude_code_version",
    content => "content",
    cwd => "cwd",
    description => "description",
    duration_api_ms => "duration_api_ms",
    errors => "errors",
    is_error => "is_error",
    mcp_servers => "mcp_servers",
    message_type => "type",
    model => "model",
    model_usage => "modelUsage",
    num_turns => "num_turns",
    output_style => "output_style",
    parent_tool_use_id => "parent_tool_use_id",
    permission_denials => "permission_denials",
    permission_mode => "permissionMode",
    plugins => "plugins",
    result => "result",
    role => "role",
    session_id => "session_id",
    skills => "skills",
    slash_commands => "slash_commands",
    stop_reason => "stop_reason",
    task_id => "task_id",
    task_type => "task_type",
    tasks => "tasks",
    tools => "tools",
    total_cost_usd => "total_cost_usd",
    usage => "usage",
    uuid => "uuid",
    compact_metadata => "compactMetadata",
    duration_ms => "durationMs",
    entrypoint => "entrypoint",
    git_branch => "gitBranch",
    id => "id",
    is_meta => "isMeta",
    is_sidechain => "isSidechain",
    level => "level",
    message => "message",
    message_count => "messageCount",
    parent_uuid => "parentUuid",
    pre_tokens => "preTokens",
    prompt => "prompt",
    prompt_id => "promptId",
    row_session_id => "sessionId",
    status => "status",
    stop_hook_active => "stop_hook_active",
    timestamp => "timestamp",
    transcript_path => "transcript_path",
    trigger => "trigger",
    user_type => "userType",
    version => "version",
}

pub(crate) fn percentage() -> serde_json::Number {
    crate::strictness::unknown(|| "missing field `percentage`".to_owned());
    serde_json::Number::from(0)
}
