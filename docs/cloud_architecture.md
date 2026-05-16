# Cloud Architecture

The current cloud architecture is specified in
[NEW_ARCHITECTURE.md](NEW_ARCHITECTURE.md).

Cloud relay links use authenticated `RoutingService.Connect` streams. Local
non-cloud daemons may also expose a plain direct `RoutingService.Connect`
listener when `tcp_port` is configured, but that operator-directed
`amux server connect` path is not the cloud relay path and does not replace
cloud authentication.
