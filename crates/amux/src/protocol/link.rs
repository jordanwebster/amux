//! Link names — the ASCII-level tokens that make up a `Route` stack.
//!
//! A link identifies one hop (a named edge between amux servers or between a
//! server and a terminal). Link names are validated at construction: they must
//! be non-empty and must not contain `'.'`, which is the reserved `Route`
//! separator. Deserialization validates on the wire.

use std::borrow::Borrow;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A single route-hop link name. Non-empty, no `'.'`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Link(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidLinkName {
    #[error("link name must not be empty")]
    Empty,
    #[error("link name exceeds 128 byte limit: {0} bytes")]
    TooLong(usize),
    #[error("link name must not contain '.' (route separator): {0}")]
    ReservedSeparator(String),
    #[error(
        "link name must contain only ASCII alphanumeric, hyphen, or underscore characters: {0}"
    )]
    InvalidCharacters(String),
}

impl Link {
    /// Validate and wrap a link name.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidLinkName> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidLinkName::Empty);
        }
        if value.len() > 128 {
            return Err(InvalidLinkName::TooLong(value.len()));
        }
        if value.contains('.') {
            return Err(InvalidLinkName::ReservedSeparator(value));
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(InvalidLinkName::InvalidCharacters(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Link {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Enables `HashMap<Link, _>::get(&str)` and `HashSet<Link>::contains(&str)`.
impl Borrow<str> for Link {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Serialize for Link {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Link {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_rejected() {
        assert_eq!(Link::new(""), Err(InvalidLinkName::Empty));
    }

    #[test]
    fn dotted_is_rejected() {
        assert_eq!(
            Link::new("a.b"),
            Err(InvalidLinkName::ReservedSeparator("a.b".to_string()))
        );
    }

    #[test]
    fn invalid_characters_are_rejected() {
        assert_eq!(
            Link::new("bad link"),
            Err(InvalidLinkName::InvalidCharacters("bad link".to_string()))
        );
        assert_eq!(
            Link::new("cafe\u{e9}"),
            Err(InvalidLinkName::InvalidCharacters("cafe\u{e9}".to_string()))
        );
    }

    #[test]
    fn overlong_names_are_rejected() {
        let value = "a".repeat(129);
        assert_eq!(Link::new(value), Err(InvalidLinkName::TooLong(129)));
    }

    #[test]
    fn valid_name_roundtrips() {
        let link = Link::new("host-abc").unwrap();
        let s = serde_json::to_string(&link).unwrap();
        assert_eq!(s, "\"host-abc\"");
        let back: Link = serde_json::from_str(&s).unwrap();
        assert_eq!(back, link);
    }

    #[test]
    fn deserialize_rejects_dotted() {
        let err: Result<Link, _> = serde_json::from_str("\"a.b\"");
        assert!(err.is_err());
    }
}
