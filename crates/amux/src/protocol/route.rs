//! Link-based stack routing for amux.
//!
//! A route names a concrete tunnel/path through the network. Host IDs identify
//! the remote endpoint; routes identify the exact path used to reach it and
//! therefore the tunnel whose routed RPC state is affected when that path
//! disappears.
//!
//! Routes are stacks of link names that get popped/pushed at each hop:
//! - Before sending through link X: pop X from dst, push X to src
//! - On receive: match dst.pop() { None → process locally, Some(link) → route to link }

use std::collections::VecDeque;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::protocol::link::Link;

/// Alphabet for generating random link suffixes (lowercase + digits)
const LINK_ALPHABET: [char; 36] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
];

/// A route is a stack of link names.
///
/// The top of the stack (front of deque) is the next hop.
/// Serializes as "AB.BC.CD" where AB is the top (first hop).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Route {
    links: VecDeque<Link>,
}

impl Route {
    /// Create an empty route (no hops).
    /// Used in routing events to indicate the agent or host is local to the sender.
    pub fn empty() -> Self {
        Self {
            links: VecDeque::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Number of hops in this route.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Create a route with a single link.
    pub fn from_link(link: Link) -> Self {
        let mut links = VecDeque::new();
        links.push_back(link);
        Self { links }
    }

    pub(crate) fn from_links(
        links: impl IntoIterator<Item = String>,
    ) -> Result<Self, crate::protocol::link::InvalidLinkName> {
        let links = links
            .into_iter()
            .map(Link::new)
            .collect::<Result<VecDeque<_>, _>>()?;
        Ok(Self { links })
    }

    /// Push a link onto the front of the route (becomes the new top).
    pub fn push(&mut self, link: Link) {
        self.links.push_front(link);
    }

    /// Pop the top link from the route (the next hop).
    pub fn pop(&mut self) -> Option<Link> {
        self.links.pop_front()
    }

    /// Peek at the first hop without consuming it.
    pub fn peek(&self) -> Option<&Link> {
        self.links.front()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Link> {
        self.links.iter()
    }

    /// Check if this route passes through a given link name.
    pub fn contains_link(&self, link: &str) -> bool {
        self.links.iter().any(|l| l.as_str() == link)
    }

    /// Check if this route starts with all links from the given prefix route, in order.
    /// An empty prefix matches any route.
    pub fn starts_with_route(&self, prefix: &Route) -> bool {
        if prefix.links.len() > self.links.len() {
            return false;
        }
        self.links
            .iter()
            .zip(prefix.links.iter())
            .all(|(a, b)| a == b)
    }

    /// Replace a matching route prefix in-place, preserving the remaining suffix.
    ///
    /// Returns false when `old_prefix` does not match the start of this route.
    pub fn replace_prefix(&mut self, old_prefix: &Route, new_prefix: &Route) -> bool {
        if !self.starts_with_route(old_prefix) {
            return false;
        }

        self.links = new_prefix
            .links
            .iter()
            .cloned()
            .chain(self.links.iter().skip(old_prefix.links.len()).cloned())
            .collect();
        true
    }

    /// Prepare to send a message toward a destination. Pops the next hop from dst
    /// and creates a single-link src from it. Returns (src, dst) ready for the message.
    ///
    /// At each forwarding hop, the server calls `send(remaining_dst)` to consume
    /// one link from dst and record it in src. The receiver's src thus accumulates
    /// the full return path.
    ///
    /// Returns None if dst is empty (message has arrived at its destination).
    pub fn send(mut dst: Route) -> Option<(Route, Route)> {
        let next_hop = dst.pop()?;
        let src = Route::from_link(next_hop);
        Some((src, dst))
    }

    /// Prepare a reply by sending back through the path the message came from.
    ///
    /// This is literally `send(src)` — the same pop-and-push operation applied to
    /// the incoming src route. It works because src accumulated each hop's link name
    /// as the original message traveled inward, so "sending" through src naturally
    /// reverses the path. This symmetry is the entire routing algorithm.
    ///
    /// Returns None if src is empty (no return path — the message originated locally).
    pub fn reply(src: Route) -> Option<(Route, Route)> {
        Route::send(src)
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self
            .links
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(".");
        f.write_str(&s)
    }
}

impl Serialize for Route {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Route {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            // Empty routes only arise from popping, but we need to accept them on the wire
            return Ok(Route {
                links: VecDeque::new(),
            });
        }
        let links: Result<VecDeque<Link>, _> = s.split('.').map(Link::new).collect();
        Ok(Route {
            links: links.map_err(serde::de::Error::custom)?,
        })
    }
}

/// Generate a random 4-character link suffix.
fn generate_link_suffix() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes[..4]
        .iter()
        .map(|b| LINK_ALPHABET[(*b as usize) % LINK_ALPHABET.len()])
        .collect()
}

/// Sanitize a hostname for use in link names.
/// Keeps only the ASCII characters accepted by `Link`; every other character
/// becomes `-`.
fn sanitize_host_name(host_name: &str) -> String {
    host_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Generate a server link name: "{hostname}" or "{hostname}-{rand}".
/// If randomise is true, appends a random suffix for uniqueness.
/// The hostname is sanitized and truncated if needed.
pub(crate) fn generate_server_link(host_name: &str, randomise: bool) -> Link {
    let sanitized = sanitize_host_name(host_name);
    let sanitized = if sanitized.is_empty() {
        "host".to_string()
    } else {
        sanitized
    };
    let max_base_len = if randomise { 123 } else { 128 };
    let sanitized: String = sanitized.chars().take(max_base_len).collect();
    let raw = if randomise {
        format!("{}-{}", sanitized, generate_link_suffix())
    } else {
        sanitized
    };
    Link::new(raw).expect("generated server link is well-formed")
}

/// Generate a terminal link name: "term-{rand}".
pub(crate) fn generate_terminal_link() -> Link {
    Link::new(format!("term-{}", generate_link_suffix()))
        .expect("generated terminal link is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    #[test]
    fn test_route_empty() {
        let mut route = Route::empty();
        assert_eq!(route.pop(), None);

        // Push onto empty produces single-link route
        route.push(link("host-a"));
        assert_eq!(route.pop(), Some(link("host-a")));
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_empty_serialize() {
        let route = Route::empty();
        let serialized = serde_json::to_string(&route).unwrap();
        assert_eq!(serialized, "\"\"");
    }

    #[test]
    fn test_route_push_pop() {
        // Build route AB.BC.CD by starting with CD and pushing
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));

        assert_eq!(route.pop(), Some(link("AB")));
        assert_eq!(route.pop(), Some(link("BC")));
        assert_eq!(route.pop(), Some(link("CD")));
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_from_link() {
        let mut route = Route::from_link(link("single"));
        assert_eq!(route.pop(), Some(link("single")));
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_serialize() {
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));

        let serialized = serde_json::to_string(&route).unwrap();
        assert_eq!(serialized, "\"AB.BC.CD\"");
    }

    #[test]
    fn test_route_serialize_single() {
        let route = Route::from_link(link("AB"));
        let serialized = serde_json::to_string(&route).unwrap();
        assert_eq!(serialized, "\"AB\"");
    }

    #[test]
    fn test_route_deserialize() {
        let route: Route = serde_json::from_str("\"AB.BC.CD\"").unwrap();
        let mut route = route;
        assert_eq!(route.pop(), Some(link("AB")));
        assert_eq!(route.pop(), Some(link("BC")));
        assert_eq!(route.pop(), Some(link("CD")));
    }

    #[test]
    fn test_route_deserialize_empty() {
        let mut route: Route = serde_json::from_str("\"\"").unwrap();
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_roundtrip() {
        let mut original = Route::from_link(link("CD"));
        original.push(link("BC"));
        original.push(link("AB"));

        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: Route = serde_json::from_str(&serialized).unwrap();

        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_generate_link_suffix() {
        let suffix = generate_link_suffix();
        assert_eq!(suffix.len(), 4);
        for c in suffix.chars() {
            assert!(c.is_ascii_lowercase() || c.is_ascii_digit());
        }
    }

    #[test]
    fn test_generate_server_link_randomised() {
        let link = generate_server_link("myhost", true);
        assert!(link.as_str().starts_with("myhost-"));
        assert_eq!(link.as_str().len(), "myhost-".len() + 4);
    }

    #[test]
    fn test_generate_server_link_deterministic() {
        let link = generate_server_link("myhost", false);
        assert_eq!(link.as_str(), "myhost");
    }

    #[test]
    fn test_generate_terminal_link() {
        let link = generate_terminal_link();
        assert!(link.as_str().starts_with("term-"));
        assert_eq!(link.as_str().len(), "term-".len() + 4);
    }

    #[test]
    fn test_sanitize_host_name_with_periods() {
        assert_eq!(sanitize_host_name("my.laptop.local"), "my-laptop-local");
        assert_eq!(
            sanitize_host_name("server.example.com"),
            "server-example-com"
        );
    }

    #[test]
    fn test_sanitize_host_name_no_periods() {
        assert_eq!(sanitize_host_name("myhost"), "myhost");
        assert_eq!(sanitize_host_name("my-host"), "my-host");
    }

    #[test]
    fn test_sanitize_host_name_replaces_invalid_link_characters() {
        assert_eq!(sanitize_host_name("my host"), "my-host");
        assert_eq!(sanitize_host_name("café"), "caf-");
    }

    #[test]
    fn test_generate_server_link_truncates_to_link_limit() {
        let long = "a".repeat(200);

        let deterministic = generate_server_link(&long, false);
        assert_eq!(deterministic.as_str().len(), 128);

        let randomised = generate_server_link(&long, true);
        assert_eq!(randomised.as_str().len(), 128);
        assert_eq!(randomised.as_str().chars().nth(123), Some('-'));
    }

    #[test]
    fn test_generate_server_link_sanitizes_periods() {
        let link = generate_server_link("my.laptop.local", false);
        assert_eq!(link.as_str(), "my-laptop-local");
        assert!(!link.as_str().contains('.'));

        let link = generate_server_link("my.laptop.local", true);
        assert!(link.as_str().starts_with("my-laptop-local-"));
        assert!(!link.as_str().contains('.'));
    }

    #[test]
    fn link_name_rejects_period() {
        assert!(Link::new("bad.link").is_err());
    }

    #[test]
    fn test_contains_link() {
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));

        assert!(route.contains_link("AB"));
        assert!(route.contains_link("BC"));
        assert!(route.contains_link("CD"));
        assert!(!route.contains_link("XX"));
    }

    #[test]
    fn test_contains_link_empty() {
        let route = Route::empty();
        assert!(!route.contains_link("any"));
    }

    #[test]
    fn test_starts_with_route_exact_match() {
        let route = Route::from_link(link("AB"));
        let prefix = Route::from_link(link("AB"));
        assert!(route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_partial_prefix() {
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));
        let prefix = Route::from_link(link("AB"));
        assert!(route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_mismatch() {
        let route = Route::from_link(link("AB"));
        let prefix = Route::from_link(link("XX"));
        assert!(!route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_empty_prefix() {
        let route = Route::from_link(link("AB"));
        assert!(route.starts_with_route(&Route::empty()));
    }

    #[test]
    fn test_starts_with_route_longer_prefix() {
        let route = Route::from_link(link("AB"));
        let mut prefix = Route::from_link(link("CD"));
        prefix.push(link("AB"));
        assert!(!route.starts_with_route(&prefix));
    }

    #[test]
    fn test_replace_prefix() {
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));

        let mut old_prefix = Route::from_link(link("BC"));
        old_prefix.push(link("AB"));
        let new_prefix = Route::from_link(link("XY"));

        assert!(route.replace_prefix(&old_prefix, &new_prefix));
        assert_eq!(route.to_string(), "XY.CD");
    }

    #[test]
    fn test_replace_prefix_no_match() {
        let mut route = Route::from_link(link("CD"));
        route.push(link("BC"));
        route.push(link("AB"));

        let old_prefix = Route::from_link(link("ZZ"));
        let new_prefix = Route::from_link(link("XY"));

        assert!(!route.replace_prefix(&old_prefix, &new_prefix));
        assert_eq!(route.to_string(), "AB.BC.CD");
    }
}
