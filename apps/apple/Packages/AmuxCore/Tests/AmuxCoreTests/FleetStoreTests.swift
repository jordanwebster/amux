import Foundation
import XCTest

@testable import AmuxCore

/// Cards as the runtime hands them over, built by hand.
enum Cards {
    static let desk = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000de5c")!)
    static let now = Date(timeIntervalSince1970: 1_800_000_000)

    static func key(_ index: Int, on host: HostId = desk) -> AgentKey {
        AgentKey(host: host, agent: UUID(uuidString: String(
            format: "00000000-0000-0000-0000-%012d", index))!)
    }

    static func row(
        _ index: Int, _ name: String, attention: Attention = .idle, minutesAgo: Double = 1,
        depth: UInt32 = 0, children: UInt32 = 0, family: Attention? = nil,
        members: UInt32 = 1, membersNeedYou: UInt32? = nil, expanded: Bool = false,
        kind: Kind = .claudeSdk, presence: Presence = .online, host: HostId = desk
    ) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: key(index, on: host), name: name, kind: kind, attention: attention,
                cwd: "/Users/pat/source/\(name)",
                phaseSinceMs: Int64(now.addingTimeInterval(-60 * minutesAgo)
                    .timeIntervalSince1970 * 1000),
                host: "desk", hostPresence: presence, children: children,
                familyAttention: family ?? attention, members: members,
                membersNeedYou: membersNeedYou ?? (attention == .needsYou ? 1 : 0),
                branch: nil, exitCause: nil),
            depth: depth, expanded: expanded, secondLine: .blank, loud: nil)
    }

    static func host(
        _ id: HostId = desk, name: String = "desk", presence: Presence = .online,
        via: HostVia = .direct, trusted: Bool = true, candidate: Bool = false,
        local: Bool = false, addrs: [String] = []
    ) -> HostView {
        HostView(
            hostId: id.bytes, name: name, local: local, trusted: trusted, candidate: candidate,
            presence: presence, away: .plain, addrs: addrs, via: via, current: true, providers: [], lastDialError: nil,
            platform: nil, signedIn: nil, version: nil)
    }
}

@MainActor
final class FleetStoreTests: XCTestCase {
    func testRowsKeepTheirPlaceUntilTheListIsShownAgain() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([Cards.row(1, "one"), Cards.row(2, "two")], hosts: [Cards.host()])
        XCTAssertEqual(fleet.rows.map(\.name), ["one", "two"])

        // The runtime ranks two above one now; the screen holds still.
        fleet.show(
            [Cards.row(2, "two", attention: .needsYou), Cards.row(1, "one")],
            hosts: [Cards.host()])
        XCTAssertEqual(fleet.rows.map(\.name), ["one", "two"])
        XCTAssertTrue(fleet.rows[1].needsYou)

        fleet.refreshOrder(now: Cards.now)
        XCTAssertEqual(fleet.rows.map(\.name), ["two", "one"])
    }

    func testANewFamilyLandsWhereTheRankingPutsIt() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([Cards.row(1, "one"), Cards.row(2, "two")], hosts: [])
        fleet.show([Cards.row(1, "one"), Cards.row(3, "three"), Cards.row(2, "two")], hosts: [])
        XCTAssertEqual(fleet.rows.map(\.name), ["one", "three", "two"])
    }

    func testAFamilysMembersFollowTheirHead() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([
            Cards.row(1, "parent", children: 2, family: .needsYou),
            Cards.row(2, "child", attention: .needsYou, depth: 1),
            Cards.row(3, "other"),
        ], hosts: [])
        XCTAssertEqual(fleet.rows.map(\.name), ["parent", "child", "other"])
        XCTAssertEqual(fleet.rows.map(\.depth), [0, 1, 0])
        XCTAssertTrue(fleet.rows[0].familyNeedsYou)
        fleet.toggle(Cards.key(1))
        XCTAssertEqual(fleet.expanded, [Cards.key(1).agent])
        fleet.toggle(Cards.key(1))
        XCTAssertTrue(fleet.expanded.isEmpty)
    }

    func testAQuietFamilyFoldsUnlessItNeedsYouOrWasNotSeen() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([
            Cards.row(1, "fresh", minutesAgo: 5),
            Cards.row(2, "quiet", minutesAgo: 60 * 30),
            Cards.row(3, "asking", attention: .needsYou, minutesAgo: 60 * 30),
        ], hosts: [])
        XCTAssertEqual(fleet.sections.map(\.kind), [.agents, .older])
        XCTAssertEqual(fleet.sections[0].rows.map(\.name), ["fresh", "asking"])
        XCTAssertEqual(fleet.sections[1].rows.map(\.name), ["quiet"])
        XCTAssertTrue(fleet.sections[1].folded)
    }

    func testActivitySinceTheLastOpeningIsUnread() {
        let fleet = FleetStore(now: Cards.now.addingTimeInterval(-3600))
        fleet.show([Cards.row(1, "one", minutesAgo: 1)], hosts: [])
        XCTAssertTrue(fleet.rows[0].unread, "active since the launch")
        fleet.opened(Cards.key(1), at: Cards.now)
        XCTAssertFalse(fleet.rows[0].unread)
    }

    func testTheSubtitleCountsWhoNeedsYou() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([Cards.row(1, "a", attention: .working), Cards.row(2, "b")], hosts: [])
        XCTAssertEqual(fleet.subtitle, "Nothing needs you · 1 running")
        fleet.show([Cards.row(1, "a", attention: .needsYou), Cards.row(2, "b")], hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 2 agents")
    }

    func testAFoldedFamilyCountsTheMembersItHides() {
        let fleet = FleetStore(now: Cards.now)
        // Folded: the lead stands for itself and the helper that needs you.
        let folded = Cards.row(
            1, "lead", children: 1, family: .needsYou, members: 2, membersNeedYou: 1)
        fleet.show([folded, Cards.row(3, "other")], hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 3 agents")

        // Unfolded, the helper is its own row and nothing is counted twice.
        let open = Cards.row(
            1, "lead", children: 1, family: .needsYou, members: 2, membersNeedYou: 1,
            expanded: true)
        fleet.show(
            [open, Cards.row(2, "helper", attention: .needsYou, depth: 1), Cards.row(3, "other")],
            hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 3 agents")
    }

    func testAnOfflineHostIsTheExceptionAndTheRelayOutranksIt() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([Cards.row(1, "a")], hosts: [Cards.host(presence: .offline, via: .unspecified)])
        XCTAssertEqual(fleet.exceptions, "desk offline")
        XCTAssertEqual(fleet.unreachableHost, "desk")
        fleet.relay(.signInAgain)
        XCTAssertEqual(fleet.exceptions, "Sign in again to reach your hosts through the relay")
        fleet.relay(.connecting)
        XCTAssertNil(fleet.relayTrouble, "a link coming up for the first time is not trouble")
        XCTAssertEqual(fleet.exceptions, "desk offline")
        fleet.relay(.retrying)
        XCTAssertEqual(fleet.exceptions, "Reconnecting to the relay")
        fleet.relay(.failed)
        XCTAssertEqual(fleet.exceptions, "The relay cannot be reached")
    }

    func testAKindThisBuildCannotOpenIsListedButNotReadable() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show([Cards.row(1, "a", kind: .unspecified)], hosts: [])
        XCTAssertFalse(fleet.rows[0].readable)
    }
}
