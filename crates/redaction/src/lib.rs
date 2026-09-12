use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

pub const SECRET_PLACEHOLDER: &str = "<REDACTED>";
pub const EMAIL_PLACEHOLDER: &str = "<REDACTED_EMAIL>";
pub const IDENTIFIER_PLACEHOLDER: &str = "<REDACTED_IDENTIFIER>";
pub const PATH_PLACEHOLDER: &str = "<MACHINE_PATH>";

/// Capture-specific values that supplement the sanitizer's structural rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Redaction {
    pub home: PathBuf,
    pub extra_paths: Vec<PathBuf>,
    pub secret_env: Vec<String>,
    /// Host name whose literal appearances should not leave the capture.
    pub hostname: Option<String>,
    /// Local user name whose literal appearances should not leave the capture.
    pub user: Option<String>,
    /// Other exact identifiers whose literal appearances should not leave the capture.
    pub extra_personal_identifiers: Vec<String>,
    /// Exact JSON field names whose values this capture treats as personal identifiers.
    pub personal_identifier_keys: Vec<String>,
}

/// Counts the sensitive values replaced while producing a diagnostic report.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionSummary {
    #[serde(default)]
    pub secrets: u64,
    #[serde(default)]
    pub machine_paths: u64,
    #[serde(default)]
    pub personal_identifiers: u64,
}

/// Redact one parsed JSON value with the shared capture rules.
pub fn redact_value(value: &mut Value, rules: &Redaction, summary: &mut RedactionSummary) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if should_redact_personal_identifier(key, rules) && !value.is_null() {
                    if value.as_str() != Some(IDENTIFIER_PLACEHOLDER) {
                        *value = Value::String(IDENTIFIER_PLACEHOLDER.to_string());
                        summary.personal_identifiers += 1;
                    }
                } else if is_sensitive_key(key) && !value.is_null() {
                    if value.as_str() != Some(SECRET_PLACEHOLDER) {
                        *value = Value::String(SECRET_PLACEHOLDER.to_string());
                        summary.secrets += 1;
                    }
                } else if is_path_key(key) {
                    sanitize_path_value(value, rules, summary);
                } else {
                    redact_value(value, rules, summary);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_value(value, rules, summary);
            }
        }
        Value::String(value) => *value = redact_text(value, rules, summary),
        _ => {}
    }
}

fn should_redact_personal_identifier(key: &str, rules: &Redaction) -> bool {
    is_personal_identifier_key(key)
        || rules
            .personal_identifier_keys
            .iter()
            .any(|identifier_key| identifier_key == key)
}

/// Whether a JSON field name conventionally identifies a person or their account.
///
/// Capture-specific field names remain on [`Redaction::personal_identifier_keys`].
/// This shared rule is public so evidence tooling can reject the same account,
/// organization, user, and bridge identifiers that corpus sanitization removes.
pub fn is_personal_identifier_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    normalized == "bridgesessionid"
        || matches!(
            normalized.as_str(),
            "email" | "userid" | "useruuid" | "organization"
        )
        || ((normalized.contains("account") || normalized.contains("organization"))
            && (normalized.ends_with("id")
                || normalized.ends_with("uuid")
                || normalized.ends_with("name")))
}

fn sanitize_path_value(value: &mut Value, rules: &Redaction, summary: &mut RedactionSummary) {
    match value {
        Value::String(path)
            if path != PATH_PLACEHOLDER && is_absolute_machine_path(path.as_str()) =>
        {
            *path = PATH_PLACEHOLDER.to_string();
            summary.machine_paths += 1;
        }
        Value::Array(values) => {
            for value in values {
                sanitize_path_value(value, rules, summary);
            }
        }
        _ => redact_value(value, rules, summary),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "token"
            | "apikey"
            | "anthropicapikey"
            | "authorization"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "password"
            | "passwd"
            | "cookie"
            | "setcookie"
            | "clientsecret"
            | "secretkey"
            | "credential"
    ) || normalized.ends_with("apikey")
}

fn is_path_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "cwd" | "path" | "filepath" | "workingdirectory" | "homedir" | "projectdir" | "rootdir"
    ) || normalized.ends_with("path")
        || normalized.ends_with("directory")
        || normalized.ends_with("directories")
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_absolute_machine_path(value: &str) -> bool {
    value.starts_with('/')
        || (value.len() >= 3
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'\\' | b'/'))
}

/// Redact secrets, machine paths, and local machine identifiers in free text.
pub fn redact_text(input: &str, rules: &Redaction, summary: &mut RedactionSummary) -> String {
    let mut value = input.to_string();
    for secret in &rules.secret_env {
        if !secret.is_empty() && secret != SECRET_PLACEHOLDER {
            value = redact_literal(&value, secret, SECRET_PLACEHOLDER, &mut summary.secrets);
        }
    }
    for marker in [
        "sk-ant-",
        "Bearer ",
        "ANTHROPIC_API_KEY=",
        "CLAUDE_CODE_OAUTH_TOKEN=",
    ] {
        value = redact_spans(&value, marker, SECRET_PLACEHOLDER, &mut summary.secrets);
    }
    value = redact_email_addresses(&value, summary);

    let mut paths = Vec::with_capacity(rules.extra_paths.len() + 1);
    if !rules.home.as_os_str().is_empty() {
        paths.push(rules.home.to_string_lossy().into_owned());
    }
    paths.extend(
        rules
            .extra_paths
            .iter()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.to_string_lossy().into_owned()),
    );
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    paths.dedup();
    for path in paths {
        value = redact_spans(&value, &path, PATH_PLACEHOLDER, &mut summary.machine_paths);
    }
    for marker in [
        "/Users/",
        "/home/",
        "/private/var/folders/",
        "/var/folders/",
        "/tmp/",
        "/Volumes/",
        "/workspace/",
        "/root/",
        "/opt/",
        "/usr/local/",
    ] {
        value = redact_spans(&value, marker, PATH_PLACEHOLDER, &mut summary.machine_paths);
    }
    value = redact_windows_user_paths(&value, summary);
    for identifier in &rules.extra_personal_identifiers {
        if !identifier.is_empty() && identifier != IDENTIFIER_PLACEHOLDER {
            value = redact_width_matched_identifier(&value, identifier, summary);
        }
    }
    for identifier in [&rules.hostname, &rules.user].into_iter().flatten() {
        if !identifier.is_empty() && identifier != IDENTIFIER_PLACEHOLDER {
            value = redact_literal(
                &value,
                identifier,
                IDENTIFIER_PLACEHOLDER,
                &mut summary.personal_identifiers,
            );
        }
    }
    value
}

fn redact_width_matched_identifier(
    input: &str,
    identifier: &str,
    summary: &mut RedactionSummary,
) -> String {
    const LABEL: &str = "<REDACTED>";

    let width = UnicodeWidthStr::width(identifier);
    let label_width = UnicodeWidthStr::width(LABEL);
    let replacement = if width >= label_width {
        format!("{LABEL}{}", "_".repeat(width - label_width))
    } else {
        "#".repeat(width)
    };
    if replacement == identifier {
        return input.to_string();
    }
    redact_literal(
        input,
        identifier,
        &replacement,
        &mut summary.personal_identifiers,
    )
}

fn redact_literal(input: &str, needle: &str, replacement: &str, count: &mut u64) -> String {
    let matches = input.matches(needle).count() as u64;
    if matches == 0 {
        return input.to_string();
    }
    *count += matches;
    input.replace(needle, replacement)
}

fn redact_spans(input: &str, marker: &str, replacement: &str, count: &mut u64) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find(marker) {
        output.push_str(&remainder[..start]);
        let token = &remainder[start..];
        let end = token
            .char_indices()
            .skip_while(|(index, _)| *index < marker.len())
            .find(|(_, character)| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
            })
            .map(|(index, _)| index)
            .unwrap_or(token.len());
        output.push_str(replacement);
        remainder = &token[end..];
        *count += 1;
    }
    output.push_str(remainder);
    output
}

fn is_email_local_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
}

fn is_email_domain(domain: &str) -> bool {
    let Some((_, top_level)) = domain.rsplit_once('.') else {
        return false;
    };
    top_level.len() >= 2 && top_level.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn redact_email_addresses(input: &str, summary: &mut RedactionSummary) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut kept = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        let mut start = index;
        while start > kept && is_email_local_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < bytes.len() && is_email_domain_byte(bytes[end]) {
            end += 1;
        }
        while end > index + 1 && bytes[end - 1] == b'.' {
            end -= 1;
        }
        if start == index || !is_email_domain(&input[index + 1..end]) {
            index += 1;
            continue;
        }
        output.push_str(&input[kept..start]);
        output.push_str(EMAIL_PLACEHOLDER);
        summary.personal_identifiers += 1;
        kept = end;
        index = end;
    }
    output.push_str(&input[kept..]);
    output
}

fn redact_windows_user_paths(input: &str, summary: &mut RedactionSummary) -> String {
    let bytes = input.as_bytes();
    for (index, character) in input.char_indices() {
        if index + 9 > bytes.len() {
            break;
        }
        if character.is_ascii_alphabetic()
            && bytes.get(index + 1) == Some(&b':')
            && matches!(bytes.get(index + 2), Some(b'\\') | Some(b'/'))
            && bytes[index + 3..index + 8].eq_ignore_ascii_case(b"users")
            && matches!(bytes.get(index + 8), Some(b'\\') | Some(b'/'))
        {
            let marker = &input[index..index + 9];
            return redact_spans(input, marker, PATH_PLACEHOLDER, &mut summary.machine_paths);
        }
    }
    input.to_string()
}
