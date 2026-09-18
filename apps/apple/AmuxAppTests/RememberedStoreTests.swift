import AmuxCore
import AmuxFeatures
import XCTest
@testable import Amux

/// The store-backed states, opened the way the goldens open them: each one
/// writes a real account store through the bridge and draws what a launch
/// reads back from it.
@MainActor
final class RememberedStoreTests: XCTestCase {
    private func open(_ screen: Screen, _ fixture: String) async -> StoreBundle {
        let door = DoorHost.shared
        let reply = await door.handle(.open(screen: screen.rawValue, fixture: fixture))
        XCTAssertEqual(reply, .ack)
        return door.stores
    }

    func testAMachineNotAnsweringLeavesEveryRememberedAgentWaitingForIt() async {
        let stores = await open(.home, "cached-fleet-unreachable")
        let rows = stores.fleet.rows
        XCTAssertEqual(Set(rows.map(\.id)), Set(Scenario.agents.map(\.id)))
        XCTAssertTrue(rows.allSatisfy(\.card.awaiting), "a remembered row was confirmed")
        XCTAssertFalse(stores.fleet.reconciled)
        for card in Scenario.agents {
            let drawn = rows.first { $0.id == card.id }?.card
            // Only the machine that is not answering withholds its agents'
            // standing; every other row keeps what its machine last said.
            let attention: Attention = card.agent.hostId == Scenario.air ? .unknown : card.attention
            XCTAssertEqual(drawn?.attention, attention, card.displayName)
            XCTAssertEqual(drawn?.phase, card.phase, card.displayName)
            XCTAssertEqual(
                drawn?.lastActivity.timeIntervalSince1970 ?? 0,
                card.lastActivity.timeIntervalSince1970, accuracy: 0.001, card.displayName)
        }
    }

    func testAnAgentItsMachineRemovedIsNotRemembered() async {
        let stores = await open(.home, "cached-fleet-removal")
        let removed = Set(Remembered.removal.removed)
        let drawn = Set(stores.fleet.rows.map(\.id))
        XCTAssertFalse(removed.isEmpty)
        XCTAssertTrue(drawn.isDisjoint(with: removed), "a removed agent was drawn")
        XCTAssertEqual(drawn.count, Scenario.agents.count - removed.count)
    }

    func testACachedConversationOpensOnItsStoredWindow() async {
        let stores = await open(.run, "cached-chat")
        let conversation = stores.conversations[Scenario.focus]
        let text = (conversation?.entries ?? []).map { "\($0.row)" }.joined(separator: "\n")
        XCTAssertTrue(text.contains("Why does the reconnect loop back off twice?"), text)
        XCTAssertTrue(text.contains("so a dropped link waits for each in turn"), text)
        XCTAssertTrue(
            stores.fleet.rows.first { $0.id == Scenario.focus }?.card.awaiting ?? false,
            "the conversation's agent was confirmed without a connection")
    }

    func testACachedConversationShowsWhereItsHistoryIsMissing() async {
        let stores = await open(.run, "cached-chat-gap")
        let rows = (stores.conversations[Scenario.focus]?.entries ?? []).transcriptRows()
        let text = rows.map { "\($0.kind)" }
        let marker = rows.firstIndex { $0.kind == .historyBreak(label: "missing history") }
        let before = rows.firstIndex {
            "\($0.kind)".contains("so a dropped link waits for each in turn")
        }
        let after = rows.firstIndex { "\($0.kind)".contains("the reconnect test passes") }
        XCTAssertNotNil(marker, "\(text)")
        XCTAssertLessThan(before ?? .max, marker ?? .min, "\(text)")
        XCTAssertLessThan(marker ?? .max, after ?? .min, "\(text)")
    }
}
