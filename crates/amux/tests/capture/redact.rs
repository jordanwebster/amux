//! Redaction for captured transcript streams before fixture graduation.
//!
//! Two jobs, in order:
//!
//! 1. **Drop config-bearing rows.** Claude Code injects the *user's own*
//!    configuration into the transcript as attachment rows — the installed
//!    skill listing (names + full descriptions), the agent listing, and the
//!    deferred-tools inventory (every MCP/connector tool the user has,
//!    e.g. their Gmail/Calendar tools). None of that is scenario structure;
//!    all of it is the owner's private config. These rows are removed whole
//!    (see [`CONFIG_ATTACHMENT_TYPES`]).
//! 2. **Scrub identifying substrings** from the rows that remain: the scratch
//!    root (`[SCRATCH]`/`[SCRATCH-SLUG]`), the home dir (`[HOME]`/`[HOME-SLUG]`),
//!    the username token (`[USER]`), the hostname (`[HOST]`).
//!
//! Then the output is *verified*: any surviving config-bearing row, home
//! dir, username token, tool-inventory / skill-description marker, or
//! credential-shaped substring fails redaction loudly rather than committing
//! a leak.

use std::path::Path;

use anyhow::{Result, bail};

/// Attachment `attachment.type` values that carry the user's own
/// configuration rather than scenario structure. Removed whole during
/// redaction and rejected by [`verify`].
///
/// - `skill_listing` — installed skills, names + full descriptions.
/// - `agent_listing_delta` — configured subagents, names + descriptions.
/// - `deferred_tools_delta` — the MCP/connector tool inventory (tool ids
///   like `mcp__…`, incl. the user's Gmail/Calendar/etc. tools).
///
/// Plan-mode attachments (`plan_mode`, `plan_mode_exit`), permission facts
/// (`command_permissions`), and turn/tool attachments stay: they are the
/// scenario.
const CONFIG_ATTACHMENT_TYPES: &[&str] = &[
    "skill_listing",
    "agent_listing_delta",
    "deferred_tools_delta",
];

/// True if a raw transcript line is a config-bearing attachment row.
fn is_config_attachment_line(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    if value.get("type").and_then(|t| t.as_str()) != Some("attachment") {
        return false;
    }
    value
        .get("attachment")
        .and_then(|a| a.get("type"))
        .and_then(|t| t.as_str())
        .is_some_and(|t| CONFIG_ATTACHMENT_TYPES.contains(&t))
}

/// Claude Code's project-directory identifier form: a path with every `/`
/// and `.` turned into `-` (e.g. `-private-var-folders-...-projects-pong`).
fn slugify(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

/// Replace whole-word occurrences of `needle` (bounded by non-alphanumeric,
/// non-`_`/`-` characters) with `replacement`. Avoids clobbering the token
/// when it is a substring of a longer identifier.
fn replace_word(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            let before_ok = i == 0 || !haystack[..i].chars().next_back().is_some_and(is_word);
            let after = i + needle.len();
            let after_ok =
                after >= haystack.len() || !haystack[after..].chars().next().is_some_and(is_word);
            if before_ok && after_ok {
                out.push_str(replacement);
                i = after;
                continue;
            }
        }
        // Advance one char (UTF-8 safe).
        let ch_len = haystack[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&haystack[i..i + ch_len]);
        i += ch_len;
    }
    out
}

pub fn redact(raw: &str, scratch_root: &Path) -> Result<String> {
    // 1. Drop config-bearing attachment rows whole.
    let kept: Vec<&str> = raw
        .lines()
        .filter(|line| line.trim().is_empty() || !is_config_attachment_line(line))
        .collect();
    let mut out = kept.join("\n");
    if raw.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }

    // 2. Scrub identifying substrings from what remains.
    let scratch = scratch_root.display().to_string();
    let scratch_private = format!("/private{scratch}");
    // Longest first so the `/private` form never leaves a `/private` stub.
    out = out.replace(&scratch_private, "[SCRATCH]");
    out = out.replace(&scratch, "[SCRATCH]");
    // JSON-escaped variants (paths inside string values keep plain slashes on
    // macOS, but be thorough about backslash-escaped forms).
    out = out.replace(&scratch_private.replace('/', "\\/"), "[SCRATCH]");
    out = out.replace(&scratch.replace('/', "\\/"), "[SCRATCH]");
    // The dash-slugified project-dir form claude uses in `~/.claude/projects`.
    out = out.replace(&slugify(&scratch_private), "[SCRATCH-SLUG]");
    out = out.replace(&slugify(&scratch), "[SCRATCH-SLUG]");

    if let Ok(home) = std::env::var("HOME") {
        let home_private = format!("/private{home}");
        out = out.replace(&home_private, "[HOME]");
        out = out.replace(&home, "[HOME]");
        out = out.replace(&slugify(&home_private), "[HOME-SLUG]");
        out = out.replace(&slugify(&home), "[HOME-SLUG]");
    }
    // Whole-word: the username as a standalone token (e.g. the owner column
    // of an `ls -l` that a Bash tool_result captured), not as a substring of
    // some hash. Path/slug forms are covered above.
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
    {
        out = replace_word(&out, &user, "[USER]");
    }
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    if !hostname.is_empty() {
        out = out.replace(&hostname, "[HOST]");
    }

    // 3. Neutralize tool-inventory *counts* that leak how many tools/skills the
    //    user has configured (e.g. a `ToolSearch` result's
    //    `"total_deferred_tools":56`). Keep the field shape, zero the value.
    out = neutralize_count(&out, "total_deferred_tools");

    verify(&out, scratch_root)?;
    Ok(out)
}

/// Replace the integer value that follows `"<key>":` with `0`, keeping valid
/// JSON while removing the leaked count. Handles optional whitespace after the
/// colon.
fn neutralize_count(input: &str, key: &str) -> String {
    let needle = format!("\"{key}\":");
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(&needle) {
        let (before, after) = rest.split_at(pos + needle.len());
        out.push_str(before);
        let trimmed = after.trim_start_matches([' ', '\t']);
        let ws = &after[..after.len() - trimmed.len()];
        out.push_str(ws);
        let digits_end = trimmed
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(trimmed.len());
        if digits_end > 0 {
            out.push('0');
            rest = &trimmed[digits_end..];
        } else {
            // Not a bare integer (unexpected) — leave as-is and move on.
            rest = trimmed;
        }
    }
    out.push_str(rest);
    out
}

/// Fail loudly if anything identifying, config-bearing, or credential-shaped
/// survived.
fn verify(redacted: &str, scratch_root: &Path) -> Result<()> {
    let mut violations = Vec::new();

    // No config-bearing attachment row may survive.
    for line in redacted.lines() {
        if is_config_attachment_line(line) {
            violations.push("config-bearing attachment row survived the strip".to_string());
            break;
        }
    }
    // Tool-inventory / skill-listing content must not leak through any other
    // row shape either. `mcp__` is the MCP tool-id prefix (none of the
    // scenarios legitimately invoke an MCP tool); the field names are the
    // structural signatures of the stripped listings.
    for marker in [
        "mcp__",
        "skill_listing",
        "agent_listing_delta",
        "deferred_tools_delta",
        "\"skillCount\"",
        "\"addedNames\"",
        "\"addedTypes\"",
    ] {
        if redacted.contains(marker) {
            violations.push(format!("tool/skill-inventory marker '{marker}' present"));
        }
    }
    // Tool-inventory counts must have been neutralized to 0.
    if redacted != neutralize_count(redacted, "total_deferred_tools") {
        violations.push("non-zero 'total_deferred_tools' count present".to_string());
    }

    let scratch = scratch_root.display().to_string();
    for form in [scratch.clone(), slugify(&scratch)] {
        if redacted.contains(&form) {
            violations.push(format!("scratch path form '{form}' still present"));
        }
    }
    if let Ok(home) = std::env::var("HOME")
        && redacted.contains(&home)
    {
        violations.push(format!("home dir '{home}' still present"));
    }
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
        && redacted != replace_word(redacted, &user, "[USER]")
    {
        violations.push(format!("username token '{user}' still present"));
    }
    for needle in [
        "sk-ant-",
        "oauth_token",
        "OAUTH_TOKEN",
        "Bearer ",
        "ANTHROPIC_API_KEY",
    ] {
        if redacted.contains(needle) {
            violations.push(format!("credential-shaped substring '{needle}' present"));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!("redaction verification failed: {violations:?}")
    }
}
