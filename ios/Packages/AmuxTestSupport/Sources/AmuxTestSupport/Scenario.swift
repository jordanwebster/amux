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
        HostState(entry: HostEntry(id: studio, name: "Studio", online: true, version: "0.4.0"), epoch: 1),
        HostState(entry: HostEntry(id: mini, name: "mini", online: true, version: "0.4.0"), epoch: 1),
        HostState(entry: HostEntry(
            id: air, name: "air", online: false, version: "0.4.0",
            lastDialError: "no route to host"), epoch: 1),
    ]

    /// A machine on the network that has not been paired yet. It is never in
    /// the fleet — an untrusted host is an offer, not a host.
    public static let unpaired = HostEntry(
        id: homelab, name: "homelab", online: true, trustStatus: .untrustedButOnline)

    // MARK: - Agents

    public static func card(
        _ slug: String,
        host: HostId,
        directory: String,
        kind: AgentKind,
        attention: Attention,
        minutesAgo: Double,
        phase: AgentPhase = .running
    ) -> AgentCard {
        AgentCard(
            agent: Agent(
                id: agentId(slug),
                hostId: host,
                name: slug,
                command: kind == .codex ? "codex" : "claude",
                workingDir: directory,
                kind: kind,
                createdAt: now.addingTimeInterval(-86_400 * 3)),
            displayName: slug,
            attention: attention,
            phase: phase,
            lastActivity: now.addingTimeInterval(-60 * minutesAgo))
    }

    private static let claude = AgentKind.claude(driver: .pty)
    private static let codex = AgentKind.codex

    /// The mix is deliberate: two agents are actually blocked, four have ended
    /// a turn and are waiting to be read, and the rest are running or stale.
    /// Agents waiting on a person are rarer than they used to be.
    public static let agents: [AgentCard] = [
        card("refactor-auth", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .permission), minutesAgo: 2),
        card("spec-suite", host: mini, directory: "~/src/amux", kind: codex,
             attention: .working, minutesAgo: 0),
        card("docs-pass", host: studio, directory: "~/src/amux-docs", kind: claude,
             attention: .needsYou(why: .question), minutesAgo: 6),
        card("ios-bridge", host: studio, directory: "~/src/amux-core-bridge", kind: codex,
             attention: .working, minutesAgo: 3),
        card("relay-cleanup", host: mini, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 14),
        card("pairing-copy", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 300),
        // The case that makes an ordering argue with itself: it ended a turn
        // two days ago and nobody has read it.
        card("flake-hunt", host: mini, directory: "~/src/amux", kind: codex,
             attention: .needsYou(why: .finished), minutesAgo: 2880),
        card("changelog", host: studio, directory: "~/src/amux", kind: claude,
             attention: .needsYou(why: .finished), minutesAgo: 540),
        card("legacy-port", host: air, directory: "~/src/legacy", kind: claude,
             attention: .unknown, minutesAgo: 1500),
        card("old-migration", host: mini, directory: "~/src/amux", kind: codex,
             attention: .idle, minutesAgo: 7200),
    ]

    /// The same fleet on a morning with nothing blocked and nothing unread.
    public static let settledAgents: [AgentCard] = agents.map { card in
        var settled = card
        if case .needsYou = card.attention { settled.attention = .working }
        return settled
    }

    /// The agent whose conversation every talking screen opens.
    public static let focus = agentId("refactor-auth")

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
