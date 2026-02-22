//! Link-based stack routing for amux.
//!
//! Routes are stacks of link names that get popped/pushed at each hop:
//! - Before sending through link X: pop X from dst, push X to src
//! - On receive: match dst.pop() { None → process locally, Some(link) → route to link }

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;

/// Alphabet for generating random link suffixes (lowercase + digits)
const LINK_ALPHABET: [char; 36] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
];

/// A route is a stack of link names.
///
/// The top of the stack (front of deque) is the next hop.
/// Serializes as "AB.BC.CD" where AB is the top (first hop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    links: VecDeque<String>,
}

impl Route {
    /// Create an empty route (no hops).
    /// Used in AnnounceAgent to indicate the agent is local to the sender.
    pub(crate) fn empty() -> Self {
        Self {
            links: VecDeque::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Create a route with a single link.
    ///
    /// # Panics
    /// Debug-asserts that the link name does not contain "." (the route separator).
    pub fn from_link(link: impl Into<String>) -> Self {
        let link = link.into();
        debug_assert!(
            !link.contains('.'),
            "link name must not contain '.': {link}"
        );
        let mut links = VecDeque::new();
        links.push_back(link);
        Self { links }
    }

    /// Push a link onto the front of the route (becomes the new top).
    ///
    /// # Panics
    /// Debug-asserts that the link name does not contain "." (the route separator).
    pub fn push(&mut self, link: impl Into<String>) {
        let link = link.into();
        debug_assert!(
            !link.contains('.'),
            "link name must not contain '.': {link}"
        );
        self.links.push_front(link);
    }

    /// Pop the top link from the route (the next hop).
    pub fn pop(&mut self) -> Option<String> {
        self.links.pop_front()
    }

    /// Peek at the first hop without consuming it.
    pub fn peek(&self) -> Option<&str> {
        self.links.front().map(|s| s.as_str())
    }

    /// Check if this route passes through a given link name.
    pub fn contains_link(&self, link: &str) -> bool {
        self.links.iter().any(|l| l == link)
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

    /// Prepare to send a new message. Pops from dst, creates src from the popped link.
    /// Returns (src, dst) ready to include in the message.
    /// Returns None if dst is empty.
    pub fn send(mut dst: Route) -> Option<(Route, Route)> {
        let next_hop = dst.pop()?;
        let src = Route::from_link(next_hop);
        Some((src, dst))
    }

    /// Prepare a reply. Sends back through the path the message came from (src).
    /// Returns (reply_src, reply_dst) ready to include in the response.
    /// Returns None if src is empty (no return path).
    pub fn reply(src: Route) -> Option<(Route, Route)> {
        Route::send(src)
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self
            .links
            .iter()
            .map(|s| s.as_str())
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
        // Join links with "." separator, top on left
        let s: String = self
            .links
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(".");
        serializer.serialize_str(&s)
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
        let links: VecDeque<String> = s.split('.').map(|s| s.to_string()).collect();
        Ok(Route { links })
    }
}

/// Generate a random 4-character link suffix.
pub fn generate_link_suffix() -> String {
    nanoid::nanoid!(4, &LINK_ALPHABET)
}

/// Sanitize a hostname for use in link names.
/// Replaces periods with hyphens since "." is the route separator.
pub fn sanitize_host_name(host_name: &str) -> String {
    host_name.replace('.', "-")
}

/// Generate a server link name: "{hostname}" or "{hostname}-{rand}".
/// If randomise is true, appends a random suffix for uniqueness.
/// The hostname is sanitized (periods replaced with hyphens).
pub fn generate_server_link(host_name: &str, randomise: bool) -> String {
    let sanitized = sanitize_host_name(host_name);
    if randomise {
        format!("{}-{}", sanitized, generate_link_suffix())
    } else {
        sanitized
    }
}

/// Generate a terminal link name: "term-{rand}".
pub fn generate_terminal_link() -> String {
    format!("term-{}", generate_link_suffix())
}

/// Generate a hook link name: "hook-{rand}".
pub fn generate_hook_link() -> String {
    format!("hook-{}", generate_link_suffix())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_empty() {
        let mut route = Route::empty();
        assert_eq!(route.pop(), None);

        // Push onto empty produces single-link route
        route.push("host-a");
        assert_eq!(route.pop(), Some("host-a".to_string()));
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
        let mut route = Route::from_link("CD");
        route.push("BC");
        route.push("AB");

        assert_eq!(route.pop(), Some("AB".to_string()));
        assert_eq!(route.pop(), Some("BC".to_string()));
        assert_eq!(route.pop(), Some("CD".to_string()));
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_from_link() {
        let mut route = Route::from_link("single");
        assert_eq!(route.pop(), Some("single".to_string()));
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_serialize() {
        let mut route = Route::from_link("CD");
        route.push("BC");
        route.push("AB");

        let serialized = serde_json::to_string(&route).unwrap();
        assert_eq!(serialized, "\"AB.BC.CD\"");
    }

    #[test]
    fn test_route_serialize_single() {
        let route = Route::from_link("AB");
        let serialized = serde_json::to_string(&route).unwrap();
        assert_eq!(serialized, "\"AB\"");
    }

    #[test]
    fn test_route_deserialize() {
        let route: Route = serde_json::from_str("\"AB.BC.CD\"").unwrap();
        let mut route = route;
        assert_eq!(route.pop(), Some("AB".to_string()));
        assert_eq!(route.pop(), Some("BC".to_string()));
        assert_eq!(route.pop(), Some("CD".to_string()));
    }

    #[test]
    fn test_route_deserialize_empty() {
        let mut route: Route = serde_json::from_str("\"\"").unwrap();
        assert_eq!(route.pop(), None);
    }

    #[test]
    fn test_route_roundtrip() {
        let mut original = Route::from_link("CD");
        original.push("BC");
        original.push("AB");

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
        assert!(link.starts_with("myhost-"));
        assert_eq!(link.len(), "myhost-".len() + 4);
    }

    #[test]
    fn test_generate_server_link_deterministic() {
        let link = generate_server_link("myhost", false);
        assert_eq!(link, "myhost");
    }

    #[test]
    fn test_generate_terminal_link() {
        let link = generate_terminal_link();
        assert!(link.starts_with("term-"));
        assert_eq!(link.len(), "term-".len() + 4);
    }

    #[test]
    fn test_generate_hook_link() {
        let link = generate_hook_link();
        assert!(link.starts_with("hook-"));
        assert_eq!(link.len(), "hook-".len() + 4);
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
    fn test_generate_server_link_sanitizes_periods() {
        let link = generate_server_link("my.laptop.local", false);
        assert_eq!(link, "my-laptop-local");
        assert!(!link.contains('.'));

        let link = generate_server_link("my.laptop.local", true);
        assert!(link.starts_with("my-laptop-local-"));
        assert!(!link.contains('.'));
    }

    #[test]
    #[should_panic(expected = "link name must not contain '.'")]
    fn test_from_link_rejects_period() {
        Route::from_link("bad.link");
    }

    #[test]
    #[should_panic(expected = "link name must not contain '.'")]
    fn test_push_rejects_period() {
        let mut route = Route::from_link("good");
        route.push("bad.link");
    }

    #[test]
    fn test_contains_link() {
        let mut route = Route::from_link("CD");
        route.push("BC");
        route.push("AB");

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
        let route = Route::from_link("AB");
        let prefix = Route::from_link("AB");
        assert!(route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_partial_prefix() {
        let mut route = Route::from_link("CD");
        route.push("BC");
        route.push("AB");
        let prefix = Route::from_link("AB");
        assert!(route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_mismatch() {
        let route = Route::from_link("AB");
        let prefix = Route::from_link("XX");
        assert!(!route.starts_with_route(&prefix));
    }

    #[test]
    fn test_starts_with_route_empty_prefix() {
        let route = Route::from_link("AB");
        assert!(route.starts_with_route(&Route::empty()));
    }

    #[test]
    fn test_starts_with_route_longer_prefix() {
        let route = Route::from_link("AB");
        let mut prefix = Route::from_link("CD");
        prefix.push("AB");
        assert!(!route.starts_with_route(&prefix));
    }
}
