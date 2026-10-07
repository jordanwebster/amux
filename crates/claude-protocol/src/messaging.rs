//! The lines amux writes to Claude Code's messaging socket: an auth line
//! carrying the socket's token, then one user message. Fields are declared
//! in the order Claude has always been sent them.

use serde::Serialize;

/// The first line on a messaging socket.
#[derive(Debug, Clone, Serialize)]
pub struct Auth<'a> {
    pub token: &'a str,
    #[serde(rename = "type")]
    pub kind: AuthKind,
}

impl<'a> Auth<'a> {
    pub fn new(token: &'a str) -> Self {
        Self {
            token,
            kind: AuthKind::Auth,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    Auth,
}

/// A message for the session, sent as if the user had typed it.
#[derive(Debug, Clone, Serialize)]
pub struct UserMessage<'a> {
    pub message: UserContent<'a>,
    #[serde(rename = "type")]
    pub kind: UserKind,
}

impl<'a> UserMessage<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            message: UserContent {
                content: text,
                role: UserKind::User,
            },
            kind: UserKind::User,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UserContent<'a> {
    pub content: &'a str,
    pub role: UserKind,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserKind {
    User,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_serialize_as_claude_reads_them() {
        assert_eq!(
            serde_json::to_string(&Auth::new("secret")).unwrap(),
            r#"{"token":"secret","type":"auth"}"#
        );
        assert_eq!(
            serde_json::to_string(&UserMessage::new("hello")).unwrap(),
            r#"{"message":{"content":"hello","role":"user"},"type":"user"}"#
        );
    }
}
