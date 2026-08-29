use std::path::PathBuf;

use serde_json::Value;

use crate::{IoEvent, RedactionSummary};

const SECRET_PLACEHOLDER: &str = "<REDACTED>";
const EMAIL_PLACEHOLDER: &str = "<REDACTED_EMAIL>";
const IDENTIFIER_PLACEHOLDER: &str = "<REDACTED_IDENTIFIER>";
const PATH_PLACEHOLDER: &str = "<MACHINE_PATH>";

/// Capture-specific values that supplement the sanitizer's structural rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Redaction {
    pub home: PathBuf,
    pub extra_paths: Vec<PathBuf>,
    pub secret_env: Vec<String>,
    /// Exact JSON field names whose values this capture treats as personal identifiers.
    pub personal_identifier_keys: Vec<String>,
}

/// Strip secrets, machine paths, and personal identifiers from raw traffic.
pub fn sanitize(io: &mut [IoEvent], rules: &Redaction) -> RedactionSummary {
    let mut summary = RedactionSummary::default();
    for event in io {
        match serde_json::from_str::<Value>(&event.line) {
            Ok(mut frame) => {
                sanitize_value(&mut frame, rules, &mut summary);
                event.line = serde_json::to_string(&frame)
                    .expect("serializing a serde_json::Value cannot fail");
            }
            Err(_) => event.line = redact_text(&event.line, rules, &mut summary),
        }
    }
    summary
}

fn sanitize_value(value: &mut Value, rules: &Redaction, summary: &mut RedactionSummary) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_personal_identifier_key(key, rules) && !value.is_null() {
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
                    sanitize_value(value, rules, summary);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                sanitize_value(value, rules, summary);
            }
        }
        Value::String(value) => *value = redact_text(value, rules, summary),
        _ => {}
    }
}

fn is_personal_identifier_key(key: &str, rules: &Redaction) -> bool {
    rules
        .personal_identifier_keys
        .iter()
        .any(|identifier_key| identifier_key == key)
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
        _ => sanitize_value(value, rules, summary),
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
            | "email"
            | "organization"
            | "userid"
            | "accountid"
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

fn redact_text(input: &str, rules: &Redaction, summary: &mut RedactionSummary) -> String {
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
    redact_windows_user_paths(&value, summary)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IoDirection, IoEvent};

    fn event(line: Value) -> IoEvent {
        IoEvent {
            us: 0,
            direction: IoDirection::Read,
            line: serde_json::to_string(&line).unwrap(),
            transport_id: None,
            session_id: None,
        }
    }

    #[test]
    fn sanitize_redacts_nested_values_and_reports_each_category() {
        let mut io = vec![
            event(serde_json::json!({
                "type": "control_request",
                "api_key": "sk-ant-secret",
                "cwd": "/Users/alice/project",
                "message": {
                    "note": "email alice@example.com and token exact-secret",
                    "paths": ["/srv/operator/project", "relative/file"]
                },
                "installationId": "2b93020b-f38b-47de-ae2f-d9885611b5f0",
                "serverName": "Alices-Laptop.local"
            })),
            event(serde_json::json!({
                "type": "system",
                "text": "read /tmp/work/file with Bearer abc123"
            })),
        ];
        let rules = Redaction {
            home: PathBuf::from("/Users/alice"),
            extra_paths: vec![PathBuf::from("/srv/operator")],
            secret_env: vec!["exact-secret".to_string()],
            personal_identifier_keys: vec!["installationId".into(), "serverName".into()],
        };

        let summary = sanitize(&mut io, &rules);
        let sanitized = io
            .iter()
            .map(|event| event.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!sanitized.contains("sk-ant-secret"));
        assert!(!sanitized.contains("alice@example.com"));
        assert!(!sanitized.contains("exact-secret"));
        assert!(!sanitized.contains("/Users/alice"));
        assert!(!sanitized.contains("/srv/operator"));
        assert!(!sanitized.contains("/tmp/work"));
        assert!(!sanitized.contains("abc123"));
        assert!(sanitized.contains(SECRET_PLACEHOLDER));
        assert!(sanitized.contains(EMAIL_PLACEHOLDER));
        assert!(sanitized.contains(IDENTIFIER_PLACEHOLDER));
        assert!(sanitized.contains(PATH_PLACEHOLDER));
        assert_eq!(
            summary,
            RedactionSummary {
                secrets: 3,
                machine_paths: 3,
                personal_identifiers: 3,
            }
        );

        let repeated = io.clone();
        assert_eq!(sanitize(&mut io, &rules), RedactionSummary::default());
        assert_eq!(io, repeated, "sanitization must be idempotent");
    }

    #[test]
    fn sanitize_scopes_identifier_keys_to_the_capture_and_exact_field_name() {
        let mut io = vec![event(serde_json::json!({
            "remoteControl": {
                "serverName": "Alices-Laptop.local"
            },
            "mcp": {
                "server_name": "spec"
            }
        }))];
        let rules = Redaction {
            personal_identifier_keys: vec!["serverName".into()],
            ..Redaction::default()
        };

        assert_eq!(
            sanitize(&mut io, &rules),
            RedactionSummary {
                personal_identifiers: 1,
                ..RedactionSummary::default()
            }
        );
        let sanitized: Value = serde_json::from_str(&io[0].line).unwrap();
        assert_eq!(
            sanitized["remoteControl"]["serverName"],
            IDENTIFIER_PLACEHOLDER
        );
        assert_eq!(sanitized["mcp"]["server_name"], "spec");
    }

    #[test]
    fn sanitize_preserves_package_and_version_identifiers() {
        let mut io = vec![event(serde_json::json!({
            "values": [
                "agent-sdk@0.3.247",
                "stripe@claude-plugins-official",
                "rust-analyzer-lsp@1.0.0"
            ]
        }))];

        assert_eq!(
            sanitize(&mut io, &Redaction::default()),
            RedactionSummary::default()
        );
        assert!(io[0].line.contains("agent-sdk@0.3.247"));
    }
}
