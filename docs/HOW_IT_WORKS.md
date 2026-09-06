# How amux works

amux lets your devices reach the AI coding agents and terminal sessions
running on your other devices — your phone checking on a build your
workstation is running, your laptop picking up a session you started at
your desk. This page explains the model behind that, and why you can
trust it with a door into your machines.

## Devices and accounts

An amux installation can hold several **profiles**, such as Personal and
Work. Each profile is a complete device: its own private key, trusted peers,
agents and cloud connection. Its key stays on your machine; no server holds
it. A cloud account provides relay access, while **pairing** decides which
devices may use each other's agents. Joining the same account never grants
that trust by itself.

Use `amux profiles` to see your profiles and `--profile <name|UUID>` to choose
one for a command. Each cloud account has at most one profile per installation;
you can also keep profiles without any cloud account. All eligible profiles
stay connected while the installation runs, even when you are viewing another
one. A failed login or cloud connection in Work leaves Personal running.
Logging out keeps the profile's identity, trust and agents; logging back into
the same account needs no re-pairing. Pause keeps the credential too, and stays
paused across restarts. Deleting a profile explicitly destroys its keys, trust
and agents. Profiles isolate amux state and routing, but do not sandbox code
running as the same OS user.

Run `amux init` to create an installation with an unbound profile, then
`amux login` to add a cloud account. Login shows the account's name and email;
use its UUID from `amux profiles` in
`amux --profile <UUID> profile rename Work` to give it a local label. Another
account gets a separate profile; logging into it never changes Work's binding.

In `amux ui --profile Work`, return to the fleet and press the leader key
(Ctrl+A by default), then `p`. The switcher lists profile labels, emails and
status. Use arrows or `j`/`k`, then Enter to switch; Esc closes it. The new fleet
shows only the selected profile's agents, and amux remembers its UUID for later
commands. `--profile` overrides the remembered selection.

`amux server stop` stops every profile and kills their running agents.
`amux update` instead saves active sessions across all profiles before replacing
the binary and restores those sessions afterward. Sessions already suspended
stay suspended. Ordinary startup does not restore suspended sessions. Profiles
share one desktop daemon, so a process crash affects every account.

## Pairing: trust is something you do once, in person

Pairing connects two devices you control (or yours and a collaborator's).
It works like you'd hope:

- **Type a PIN** — one device shows a 6-digit code, you type it into the
  other; or
- **Scan a QR code** — point your phone's system camera at the screen.

Under the hood both run the same password-authenticated key exchange
(SPAKE2): the code proves to each device that the other one is the
machine physically in front of you, *without the code itself ever
crossing the network*. An eavesdropper learns nothing they can replay;
codes are one-shot and expire in about five minutes.

For QR pairing, `amux pair --qr` renders a production `amux://pair?...`
deep link in the terminal QR. Development builds can also print that link
with `amux pair --qr --link` for simulator testing.

For unattended demos — an app reviewer who will never see your screen —
`amux pair --demo --pin 123456 --for 30d` holds an operator-chosen PIN open
for a fixed period. It is the same SPAKE2 exchange, but the PIN is reusable:
success does not consume it and mistyped attempts do not lock it out. The
command returns immediately; the daemon keeps the session until it expires,
`amux pair --cancel`, or a daemon restart. Treat the PIN as a shared secret
for that window and only use it on a throwaway machine.

What pairing produces is small and local: each device **pins the other's
public key** in its own trust store, like remembering a face. From then
on, all trust decisions are made against that pinned key — on your
device, by your device.

Pairing belongs to the selected profile. If two machines use both Personal
and Work, pair them once for each account. A key trusted by Personal grants
no access to Work. Paired peers can operate agents, including creating and
deleting them, but cannot administer your trust store or stop, suspend or
resume your installation.

## Talking: links and tunnels

Paired devices connect however they can reach each other — a direct
connection on your network, SSH, or through a relay when they can't
reach each other directly (your phone on cellular, your workstation
behind a home router). amux calls these connections **links**.

Every actual conversation — attaching to a session, listing agents,
streaming output — travels inside a **tunnel**: an end-to-end encrypted
channel that rides whatever links are available. Before a single byte of
your session flows, the two endpoints complete a mutual TLS handshake
checked against their pinned keys. Both sides prove who they are, every
time, no matter what carried the connection.

That one rule — every call is a tunnel, every tunnel is authenticated
end-to-end — is the heart of the security model. The transport
underneath is just plumbing; nothing about it is trusted.

## What a relay can see and do

When two of your devices can't reach each other directly, a relay passes
their messages along. The relay can be the amux cloud, or any always-on
device you've paired (a home server works fine) — relaying is built into
every node.

The cloud relay sees encrypted tunnel traffic plus metadata such as device
names, account identity, online status, delivery addresses and traffic timing.
It cannot:

- **read your sessions** — tunnel contents are encrypted end-to-end
  between your devices;
- **impersonate a device** — it holds no pinned key, so it fails the
  handshake that guards every call;
- **create agents or run commands** — those require a tunnel that
  terminates inside your trusted circle, which the cloud relay cannot form.

The cloud relay has a subscription check at the door. A compromised relay
could drop or delay packets and disrupt connectivity, but it would gain no
authority to read sessions or operate agents. A paired device that also relays
traffic has the agent authority you granted when pairing; it still cannot read
the tunnels it forwards between other devices.

## Leaving: revocation is local and immediate

Unpair a device in a profile and that profile deletes its pinned key. From
that moment every connection and in-flight session from that device is
cut, and new attempts fail their handshake. You don't ask a server's
permission to stop trusting someone; you just stop.
Other profiles keep their own trust decisions.

## Why this is trustworthy

- **The protocol is small.** A handful of message types, two routing
  rules, one pairing flow, one way to authenticate a call. Small enough
  to read in an afternoon, small enough to audit.
- **It's missing things on purpose.** No transitive trust ("a friend of
  a friend"), no multi-hop routing, no central trust authority, no
  bearer tokens for pairing. Each absence is a whole class of attacks
  and bugs that cannot exist.
- **The spec is executable.** The protocol's guarantees are locked in by
  a test suite written as plain-English specifications — from "a relay
  that carries every byte still cannot call the devices it serves" to
  "revoking trust breaks in-flight sessions immediately" — and run
  against real daemons, real TLS, and real sockets on every change.

For the wire-level details, see the [protocol specification](PROTOCOL.md).
For how the pieces fit together inside, see the
[architecture](ARCHITECTURE.md).
