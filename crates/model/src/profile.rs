use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identity for one account-scoped profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(pub Uuid);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
