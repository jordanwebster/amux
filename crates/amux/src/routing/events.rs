use tokio::sync::mpsc;
use uuid::Uuid;

use crate::HostId;
use crate::routing::types::{Capabilities, Host};
use crate::routing::{Link, Route};

const DEFAULT_EVENT_BUFFER: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoutingEvent {
    HostUp {
        host: Host,
        route: Route,
        origin_link: Option<Link>,
    },
    HostDown {
        host_id: HostId,
        route: Route,
        origin_link: Option<Link>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostReachabilityEvent {
    Added { host: Host },
    Removed { host_id: HostId },
    StatusChanged { host_id: HostId },
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostTrustStatus {
    Trusted,
    UntrustedButOnline,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostReachabilityStatus {
    Reachable,
    Unreachable { last_error: String },
    Unknown,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub id: Uuid,
    pub name: String,
    pub online: bool,
    pub version: Option<String>,
    pub capabilities: Option<Capabilities>,
    pub trust_status: HostTrustStatus,
    pub reachability_status: Option<HostReachabilityStatus>,
}

/// Client-facing host inventory events.
///
/// These are carried by `ClientService.SubscribeHosts`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    SnapshotComplete,
    HostUpdated { host: HostEntry },
    HostRemoved { id: Uuid },
}

impl HostEvent {
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::SnapshotComplete => "Host::SnapshotComplete",
            Self::HostUpdated { .. } => "Host::HostUpdated",
            Self::HostRemoved { .. } => "Host::HostRemoved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EmitStats {
    pub(crate) delivered: usize,
    pub(crate) dropped: usize,
}

pub(crate) struct EventSource<E> {
    capacity: usize,
    subscribers: Vec<EventSubscriber<E>>,
}

struct EventSubscriber<E> {
    tx: mpsc::Sender<E>,
    overflow: OverflowPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverflowPolicy {
    Critical,
    DropSubscriber,
}

impl<E> Default for EventSource<E> {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUFFER)
    }
}

impl<E> EventSource<E> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            subscribers: Vec::new(),
        }
    }

    pub(crate) fn subscribe(&mut self) -> mpsc::Receiver<E> {
        self.subscribe_with_policy(OverflowPolicy::Critical)
    }

    pub(crate) fn subscribe_drop_on_overflow(&mut self) -> mpsc::Receiver<E> {
        self.subscribe_with_policy(OverflowPolicy::DropSubscriber)
    }

    fn subscribe_with_policy(&mut self, overflow: OverflowPolicy) -> mpsc::Receiver<E> {
        let (tx, rx) = mpsc::channel(self.capacity);
        self.subscribers.push(EventSubscriber { tx, overflow });
        rx
    }
}

impl<E: Clone> EventSource<E> {
    pub(crate) fn emit(&mut self, event: E) -> EmitStats {
        let mut delivered = 0;
        let before = self.subscribers.len();

        self.subscribers
            .retain(|subscriber| match subscriber.tx.try_send(event.clone()) {
                Ok(()) => {
                    delivered += 1;
                    true
                }
                Err(mpsc::error::TrySendError::Full(_))
                    if subscriber.overflow == OverflowPolicy::Critical =>
                {
                    panic!("critical event subscriber queue full")
                }
                Err(mpsc::error::TrySendError::Full(_))
                | Err(mpsc::error::TrySendError::Closed(_)) => false,
            });

        EmitStats {
            delivered,
            dropped: before.saturating_sub(self.subscribers.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drop_on_overflow_subscriber_is_dropped_without_blocking_later_subscribers() {
        let mut source = EventSource::new(1);
        let mut full = source.subscribe_drop_on_overflow();
        let mut live = source.subscribe_drop_on_overflow();

        let first = HostReachabilityEvent::Removed {
            host_id: HostId::nil(),
        };
        assert_eq!(
            source.emit(first.clone()),
            EmitStats {
                delivered: 2,
                dropped: 0
            }
        );
        assert_eq!(live.recv().await, Some(first));

        let second = HostReachabilityEvent::Removed {
            host_id: HostId::from_u128(1),
        };
        assert_eq!(
            source.emit(second.clone()),
            EmitStats {
                delivered: 1,
                dropped: 1
            }
        );
        assert_eq!(live.recv().await, Some(second));
        assert!(full.try_recv().is_ok());
    }

    #[test]
    #[should_panic(expected = "critical event subscriber queue full")]
    fn full_critical_subscriber_panics() {
        let mut source = EventSource::new(1);
        let _full = source.subscribe();
        source.emit(HostReachabilityEvent::Removed {
            host_id: HostId::nil(),
        });
        source.emit(HostReachabilityEvent::Removed {
            host_id: HostId::from_u128(1),
        });
    }
}
