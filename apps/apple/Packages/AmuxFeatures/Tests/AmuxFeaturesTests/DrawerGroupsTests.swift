import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// The drawer is one list where the home folds its tail. What the home folds
/// away is still an agent you might switch to, so it follows everything else
/// here rather than being a second thing to open.
final class DrawerGroupsTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let host = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)

    private func card(_ number: Int, _ name: String, _ attention: Attention,
                      minutesAgo: Double) -> AgentCard {
        AgentCard(
            agent: Agent(
                id: AgentId(UUID(uuidString: String(format: "00000000-0000-0000-0000-%012d",
                                                    number))!),
                hostId: host, name: name, command: "provider", workingDir: "/work/\(name)",
                kind: .claude(driver: .pty), createdAt: now.addingTimeInterval(-86_400 * 7)),
            displayName: name, attention: attention, phase: .running,
            lastActivity: now.addingTimeInterval(-60 * minutesAgo))
    }

    private func sections(_ cards: [AgentCard]) -> [FleetSection] {
        fleetOrder(cards, now: now, unread: UnreadWeights(
            lastOpened: Dictionary(uniqueKeysWithValues: cards.map { ($0.id, self.now) })))
    }

    func testWhatTheHomeFoldsAwayIsStillListed() {
        let grouped = sections([
            card(1, "waiting", .needsYou(why: .permission), minutesAgo: 3),
            card(2, "running", .working, minutesAgo: 10),
            card(3, "quiet", .idle, minutesAgo: 3 * 24 * 60),
        ])
        XCTAssertEqual(grouped.map(\.kind), [.agents, .older])
        XCTAssertEqual(DrawerRows(grouped).rows.map(\.name), ["waiting", "running", "quiet"])
    }
}
