import Foundation
import XCTest
@testable import AmuxCore

@MainActor
final class FleetStoreTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    func testACachedHomeArrivesBeforeItIsConfirmed() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 20, now: now),
        ], reconciled: false))

        XCTAssertEqual(store.rows.map(\.name), ["alpha", "beta"])
        XCTAssertFalse(store.reconciled)
        // Rows the cache remembers are shown, and marked as unconfirmed rather
        // than dressed up as fact.
        XCTAssertEqual(store.rows.map(\.confirmed), [false, false])
    }

    func testSyncConfirmsTheRowsWithoutRegroupingThem() {
        let store = FleetStore(now: now)
        let cached = [
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 20, now: now),
            Made.card(3, name: "gamma", attention: .idle, minutesAgo: 40, now: now),
        ]
        store.apply(Made.fleet(cached, reconciled: false))
        let placedFirst = store.rows.map(\.id)

        // The sync says beta just did something and gamma now needs you. Both
        // would sort somewhere else on a fresh screen; neither may move under
        // the user's thumb.
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now),
            Made.card(2, name: "beta", attention: .working, minutesAgo: 0, now: now),
            Made.card(3, name: "gamma", attention: .needsYou(why: .permission), minutesAgo: 40, now: now),
        ], reconciled: true))

        XCTAssertEqual(store.rows.map(\.id), placedFirst)
        XCTAssertEqual(store.sections.map(\.kind), [.everythingElse])
        XCTAssertTrue(store.reconciled)
        XCTAssertEqual(store.rows.map(\.confirmed), [true, true, true])

        // Regrouping is something the screen asks for, not something a sync
        // does to it.
        store.refreshOrder(now: now)
        XCTAssertEqual(store.sections.map(\.kind), [.needsYou, .everythingElse])
        XCTAssertEqual(store.sections[0].rows.map(\.name), ["gamma"])
    }

    func testAnArrivingAgentLandsWhereTheOrderingPutsIt() {
        let store = FleetStore(now: now)
        let cached = [
            Made.card(1, name: "alpha", attention: .idle, minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 50, now: now),
        ]
        store.apply(Made.fleet(cached, reconciled: false))
        store.apply(Made.fleet(cached + [
            Made.card(3, name: "gamma", attention: .idle, minutesAgo: 20, now: now),
        ], reconciled: true))

        XCTAssertEqual(store.rows.map(\.name), ["alpha", "gamma", "beta"])
    }

    func testADeletedAgentLeavesAndTheRestStayPut() {
        let store = FleetStore(now: now)
        let cards = (1...3).map { Made.card($0, name: "agent-\($0)", minutesAgo: Double($0), now: now) }
        store.apply(Made.fleet(cards, reconciled: true))
        store.apply(Made.fleet([cards[0], cards[2]], reconciled: true))
        XCTAssertEqual(store.rows.map(\.name), ["agent-1", "agent-3"])
    }

    func testTheSubtitleCountsWhatIsWaitingOrSaysNothingIs() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .needsYou(why: .question), minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .working, minutesAgo: 1, now: now),
        ], reconciled: true))
        XCTAssertEqual(store.subtitle, "1 need you · 2 agents")

        let quiet = FleetStore(now: now)
        quiet.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 1, now: now),
        ], reconciled: true))
        XCTAssertEqual(quiet.subtitle, "Nothing needs you · 1 running")
    }

    func testAQuietHomeSaysSomethingOnlyWhenAHostIsMissing() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet(
            [Made.card(1, name: "alpha", minutesAgo: 5, now: now)],
            hosts: [Made.hostEntry(Made.host, name: "studio")],
            reconciled: true))
        XCTAssertNil(store.exceptions)

        store.apply(Made.fleet(
            [Made.card(1, name: "alpha", minutesAgo: 5, now: now)],
            hosts: [Made.hostEntry(Made.host, name: "studio"),
                    Made.hostEntry(Made.other, name: "mini", online: false)],
            reconciled: true))
        XCTAssertEqual(store.exceptions, "mini offline")

        store.apply(.connection(ConnectionUpdate(state: .disconnected, reason: "relay unavailable")))
        XCTAssertEqual(store.exceptions, "Offline · relay unavailable")
    }

    func testOpeningAnAgentClearsItsUnreadWeight() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([Made.card(1, name: "alpha", minutesAgo: 5, now: now)], reconciled: true))
        XCTAssertTrue(store.rows[0].unread)
        store.opened(Made.agentId(1), at: now)
        XCTAssertFalse(store.rows[0].unread)
    }
}
