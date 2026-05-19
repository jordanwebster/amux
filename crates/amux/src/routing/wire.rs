use uuid::Uuid;

use crate::HostId;
use crate::protocol::wire::pb;
use crate::routing::{
    Host, Link, ROUTE_HOP_CAP, Route, RoutingEvent, host_to_wire, validate_remote_host,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InboundRoutingEvent {
    HostUp { host: Host, route: Route },
    HostDown { host_id: HostId, route: Route },
    SnapshotComplete,
    RouteOverHopCap,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WireRoutingEventError {
    #[error("routing event is missing event body")]
    MissingEvent,
    #[error("{0} is missing")]
    MissingField(&'static str),
    #[error("{field} is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

pub(crate) fn outbound_routing_event_to_wire(event: &RoutingEvent) -> pb::RoutingEvent {
    let event = match event {
        RoutingEvent::HostUp { host, route, .. } => pb::routing_event::Event::HostUp(pb::HostUp {
            host: Some(host_to_wire(host)),
            route: Some(route_to_wire(route)),
        }),
        RoutingEvent::HostDown { host_id, route, .. } => {
            pb::routing_event::Event::HostDown(pb::HostDown {
                host_id: host_id.as_bytes().to_vec(),
                route: Some(route_to_wire(route)),
                reason: None,
            })
        }
    };
    pb::RoutingEvent { event: Some(event) }
}

pub(crate) fn outbound_routing_message(event: &RoutingEvent) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::RoutingEvent(
            outbound_routing_event_to_wire(event),
        )),
    }
}

#[cfg(test)]
pub(crate) fn routing_snapshot_to_wire(
    events: &[RoutingEvent],
    outbound_link: &Link,
    peer_host_id: Option<HostId>,
) -> Vec<pb::RoutingEvent> {
    events
        .iter()
        .filter(|event| should_send_routing_event_to_link(event, outbound_link, peer_host_id))
        .map(outbound_routing_event_to_wire)
        .chain(std::iter::once(snapshot_complete_event()))
        .collect()
}

pub(crate) fn should_send_routing_event_to_link(
    event: &RoutingEvent,
    outbound_link: &Link,
    peer_host_id: Option<HostId>,
) -> bool {
    match event {
        RoutingEvent::HostUp {
            host, origin_link, ..
        } => origin_link.as_ref() != Some(outbound_link) && Some(host.id) != peer_host_id,
        RoutingEvent::HostDown { origin_link, .. } => origin_link.as_ref() != Some(outbound_link),
    }
}

pub(crate) fn wire_routing_event_to_inbound(
    event: pb::RoutingEvent,
    incoming_link: &Link,
) -> Result<InboundRoutingEvent, WireRoutingEventError> {
    let event = event.event.ok_or(WireRoutingEventError::MissingEvent)?;
    match event {
        pb::routing_event::Event::HostUp(event) => {
            let Some(route) = required_route_from_wire("HostUp.route", event.route, incoming_link)?
            else {
                return Ok(InboundRoutingEvent::RouteOverHopCap);
            };
            let host = event
                .host
                .ok_or(WireRoutingEventError::MissingField("HostUp.host"))
                .and_then(inbound_host_from_wire)?;
            Ok(InboundRoutingEvent::HostUp { host, route })
        }
        pb::routing_event::Event::HostDown(event) => {
            let Some(route) =
                required_route_from_wire("HostDown.route", event.route, incoming_link)?
            else {
                return Ok(InboundRoutingEvent::RouteOverHopCap);
            };
            Ok(InboundRoutingEvent::HostDown {
                host_id: uuid_from_bytes("HostDown.host_id", &event.host_id)?,
                route,
            })
        }
        pb::routing_event::Event::SnapshotComplete(_) => Ok(InboundRoutingEvent::SnapshotComplete),
    }
}

#[cfg(test)]
fn snapshot_complete_event() -> pb::RoutingEvent {
    pb::RoutingEvent {
        event: Some(pb::routing_event::Event::SnapshotComplete(
            pb::SnapshotComplete {},
        )),
    }
}

fn inbound_host_from_wire(host: pb::Host) -> Result<Host, WireRoutingEventError> {
    let host = crate::routing::host_from_wire(host).map_err(|error| {
        WireRoutingEventError::InvalidField {
            field: "Host",
            reason: error.to_string(),
        }
    })?;
    validate_remote_host(&host).map_err(|reason| WireRoutingEventError::InvalidField {
        field: "Host",
        reason,
    })?;
    Ok(host)
}

fn required_route_from_wire(
    field: &'static str,
    route: Option<pb::Route>,
    incoming_link: &Link,
) -> Result<Option<Route>, WireRoutingEventError> {
    let route = route.ok_or(WireRoutingEventError::MissingField(field))?;
    if route.links.len().saturating_add(1) > ROUTE_HOP_CAP {
        return Ok(None);
    }
    let mut route = route_from_wire(field, route)?;
    route.push(incoming_link.clone());
    Ok(Some(route))
}

fn route_from_wire(field: &'static str, route: pb::Route) -> Result<Route, WireRoutingEventError> {
    Route::from_links(route.links).map_err(|error| WireRoutingEventError::InvalidField {
        field,
        reason: error.to_string(),
    })
}

pub(crate) fn route_to_wire(route: &Route) -> pb::Route {
    pb::Route {
        links: route.iter().map(|link| link.as_str().to_string()).collect(),
    }
}

fn uuid_from_bytes(field: &'static str, bytes: &[u8]) -> Result<Uuid, WireRoutingEventError> {
    Uuid::from_slice(bytes).map_err(|error| WireRoutingEventError::InvalidField {
        field,
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{Capabilities, SupportedAgentType};

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    fn route(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: vec![SupportedAgentType {
                    agent_type: "test-agent".to_string(),
                }],
            },
        }
    }

    fn wire_host_up(host: Host, links: &[&str]) -> pb::RoutingEvent {
        pb::RoutingEvent {
            event: Some(pb::routing_event::Event::HostUp(pb::HostUp {
                host: Some(host_to_wire(&host)),
                route: Some(pb::Route {
                    links: links.iter().map(|link| (*link).to_string()).collect(),
                }),
            })),
        }
    }

    fn wire_host_down(host_id: HostId, links: &[&str]) -> pb::RoutingEvent {
        pb::RoutingEvent {
            event: Some(pb::routing_event::Event::HostDown(pb::HostDown {
                host_id: host_id.as_bytes().to_vec(),
                route: Some(pb::Route {
                    links: links.iter().map(|link| (*link).to_string()).collect(),
                }),
                reason: None,
            })),
        }
    }

    #[test]
    fn outbound_routing_event_strips_origin_link_metadata() {
        let event = RoutingEvent::HostUp {
            host: host(1),
            route: route(&["relay", "target"]),
            origin_link: Some(link("inbound")),
        };

        let wire = outbound_routing_event_to_wire(&event);
        let Some(pb::routing_event::Event::HostUp(up)) = wire.event else {
            panic!("expected HostUp");
        };
        assert_eq!(up.route.unwrap().links, vec!["relay", "target"]);
        assert_eq!(up.host.unwrap().host_id, HostId::from_u128(1).as_bytes());
    }

    #[test]
    fn outbound_message_wraps_routing_event_body() {
        let event = RoutingEvent::HostDown {
            host_id: HostId::from_u128(2),
            route: route(&["peer"]),
            origin_link: None,
        };

        assert!(matches!(
            outbound_routing_message(&event).body,
            Some(pb::message::Body::RoutingEvent(pb::RoutingEvent {
                event: Some(pb::routing_event::Event::HostDown(_))
            }))
        ));
    }

    #[test]
    fn outbound_filter_prevents_origin_echo_and_peer_self_announcement() {
        let outbound = link("peer");
        let peer_host_id = HostId::from_u128(10);

        assert!(!should_send_routing_event_to_link(
            &RoutingEvent::HostUp {
                host: host(10),
                route: route(&["peer"]),
                origin_link: None,
            },
            &outbound,
            Some(peer_host_id),
        ));
        assert!(!should_send_routing_event_to_link(
            &RoutingEvent::HostUp {
                host: host(20),
                route: route(&["peer", "remote"]),
                origin_link: Some(outbound.clone()),
            },
            &outbound,
            Some(peer_host_id),
        ));
        assert!(!should_send_routing_event_to_link(
            &RoutingEvent::HostDown {
                host_id: HostId::from_u128(20),
                route: route(&["peer", "remote"]),
                origin_link: Some(outbound.clone()),
            },
            &outbound,
            Some(peer_host_id),
        ));
        assert!(should_send_routing_event_to_link(
            &RoutingEvent::HostUp {
                host: host(20),
                route: route(&["other", "remote"]),
                origin_link: Some(link("other")),
            },
            &outbound,
            Some(peer_host_id),
        ));
    }

    #[test]
    fn snapshot_conversion_filters_then_appends_snapshot_complete() {
        let outbound = link("peer");
        let events = vec![
            RoutingEvent::HostUp {
                host: host(1),
                route: route(&["peer"]),
                origin_link: Some(outbound.clone()),
            },
            RoutingEvent::HostUp {
                host: host(2),
                route: route(&["other"]),
                origin_link: Some(link("other")),
            },
        ];

        let wire = routing_snapshot_to_wire(&events, &outbound, None);
        assert_eq!(wire.len(), 2);
        assert!(matches!(
            wire[0].event,
            Some(pb::routing_event::Event::HostUp(_))
        ));
        assert!(matches!(
            wire[1].event,
            Some(pb::routing_event::Event::SnapshotComplete(_))
        ));
    }

    #[test]
    fn inbound_events_prepend_incoming_link_before_storage() {
        let incoming = link("peer");

        assert_eq!(
            wire_routing_event_to_inbound(wire_host_up(host(1), &["remote"]), &incoming).unwrap(),
            InboundRoutingEvent::HostUp {
                host: host(1),
                route: route(&["peer", "remote"]),
            }
        );
        assert_eq!(
            wire_routing_event_to_inbound(wire_host_down(HostId::from_u128(1), &[]), &incoming)
                .unwrap(),
            InboundRoutingEvent::HostDown {
                host_id: HostId::from_u128(1),
                route: route(&["peer"]),
            }
        );
    }

    #[test]
    fn inbound_snapshot_complete_is_marker_only() {
        let event = pb::RoutingEvent {
            event: Some(pb::routing_event::Event::SnapshotComplete(
                pb::SnapshotComplete {},
            )),
        };
        assert_eq!(
            wire_routing_event_to_inbound(event, &link("peer")).unwrap(),
            InboundRoutingEvent::SnapshotComplete
        );
    }

    #[test]
    fn inbound_route_over_hop_cap_is_dropped_marker() {
        let incoming = link("incoming");
        let over_cap = (0..ROUTE_HOP_CAP)
            .map(|idx| format!("hop-{idx}"))
            .collect::<Vec<_>>();
        let over_cap_refs = over_cap.iter().map(String::as_str).collect::<Vec<_>>();

        assert_eq!(
            wire_routing_event_to_inbound(wire_host_up(host(1), &over_cap_refs), &incoming)
                .unwrap(),
            InboundRoutingEvent::RouteOverHopCap
        );
        assert_eq!(
            wire_routing_event_to_inbound(
                wire_host_down(HostId::from_u128(1), &over_cap_refs),
                &incoming,
            )
            .unwrap(),
            InboundRoutingEvent::RouteOverHopCap
        );

        let mut invalid_over_cap = over_cap.clone();
        invalid_over_cap[0] = "bad.link".to_string();
        let invalid_over_cap_refs = invalid_over_cap
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            wire_routing_event_to_inbound(wire_host_up(host(1), &invalid_over_cap_refs), &incoming)
                .unwrap(),
            InboundRoutingEvent::RouteOverHopCap
        );

        let mut invalid_host = host(1);
        invalid_host.name.clear();
        assert_eq!(
            wire_routing_event_to_inbound(wire_host_up(invalid_host, &over_cap_refs), &incoming)
                .unwrap(),
            InboundRoutingEvent::RouteOverHopCap
        );
    }

    #[test]
    fn inbound_validation_rejects_missing_fields_and_bad_routes() {
        assert!(matches!(
            wire_routing_event_to_inbound(pb::RoutingEvent { event: None }, &link("peer")),
            Err(WireRoutingEventError::MissingEvent)
        ));
        assert!(matches!(
            wire_routing_event_to_inbound(
                pb::RoutingEvent {
                    event: Some(pb::routing_event::Event::HostUp(pb::HostUp {
                        host: Some(host_to_wire(&host(1))),
                        route: None,
                    })),
                },
                &link("peer"),
            ),
            Err(WireRoutingEventError::MissingField("HostUp.route"))
        ));
        assert!(matches!(
            wire_routing_event_to_inbound(
                pb::RoutingEvent {
                    event: Some(pb::routing_event::Event::HostDown(pb::HostDown {
                        host_id: vec![1, 2, 3],
                        route: Some(pb::Route { links: Vec::new() }),
                        reason: None,
                    })),
                },
                &link("peer"),
            ),
            Err(WireRoutingEventError::InvalidField {
                field: "HostDown.host_id",
                ..
            })
        ));
        assert!(matches!(
            wire_routing_event_to_inbound(wire_host_up(host(1), &["bad.link"]), &link("peer")),
            Err(WireRoutingEventError::InvalidField {
                field: "HostUp.route",
                ..
            })
        ));
    }
}
