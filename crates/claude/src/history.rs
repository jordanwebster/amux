use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::sdk::error::Error;
use crate::sdk::types::{Extensions, RawFrame};

#[derive(Debug, Clone, Default)]
pub struct ListSessionsOptions {
    pub dir: Option<PathBuf>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub include_worktrees: Option<bool>,
    pub include_programmatic: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct GetSessionInfoOptions {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct GetSessionMessagesOptions {
    pub dir: Option<PathBuf>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub include_system_messages: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ListSubagentsOptions {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct GetSubagentMessagesOptions {
    pub dir: Option<PathBuf>,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SessionMutationOptions {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ForkSessionOptions {
    pub dir: Option<PathBuf>,
    pub up_to_message_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub summary: String,
    pub last_modified: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageType {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SessionMessage {
    Known(KnownSessionMessage),
    Raw(RawFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnownSessionMessage {
    #[serde(rename = "type")]
    pub message_type: SessionMessageType,
    pub uuid: String,
    pub session_id: String,
    pub message: Value,
    pub parent_tool_use_id: Option<String>,
    pub parent_agent_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionResult {
    pub session_id: String,
}

pub async fn list_sessions(options: ListSessionsOptions) -> Result<Vec<SessionInfo>, Error> {
    validate_pagination(options.limit, options.offset)?;
    let directories =
        project_directories(options.dir.as_deref(), options.include_worktrees).await?;
    let mut sessions = HashMap::<String, SessionInfo>::new();
    for directory in directories {
        if !directory.exists() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !valid_session_id(session_id) {
                continue;
            }
            if let Some(info) = read_session_info(&path, session_id).await? {
                if options.include_programmatic == Some(false)
                    && transcript_is_programmatic(&path).await?
                {
                    continue;
                }
                match sessions.get(session_id) {
                    Some(existing) if existing.last_modified >= info.last_modified => {}
                    _ => {
                        sessions.insert(session_id.to_owned(), info);
                    }
                }
            }
        }
    }
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .last_modified
            .cmp(&left.last_modified)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(page(sessions, options.offset, options.limit))
}

pub async fn get_session_info(
    session_id: &str,
    options: GetSessionInfoOptions,
) -> Result<Option<SessionInfo>, Error> {
    validate_session_id(session_id)?;
    let Some(path) = find_session_file(session_id, options.dir.as_deref()).await? else {
        return Ok(None);
    };
    read_session_info(&path, session_id).await
}

pub async fn get_session_messages(
    session_id: &str,
    options: GetSessionMessagesOptions,
) -> Result<Vec<SessionMessage>, Error> {
    validate_session_id(session_id)?;
    validate_pagination(options.limit, options.offset)?;
    let Some(path) = find_session_file(session_id, options.dir.as_deref()).await? else {
        return Ok(Vec::new());
    };
    read_messages(
        &path,
        session_id,
        options.include_system_messages,
        options.offset,
        options.limit,
    )
    .await
}

pub async fn list_subagents(
    session_id: &str,
    options: ListSubagentsOptions,
) -> Result<Vec<String>, Error> {
    validate_session_id(session_id)?;
    let Some(path) = find_session_file(session_id, options.dir.as_deref()).await? else {
        return Ok(Vec::new());
    };
    let Some(project) = path.parent() else {
        return Ok(Vec::new());
    };
    let directory = project.join(session_id).join("subagents");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(agent_id) = name
            .strip_prefix("agent-")
            .and_then(|name| name.strip_suffix(".jsonl"))
            && !agent_id.is_empty()
        {
            result.push(agent_id.to_owned());
        }
    }
    result.sort();
    Ok(result)
}

pub async fn get_subagent_messages(
    session_id: &str,
    agent_id: &str,
    options: GetSubagentMessagesOptions,
) -> Result<Vec<SessionMessage>, Error> {
    validate_session_id(session_id)?;
    validate_component("agent_id", agent_id)?;
    validate_pagination(options.limit, options.offset)?;
    let Some(path) = find_session_file(session_id, options.dir.as_deref()).await? else {
        return Ok(Vec::new());
    };
    let path = path
        .parent()
        .expect("session file has parent")
        .join(session_id)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"));
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_messages(&path, session_id, false, options.offset, options.limit).await
}

pub async fn rename_session(
    session_id: &str,
    title: &str,
    options: SessionMutationOptions,
) -> Result<(), Error> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Persistence("session title must be non-empty".into()));
    }
    append_metadata(
        session_id,
        options.dir.as_deref(),
        serde_json::json!({"type":"custom-title","customTitle":title,"sessionId":session_id}),
    )
    .await
}

pub async fn tag_session(
    session_id: &str,
    tag: Option<&str>,
    options: SessionMutationOptions,
) -> Result<(), Error> {
    if tag.is_some_and(|tag| tag.trim().is_empty()) {
        return Err(Error::Persistence("session tag must be non-empty".into()));
    }
    append_metadata(
        session_id,
        options.dir.as_deref(),
        serde_json::json!({"type":"tag","tag":tag.map(str::trim),"sessionId":session_id}),
    )
    .await
}

pub async fn delete_session(
    session_id: &str,
    options: SessionMutationOptions,
) -> Result<(), Error> {
    validate_session_id(session_id)?;
    let path = find_session_file(session_id, options.dir.as_deref())
        .await?
        .ok_or_else(|| Error::Persistence(format!("session {session_id} was not found")))?;
    tokio::fs::remove_file(&path).await?;
    let sidecars = path
        .parent()
        .expect("session file has parent")
        .join(session_id);
    if sidecars.exists() {
        tokio::fs::remove_dir_all(sidecars).await?;
    }
    Ok(())
}

pub async fn fork_session(
    session_id: &str,
    options: ForkSessionOptions,
) -> Result<ForkSessionResult, Error> {
    validate_session_id(session_id)?;
    if let Some(message_id) = &options.up_to_message_id {
        validate_component("up_to_message_id", message_id)?;
    }
    let source = find_session_file(session_id, options.dir.as_deref())
        .await?
        .ok_or_else(|| Error::Persistence(format!("session {session_id} was not found")))?;
    let mut entries = read_json_lines(&source).await?;
    if let Some(message_id) = &options.up_to_message_id {
        let index = entries
            .iter()
            .position(|entry| string(entry, "uuid") == Some(message_id.as_str()))
            .ok_or_else(|| Error::Persistence(format!("message {message_id} was not found")))?;
        entries.truncate(index + 1);
    }
    let new_session_id = uuid::Uuid::new_v4().to_string();
    let mut uuid_map = HashMap::new();
    for entry in &entries {
        if let Some(id) = string(entry, "uuid") {
            uuid_map.insert(id.to_owned(), uuid::Uuid::new_v4().to_string());
        }
    }
    for entry in &mut entries {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        if let Some(old) = object.get("uuid").and_then(Value::as_str)
            && let Some(new) = uuid_map.get(old)
        {
            object.insert("uuid".into(), Value::String(new.clone()));
        }
        if let Some(old) = object.get("parentUuid").and_then(Value::as_str)
            && let Some(new) = uuid_map.get(old)
        {
            object.insert("parentUuid".into(), Value::String(new.clone()));
        }
        if object.contains_key("sessionId") {
            object.insert("sessionId".into(), Value::String(new_session_id.clone()));
        }
        if object.contains_key("session_id") {
            object.insert("session_id".into(), Value::String(new_session_id.clone()));
        }
    }
    let title = match options.title {
        Some(title) if !title.trim().is_empty() => Some(title.trim().to_owned()),
        Some(_) => return Err(Error::Persistence("fork title must be non-empty".into())),
        None => get_session_info(
            session_id,
            GetSessionInfoOptions {
                dir: options.dir.clone(),
            },
        )
        .await?
        .map(|info| format!("{} (fork)", info.custom_title.unwrap_or(info.summary))),
    };
    if let Some(title) = title {
        entries.push(serde_json::json!({
            "type":"custom-title","customTitle":title,"sessionId":new_session_id
        }));
    }
    let destination = source
        .parent()
        .expect("session file has parent")
        .join(format!("{new_session_id}.jsonl"));
    let mut bytes = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut bytes, &entry)?;
        bytes.push(b'\n');
    }
    tokio::fs::write(destination, bytes).await?;
    Ok(ForkSessionResult {
        session_id: new_session_id,
    })
}

async fn append_metadata(session_id: &str, dir: Option<&Path>, value: Value) -> Result<(), Error> {
    validate_session_id(session_id)?;
    let path = find_session_file(session_id, dir)
        .await?
        .ok_or_else(|| Error::Persistence(format!("session {session_id} was not found")))?;
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .await?;
    let mut line = serde_json::to_vec(&value)?;
    line.push(b'\n');
    file.write_all(&line).await?;
    file.flush().await?;
    Ok(())
}

async fn read_session_info(path: &Path, session_id: &str) -> Result<Option<SessionInfo>, Error> {
    let entries = read_json_lines(path).await?;
    if entries
        .iter()
        .any(|entry| entry.get("isSidechain") == Some(&Value::Bool(true)))
    {
        return Ok(None);
    }
    let first_prompt = entries.iter().find_map(first_prompt);
    let custom_title = entries
        .iter()
        .filter_map(|entry| string(entry, "customTitle"))
        .next_back()
        .map(str::to_owned);
    let summary_hint = ["aiTitle", "lastPrompt", "summaryHint", "summary"]
        .into_iter()
        .find_map(|key| {
            entries
                .iter()
                .filter_map(|entry| string(entry, key))
                .next_back()
                .map(str::to_owned)
        });
    let summary = custom_title
        .clone()
        .or(summary_hint)
        .or_else(|| first_prompt.clone());
    let Some(summary) = summary else {
        return Ok(None);
    };
    let metadata = tokio::fs::metadata(path).await?;
    let last_modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    Ok(Some(SessionInfo {
        session_id: session_id.to_owned(),
        summary,
        last_modified,
        file_size: Some(metadata.len()),
        custom_title,
        first_prompt,
        git_branch: last_string(&entries, "gitBranch"),
        cwd: last_string(&entries, "cwd"),
        tag: entries
            .iter()
            .filter(|entry| string(entry, "type") == Some("tag"))
            .filter_map(|entry| entry.get("tag"))
            .next_back()
            .and_then(Value::as_str)
            .map(str::to_owned),
        created_at: entries
            .iter()
            .filter_map(|entry| string(entry, "timestamp"))
            .find_map(parse_timestamp_millis),
        extensions: Extensions::new(),
    }))
}

async fn read_messages(
    path: &Path,
    default_session_id: &str,
    include_system: bool,
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<SessionMessage>, Error> {
    let entries = read_json_lines(path).await?;
    let chain = conversation_chain(&entries);
    let mut messages = Vec::new();
    for entry in chain {
        let Some(message_type) = string(entry, "type") else {
            continue;
        };
        let known_type = match message_type {
            "user" => Some(SessionMessageType::User),
            "assistant" => Some(SessionMessageType::Assistant),
            "system" if include_system => Some(SessionMessageType::System),
            "system" => None,
            _ => {
                if entry.get("message").is_some() {
                    messages.push(SessionMessage::Raw(RawFrame::new(entry.clone())));
                }
                continue;
            }
        };
        let Some(message_type) = known_type else {
            continue;
        };
        let uuid = string(entry, "uuid")
            .ok_or_else(|| Error::Persistence("transcript message omitted uuid".into()))?;
        let message = entry
            .get("message")
            .cloned()
            .ok_or_else(|| Error::Persistence("transcript message omitted message".into()))?;
        let known = [
            "type",
            "uuid",
            "sessionId",
            "session_id",
            "message",
            "parentToolUseId",
            "parent_tool_use_id",
            "parentAgentId",
            "parent_agent_id",
            "parentUuid",
        ];
        let extensions = entry
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| !known.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        messages.push(SessionMessage::Known(KnownSessionMessage {
            message_type,
            uuid: uuid.to_owned(),
            session_id: string(entry, "sessionId")
                .or_else(|| string(entry, "session_id"))
                .unwrap_or(default_session_id)
                .to_owned(),
            message,
            parent_tool_use_id: string(entry, "parentToolUseId")
                .or_else(|| string(entry, "parent_tool_use_id"))
                .map(str::to_owned),
            parent_agent_id: string(entry, "parentAgentId")
                .or_else(|| string(entry, "parent_agent_id"))
                .map(str::to_owned),
            extensions,
        }));
    }
    Ok(page(messages, offset, limit))
}

fn conversation_chain(entries: &[Value]) -> Vec<&Value> {
    let by_uuid = entries
        .iter()
        .filter_map(|entry| string(entry, "uuid").map(|uuid| (uuid, entry)))
        .collect::<HashMap<_, _>>();
    let Some(mut current) = entries
        .iter()
        .rev()
        .find(|entry| entry.get("message").is_some() && string(entry, "uuid").is_some())
    else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    while let Some(uuid) = string(current, "uuid") {
        if !visited.insert(uuid) {
            break;
        }
        result.push(current);
        let Some(parent) = string(current, "parentUuid") else {
            break;
        };
        let Some(next) = by_uuid.get(parent) else {
            break;
        };
        current = next;
    }
    result.reverse();
    result
}

async fn read_json_lines(path: &Path) -> Result<Vec<Value>, Error> {
    let contents = tokio::fs::read_to_string(path).await?;
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| {
                Error::Persistence(format!(
                    "malformed transcript {} line {}: {error}",
                    path.display(),
                    index + 1
                ))
            })
        })
        .collect()
}

async fn find_session_file(session_id: &str, dir: Option<&Path>) -> Result<Option<PathBuf>, Error> {
    for directory in project_directories(dir, Some(true)).await? {
        let path = directory.join(format!("{session_id}.jsonl"));
        if path.is_file() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

async fn project_directories(
    dir: Option<&Path>,
    include_worktrees: Option<bool>,
) -> Result<Vec<PathBuf>, Error> {
    let projects = config_dir()?.join("projects");
    if let Some(dir) = dir {
        let mut paths = vec![absolute_path(dir)?];
        if include_worktrees.unwrap_or(true) {
            paths.extend(git_worktrees(dir).await);
        }
        paths.sort();
        paths.dedup();
        return Ok(paths
            .into_iter()
            .map(|path| projects.join(hash_project_path(&path)))
            .collect());
    }
    if !projects.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut entries = tokio::fs::read_dir(projects).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

async fn git_worktrees(dir: &Path) -> Vec<PathBuf> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .await;
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

async fn transcript_is_programmatic(path: &Path) -> Result<bool, Error> {
    const PROGRAMMATIC: &[&str] = &["sdk-cli", "sdk-ts", "sdk-py", "daemon", "daemon-worker"];
    Ok(read_json_lines(path).await?.iter().any(|entry| {
        string(entry, "entrypoint").is_some_and(|entrypoint| PROGRAMMATIC.contains(&entrypoint))
    }))
}

fn first_prompt(entry: &Value) -> Option<String> {
    if string(entry, "type") != Some("user") {
        return None;
    }
    let message = entry.get("message")?;
    if string(message, "role") != Some("user") {
        return None;
    }
    match message.get("content")? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Value::Array(blocks) => blocks.iter().find_map(|block| {
            (string(block, "type") == Some("text"))
                .then(|| string(block, "text"))
                .flatten()
                .filter(|text| !text.trim().is_empty())
                .map(|text| text.trim().to_owned())
        }),
        _ => None,
    }
}

fn last_string(entries: &[Value], key: &str) -> Option<String> {
    entries
        .iter()
        .filter_map(|entry| string(entry, key))
        .next_back()
        .map(str::to_owned)
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn page<T>(values: Vec<T>, offset: usize, limit: Option<usize>) -> Vec<T> {
    values
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

fn validate_pagination(limit: Option<usize>, offset: usize) -> Result<(), Error> {
    if limit == Some(0) && offset == 0 {
        return Err(Error::Persistence("limit must be positive".into()));
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), Error> {
    if !valid_session_id(session_id) {
        return Err(Error::Persistence("session_id must be a valid UUID".into()));
    }
    Ok(())
}

fn valid_session_id(session_id: &str) -> bool {
    uuid::Uuid::parse_str(session_id).is_ok()
}

fn validate_component(name: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() || value.contains(['/', '\\']) || value == "." || value == ".." {
        return Err(Error::Persistence(format!("invalid {name}")));
    }
    Ok(())
}

fn config_dir() -> Result<PathBuf, Error> {
    if let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(config));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or_else(|| Error::Process("could not determine Claude config directory".into()))?;
    Ok(PathBuf::from(home).join(".claude"))
}

fn absolute_path(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn hash_project_path(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.to_string_lossy().replace(['/', '_'], "-")
}

fn parse_timestamp_millis(timestamp: &str) -> Option<u64> {
    let bytes = timestamp.as_bytes();
    if bytes.len() < 20 || bytes.get(4) != Some(&b'-') || bytes.get(10) != Some(&b'T') {
        return None;
    }
    let number = |start: usize, end: usize| timestamp.get(start..end)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    let fraction = timestamp
        .get(19..)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect::<String>()
        })
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| format!("{digits:0<3}").parse::<i64>().ok())
        .unwrap_or(0);
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    (seconds >= 0).then_some((seconds * 1_000 + fraction) as u64)
}
