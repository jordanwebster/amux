import Foundation
import XCTest
@testable import AmuxCore

@MainActor
final class HostsStoreTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000001")!)
    private let air = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000002")!)
    private var reading = Date(timeIntervalSince1970: 1_700_000_000)

    private func store() -> HostsStore {
        HostsStore(clock: { [self] in reading })
    }

    private func fleet(_ hosts: [HostState]) -> Event {
        .fleet(Fleet(epoch: 1, agents: [], hosts: hosts, reconciled: true))
    }

    private func host(_ id: HostId, _ name: String, online: Bool, platform: String? = "macOS")
        -> HostState
    {
        HostState(
            entry: HostEntry(id: id, name: name, online: online, platform: platform), epoch: 1)
    }

    func testReachableMachinesComeFirstThenTheAlphabet() {
        let hosts = store()
        hosts.apply(fleet([
            host(air, "air", online: false),
            host(studio, "Studio", online: true),
        ]))
        XCTAssertEqual(hosts.hosts.map(\.name), ["Studio", "air"])
        XCTAssertEqual(hosts.online.map(\.name), ["Studio"])
        XCTAssertEqual(hosts.offline.map(\.name), ["air"])
    }

    func testAMachineThatWasNeverSeenOnlineHasNoDepartureTime() {
        let hosts = store()
        hosts.apply(fleet([host(air, "air", online: false)]))
        XCTAssertNil(hosts.wentOffline(air))
    }

    func testTheDepartureIsTheMomentThePhoneWatchedItHappen() {
        let hosts = store()
        hosts.apply(fleet([host(air, "air", online: true)]))
        reading = reading.addingTimeInterval(600)
        hosts.apply(fleet([host(air, "air", online: false)]))
        XCTAssertEqual(hosts.wentOffline(air), reading)

        // A later snapshot that says the same thing does not move it: the
        // machine went once, not on every sync.
        let gone = reading
        reading = reading.addingTimeInterval(600)
        hosts.apply(fleet([host(air, "air", online: false)]))
        XCTAssertEqual(hosts.wentOffline(air), gone)
    }

    func testComingBackClearsTheDeparture() {
        let hosts = store()
        hosts.apply(fleet([host(air, "air", online: true)]))
        hosts.apply(fleet([host(air, "air", online: false)]))
        XCTAssertNotNil(hosts.wentOffline(air))
        hosts.apply(fleet([host(air, "air", online: true)]))
        XCTAssertNil(hosts.wentOffline(air))
    }

    func testAMachineThatLeavesTheInventoryIsForgotten() {
        let hosts = store()
        hosts.apply(fleet([host(air, "air", online: true), host(studio, "Studio", online: true)]))
        hosts.apply(fleet([host(air, "air", online: false), host(studio, "Studio", online: true)]))
        XCTAssertNotNil(hosts.wentOffline(air))
        hosts.apply(fleet([host(studio, "Studio", online: true)]))
        XCTAssertNil(hosts.wentOffline(air))
    }

    func testTheKindIsWhateverTheMachineSaidAndNothingWhereItSaidNothing() {
        let hosts = store()
        hosts.apply(fleet([
            host(studio, "Studio", online: true, platform: "Linux"),
            host(air, "air", online: false, platform: nil),
        ]))
        XCTAssertEqual(hosts.host(studio)?.platform, "Linux")
        XCTAssertNil(hosts.host(air)?.platform)
    }
}
