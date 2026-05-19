# Cloud Architecture

The current cloud architecture is specified in
[NETWORKING.md](NETWORKING.md).

Cloud relay links use authenticated `RoutingService.Connect` streams. Local
non-cloud daemons may also expose the TLS dispatcher on `tcp_port` for
paired Trusted Server runtime traffic and PIN pairing, but there is no
plaintext `amux server connect` bypass.
