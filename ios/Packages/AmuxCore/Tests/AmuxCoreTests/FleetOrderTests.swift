import Foundation
import XCTest
@testable import AmuxCore

final class FleetOrderTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    func testAgentsThatNeedYouArePinnedLongestWaitingFirst() {
        let cards = [
            Made.card(1, name: "recent-ask", attention: .needsYou(why: .permission), minutesAgo: 5, now: now),
            Made.card(2, name: "busy", attention: .working, minutesAgo: 1, now: now),
            Made.card(3, name: "old-ask", attention: .needsYou(why: .question), minutesAgo: 90, now: now),
            Made.card(4, name: "middle-ask", attention: .needsYou(why: .finished), minutesAgo: 30, now: now),
        ]
        let sections = fleetOrder(cards, now: now, unread: UnreadWeights())
        XCTAssertEqual(sections.first?.kind, .needsYou)
        XCTAssertEqual(sections.first?.rows.map(\.name), ["old-ask", "middle-ask", "recent-ask"])
        XCTAssertEqual(sections.first?.title, "Needs you")
        XCTAssertEqual(sections[1].title, "Everything else")
        XCTAssertEqual(sections[1].rows.map(\.name), ["busy"])
    }

    func testEverythingElseIsOneRecencyList() {
        let cards = [
            Made.card(1, name: "stopped-an-hour-ago", attention: .idle, minutesAgo: 60, now: now),
            Made.card(2, name: "mid-command", attention: .working, minutesAgo: 240, now: now),
            Made.card(3, name: "just-now", attention: .working, minutesAgo: 1, now: now),
        ]
        let sections = fleetOrder(cards, now: now, unread: UnreadWeights())
        XCTAssertEqual(sections.count, 1)
        // Running is not a rank: the agent that stopped an hour ago outranks
        // the one that has been mid-command since this morning.
        XCTAssertEqual(sections[0].rows.map(\.name), ["just-now", "stopped-an-hour-ago", "mid-command"])
        XCTAssertEqual(sections[0].title, "Agents")
    }

    func testAnythingQuietForADayFoldsAway() {
        let cards = [
            Made.card(1, name: "today", attention: .idle, minutesAgo: 30, now: now),
            Made.card(2, name: "yesterday", attention: .idle, minutesAgo: 60 * 25, now: now),
            Made.card(3, name: "last-week", attention: .unknown, minutesAgo: 60 * 24 * 7, now: now),
        ]
        var read = UnreadWeights()
        for card in cards { read.opened(card.id, at: now) }
        let sections = fleetOrder(cards, now: now, unread: read)
        XCTAssertEqual(sections.map(\.kind), [.everythingElse, .older])
        XCTAssertEqual(sections[0].rows.map(\.name), ["today"])
        XCTAssertEqual(sections[1].rows.map(\.name), ["yesterday", "last-week"])
        XCTAssertTrue(sections[1].folded)
    }

    func testAnUnreadAgentNeverFoldsAwayHoweverOldItIs() {
        // The case that makes an ordering argue with itself: a turn that ended
        // two days ago that nobody has read. Folding it hides the only thing
        // on the screen that is actually waiting for a person.
        let cards = [
            Made.card(1, name: "flake-hunt", attention: .idle, minutesAgo: 60 * 48, now: now),
            Made.card(2, name: "reviewed", attention: .idle, minutesAgo: 60 * 48, now: now),
        ]
        var read = UnreadWeights()
        read.opened(Made.agentId(2), at: now)
        let sections = fleetOrder(cards, now: now, unread: read)
        XCTAssertEqual(sections[0].rows.map(\.name), ["flake-hunt"])
        XCTAssertTrue(sections[0].rows[0].unread)
        XCTAssertEqual(sections[1].rows.map(\.name), ["reviewed"])
    }

    func testAgentsNeverSeenCountAsUnread() {
        let card = Made.card(1, name: "new", attention: .idle, minutesAgo: 5, now: now)
        XCTAssertTrue(UnreadWeights().isUnread(card))
        XCTAssertFalse(UnreadWeights(lastOpened: [card.id: now]).isUnread(card))
    }

    func testTiesOrderByIdentitySoTheListNeverShuffles() {
        let cards = (1...3).map {
            Made.card($0, name: "agent-\($0)", attention: .needsYou(why: .permission), minutesAgo: 10, now: now)
        }
        let first = fleetOrder(cards, now: now, unread: UnreadWeights())
        let again = fleetOrder(cards.reversed(), now: now, unread: UnreadWeights())
        XCTAssertEqual(first.map { $0.rows.map(\.id) }, again.map { $0.rows.map(\.id) })
    }
}
