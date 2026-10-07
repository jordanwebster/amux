import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class HostsStoreTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000501")!)
    private let mini = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000502")!)
    private let found = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000503")!)
    private let phone = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000504")!)

    func testHostsAreSplitIntoThisPhoneTrustedAndFound() {
        let hosts = HostsStore()
        hosts.show([
            Cards.host(phone, name: "phone", local: true),
            Cards.host(studio, name: "studio"),
            Cards.host(found, name: "found", trusted: false, candidate: true, addrs: ["10.0.0.9:7"]),
        ])
        XCTAssertEqual(hosts.local?.name, "phone")
        XCTAssertEqual(hosts.hosts.map(\.name), ["studio"])
        XCTAssertEqual(hosts.discovered.map(\.name), ["found"])
        XCTAssertEqual(hosts.candidates(.onThisNetwork).map(\.name), ["found"])
        XCTAssertEqual(hosts.known(found)?.addrs, ["10.0.0.9:7"])
    }

    func testReachGroupsFollowTheRouteAndPresence() {
        XCTAssertEqual(Cards.host(via: .direct).reach, .onThisNetwork)
        XCTAssertEqual(Cards.host(via: .relay).reach, .throughTheRelay)
        XCTAssertEqual(Cards.host(via: .ssh).reach, .throughTheRelay)
        XCTAssertEqual(Cards.host(presence: .away, via: .unspecified).reach, .away)
        XCTAssertEqual(Cards.host(presence: .offline, via: .unspecified).reach, .offline)
        XCTAssertTrue(HostReach.onThisNetwork.live)
        XCTAssertFalse(HostReach.away.live)
    }

    func testOnlineHostsListFirstThenByName() {
        let hosts = HostsStore()
        hosts.show([
            Cards.host(studio, name: "Zed", presence: .offline, via: .unspecified),
            Cards.host(mini, name: "alpha", presence: .offline, via: .unspecified),
            Cards.host(found, name: "mid"),
        ])
        XCTAssertEqual(hosts.hosts.map(\.name), ["mid", "alpha", "Zed"])
    }

    func testOnlyADepartureThisPhoneWatchedHasATime() {
        var clock = Date(timeIntervalSince1970: 1_000)
        let hosts = HostsStore(clock: { clock })
        hosts.show([Cards.host(studio, presence: .offline, via: .unspecified)])
        XCTAssertNil(hosts.wentOffline(studio), "already gone when first seen")
        hosts.show([Cards.host(studio)])
        clock = Date(timeIntervalSince1970: 2_000)
        hosts.show([Cards.host(studio, presence: .offline, via: .unspecified)])
        XCTAssertEqual(hosts.wentOffline(studio), clock)
        hosts.show([Cards.host(studio)])
        XCTAssertNil(hosts.wentOffline(studio))
    }

    func testTheRosterAndThePermissionAreHeld() {
        let hosts = HostsStore()
        let roster = Roster(
            identity: Identity(hostId: phone.bytes, name: "phone", fingerprint: "ab"),
            peers: [PairedPeer(hostId: studio.bytes, name: "studio", fingerprint: "cd")])
        hosts.show(roster)
        hosts.sawLocalNetwork(.denied)
        XCTAssertEqual(hosts.devices.map(\.name), ["studio"])
        XCTAssertEqual(hosts.localNetwork, .denied)
    }
}
