import AmuxCore
import Foundation

/// The design's scenario, in the core's own vocabulary.
///
/// Every fixture is a state of this one morning: the same ten agents on the
/// same four machines, the same conversation open. Keeping one scenario means
/// a screenshot can never disagree with another screenshot about what an agent
/// is called or what it is asking for.
public enum Scenario {
    /// A fixed morning. Fixtures state ages relative to it, so a capture taken
    /// a year from now still shows "2m" rather than drifting.
    public static let now = Date(timeIntervalSince1970: 1_764_580_800)

    /// Stable identity from a name, so the same agent keeps the same
    /// identifier across runs, machines and captures.
    public static func agentId(_ slug: String) -> AgentId {
        AgentId(uuid(slug, prefix: 0xA6))
    }

    public static func hostId(_ slug: String) -> HostId {
        HostId(uuid(slug, prefix: 0x40))
    }

    private static func uuid(_ slug: String, prefix: UInt8) -> UUID {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in slug.utf8 {
            hash = (hash ^ UInt64(byte)) &* 0x0000_0100_0000_01B3
        }
        var bytes = [UInt8](repeating: 0, count: 16)
        bytes[0] = prefix
        for index in 0..<8 { bytes[8 + index] = UInt8((hash >> (8 * UInt64(index))) & 0xFF) }
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                           bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
                           bytes[14], bytes[15]))
    }

    // MARK: - Machines

    public static let studio = hostId("studio")
    public static let mini = hostId("mini")
    public static let air = hostId("air")
    public static let homelab = hostId("homelab")

    /// The paired machines. `air` is one you cannot reach this morning, which
    /// is why one agent's state is genuinely unknown rather than idle.
    public static let hosts: [HostState] = [
        HostState(entry: HostEntry(
            id: studio, name: "Studio", online: true, version: "0.4.0",
            platform: "macOS"), epoch: 1),
        HostState(entry: HostEntry(
            id: mini, name: "mini", online: true, version: "0.4.0",
            platform: "macOS"), epoch: 1),
        HostState(entry: HostEntry(
            id: air, name: "air", online: false, version: "0.4.0",
            lastDialError: "no route to host", platform: "macOS"), epoch: 1),
    ]

    /// The same machines on a morning when all three answer. What a phone
    /// hears before one of them goes away, so a fixture can put a machine's
    /// departure in the past by playing both.
    public static let reachableHosts: [HostState] = hosts.map { host in
        var reachable = host
        reachable.entry.online = true
        reachable.entry.lastDialError = nil
        return reachable
    }

    /// What the pinned clock reads.
    ///
    /// A capture is taken at one fixed moment so two runs draw the same
    /// picture. A fixture whose state is about something having happened
    /// earlier winds this back, applies that, and puts it forward again —
    /// which is the only way this phone ever learns that a machine went away
    /// eight minutes ago rather than just now.
    @MainActor public static var reading = now

    /// A machine on the network that has not been paired yet. It is never in
    /// the fleet — an untrusted host is an offer, not a host.
    public static let unpaired = HostEntry(
        id: homelab, name: "homelab", online: true, trustStatus: .untrustedButOnline,
        platform: "Linux")

    /// That machine, having answered an invitation: it has proved it issued
    /// the offer and said who it is, and nothing has been trusted. The
    /// fingerprint is a real one's shape — SHA-256 of a key, sixty-four hex
    /// characters — because how long it is is most of what the screen has to
    /// cope with.
    @MainActor public static var offered: PendingPeer {
        PendingPeer(
            pending: "0f9d5a2c-4e11-4a76-9f2b-6b1c0d3e8a54",
            host: homelab, name: "homelab",
            fingerprint: "4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0",
            expiresAt: reading.addingTimeInterval(4 * 60 + 30))
    }

    // MARK: - Agents

    public static func card(
        _ slug: String,
        host: HostId,
        directory: String,
        kind: AgentKind,
        attention: Attention,
        minutesAgo: Double,
        phase: AgentPhase = .running,
        headline: String? = nil,
        outcome: TurnOutcome? = nil
    ) -> AgentCard {
        let activity = now.addingTimeInterval(-60 * minutesAgo)
        return AgentCard(
            agent: Agent(
                id: agentId(slug),
                hostId: host,
                name: slug,
                command: kind == .codex ? "codex" : "claude",
                workingDir: directory,
                kind: kind,
                createdAt: now.addingTimeInterval(-86_400 * 3),
                workingOn: headline.map { WorkingOn(text: $0, updatedAt: activity) }),
            displayName: slug,
            attention: attention,
            phase: phase,
            lastActivity: activity,
            outcome: outcome)
    }

    private static let claude = AgentKind.claude(driver: .pty)
    private static let codex = AgentKind.codex

    /// The mix is deliberate: two agents are actually blocked, four have ended
    /// a turn and are waiting to be read, and the rest are running or stale.
    /// Agents waiting on a person are rarer than they used to be.
    public static let agents: [AgentCard] = [
        card("refactor-auth", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .permission), minutesAgo: 2,
             headline: "Wants to run a command"),
        card("spec-suite", host: mini, directory: "~/src/amux", kind: codex,
             attention: .working, minutesAgo: 14.0 / 60,
             headline: "Running the spec suite · 47 of 126"),
        card("docs-pass", host: studio, directory: "~/src/amux-docs", kind: claude,
             attention: .needsYou(why: .question), minutesAgo: 6,
             headline: "Which crate should own the redaction table?"),
        card("ios-bridge", host: studio, directory: "~/src/amux-core-bridge", kind: codex,
             attention: .working, minutesAgo: 3,
             headline: "Editing bridge/src/session.rs"),
        card("relay-cleanup", host: mini, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 14,
             headline: "Collapsed the pairing errors onto one string",
             outcome: TurnOutcome(files: 4, insertions: 118, deletions: 40,
                                  note: "3 tests added")),
        card("pairing-copy", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 300,
             headline: "Rewrote the pairing failure copy",
             outcome: TurnOutcome(files: 2, insertions: 36, deletions: 9)),
        // The case that makes an ordering argue with itself: it ended a turn
        // two days ago and nobody has read it.
        card("flake-hunt", host: mini, directory: "~/src/amux", kind: codex,
             attention: .needsYou(why: .finished), minutesAgo: 2880,
             headline: "Tracked the flake down to a shared temp dir",
             outcome: TurnOutcome(files: 1, insertions: 21, deletions: 6)),
        card("changelog", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 540,
             headline: "Drafted the 0.4 changelog",
             outcome: TurnOutcome(files: 1, insertions: 64, deletions: 2)),
        // Its host is unreachable, so the phone shows the last thing it heard
        // and says the state is unknown rather than guessing at it.
        card("legacy-port", host: air, directory: "~/src/legacy", kind: claude,
             attention: .unknown, minutesAgo: 1500,
             headline: "Porting the legacy transport"),
        card("old-migration", host: mini, directory: "~/src/amux", kind: codex,
             attention: .idle, minutesAgo: 7200,
             headline: "Waiting on you since Monday"),
    ]

    /// An agent run by a provider this build has no case for.
    ///
    /// One machine on an account can run a newer amux than the phone, and what
    /// comes back then is a real agent under a provider name this build has
    /// never heard of. It is listed under that name, said to be unreadable and
    /// never offered to open: throwing it away would take the fleet with it.
    public static let unreadableAgent = card(
        "transport-shim", host: studio, directory: "~/src/amux", kind: .unknown("gemini"),
        attention: .needsYou(why: .finished), minutesAgo: 9,
        headline: "Ported the transport shim")

    /// The same morning as `agents`, as a launch that has reached nothing yet
    /// sees it: every card is what this phone remembered, and no machine has
    /// answered for any of them.
    public static let remembered: [AgentCard] = agents.map { card in
        var remembered = card
        remembered.awaiting = true
        return remembered
    }

    /// The same morning with the agent this app opens on mid-turn.
    ///
    /// The conversation fixtures put a running command at the end of the feed;
    /// the fleet has to agree, because the composer names what the transcript
    /// is doing and counts from when the fleet says it started. A card that
    /// still said "wants to run a command" would put a screen into a state the
    /// core never produces.
    public static let working: [AgentCard] = [
        card("refactor-auth", host: studio, directory: "~/src/amux", kind: claude,
             attention: .working, minutesAgo: 8.0 / 60,
             headline: "Running the spec suite"),
    ] + agents.dropFirst()

    /// The same morning with the three agents `refactor-auth` started on the
    /// fleet as well.
    ///
    /// They are here because the fleet owns an agent's name and the
    /// conversation does not: a child the fleet has never heard of is drawn
    /// with the identity it was started under, which is true but unreadable.
    /// One of them is waiting on somebody, which is the fact the strip above
    /// the composer colours for.
    public static let startedWork: [AgentCard] = working + [
        card("spec-fixer", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .permission), minutesAgo: 1,
             headline: "Rewriting the three assertions"),
        card("docs-sweep", host: studio, directory: "~/src/amux", kind: claude,
             attention: .working, minutesAgo: 2,
             headline: "Reading docs/PAIRING.md"),
        card("changelog-line", host: studio, directory: "~/src/amux", kind: claude,
             attention: .working, minutesAgo: 3,
             headline: "Drafting the changelog line"),
    ]

    /// The same morning, later: everything that was blocked has been answered
    /// and every finished turn has been read.
    ///
    /// Answered agents are gone rather than converted — an agent whose
    /// permission you granted goes on working under its own name, and there
    /// is no state in which ten agents are all mid-command at once. What is
    /// left is two running, one that has gone quiet, and two nobody has
    /// touched in over a day.
    public static let settledAgents: [AgentCard] = [
        card("spec-suite", host: mini, directory: "~/src/amux", kind: codex,
             attention: .working, minutesAgo: 14.0 / 60,
             headline: "Running the spec suite · 47 of 126"),
        card("ios-bridge", host: studio, directory: "~/src/amux-core-bridge", kind: codex,
             attention: .working, minutesAgo: 3,
             headline: "Editing bridge/src/session.rs"),
        card("changelog", host: studio, directory: "~/src/amux", kind: claude,
             attention: .idle, minutesAgo: 540,
             headline: "Drafted the 0.4 changelog",
             outcome: TurnOutcome(files: 1, insertions: 64, deletions: 2)),
        card("legacy-port", host: air, directory: "~/src/legacy", kind: claude,
             attention: .unknown, minutesAgo: 1500,
             headline: "Porting the legacy transport"),
        card("old-migration", host: mini, directory: "~/src/amux", kind: codex,
             attention: .idle, minutesAgo: 7200,
             headline: "Waiting on you since Monday"),
    ]

    /// The agent whose conversation every talking screen opens.
    public static let focus = agentId("refactor-auth")

    /// A photo somebody attached. The identity is the shape a stored blob's
    /// identity really has — a sha256 digest — because the element that names
    /// it is rejected by the shared parser otherwise, and a fixture that could
    /// not be parsed would photograph prose pretending to be a token.
    public static let screenshot = DraftAttachment(
        id: ArtifactId("sha256:" + String(repeating: "a1b2c3d4", count: 8)),
        kind: .image, name: "reconnect-loop.png", mime: "image/png", size: 184_320)

    /// A file somebody attached.
    public static let trace = DraftAttachment(
        id: ArtifactId("sha256:" + String(repeating: "9f8e7d6c", count: 8)),
        kind: .file, name: "relay-trace.json", mime: "application/json", size: 20_480)

    /// A file the *agent* attached, through its own attach tool. It is the
    /// same shape as one a person attaches, because in the message text there
    /// is no difference between them — which is the thing worth photographing.
    public static let checkOutput = DraftAttachment(
        id: ArtifactId("sha256:" + String(repeating: "5e4d3c2b", count: 8)),
        kind: .file, name: "cargo-check.txt", mime: "text/plain", size: 3_072)

    /// A paste past the length at which the shared library makes it a token.
    public static let longPaste = (1...12)
        .map { "  relay: attempt \($0) refused, retrying in \($0 * 250)ms" }
        .joined(separator: "\n")

    /// Everything the phone has read, so the settled fleet carries no unread
    /// weight of its own.
    public static var allRead: UnreadWeights {
        UnreadWeights(lastOpened: Dictionary(
            uniqueKeysWithValues: agents.map { ($0.id, now.addingTimeInterval(60)) }))
    }

    /// Read except the four turns nobody has opened yet.
    public static var mostlyRead: UnreadWeights {
        let unseen: Set<String> = ["refactor-auth", "docs-pass", "relay-cleanup", "pairing-copy",
                                   "flake-hunt"]
        return UnreadWeights(lastOpened: Dictionary(uniqueKeysWithValues: agents
            .filter { !unseen.contains($0.displayName) }
            .map { ($0.id, now.addingTimeInterval(60)) }))
    }
}
