import Foundation
@testable import AmuxCore

/// Small builders so a test can say what it is about — who is waiting, and for
/// how long — without restating the whole protocol each time.
enum Made {
    static let host = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)
    static let other = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000BB")!)

    static func agentId(_ number: Int) -> AgentId {
        AgentId(UUID(uuidString: String(format: "00000000-0000-0000-0000-%012d", number))!)
    }

    static func card(
        _ number: Int,
        name: String,
        attention: Attention = .idle,
        minutesAgo: Double,
        now: Date,
        host: HostId = Made.host,
        awaiting: Bool = false
    ) -> AgentCard {
        AgentCard(
            agent: Agent(
                id: agentId(number),
                hostId: host,
                name: name,
                command: "provider",
                workingDir: "/work/\(name)",
                kind: .claude(driver: .pty),
                createdAt: now.addingTimeInterval(-86_400 * 7)),
            displayName: name,
            attention: attention,
            phase: .running,
            lastActivity: now.addingTimeInterval(-60 * minutesAgo),
            awaiting: awaiting)
    }

    static func hostEntry(_ id: HostId, name: String, online: Bool = true) -> HostState {
        HostState(entry: HostEntry(id: id, name: name, online: online), epoch: 1)
    }

    static func fleet(_ cards: [AgentCard], hosts: [HostState] = [hostEntry(host, name: "studio")],
                      reconciled: Bool) -> Event {
        .fleet(Fleet(epoch: 1, agents: cards, hosts: hosts, reconciled: reconciled))
    }
}
