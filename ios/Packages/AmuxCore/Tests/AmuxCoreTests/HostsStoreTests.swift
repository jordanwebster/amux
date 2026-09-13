import Foundation
import XCTest
@testable import AmuxCore

@MainActor
final class HostsStoreTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000001")!)
    private let air = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000002")!)
    private var reading = Date(timeIntervalSince1970: 1_700_000_000)

    private let mini = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000003")!)

    /// A key of the right shape: SHA-256 of a public key, sixty-four hex
    /// characters, because how long one is is most of what a screen has to
    /// cope with.
    private func key(_ seed: String) -> String {
        String(String(repeating: seed, count: 64).prefix(64))
    }

    private var roster: DeviceRoster {
        DeviceRoster(
            identity: DeviceIdentity(
                host: HostId(UUID(uuidString: "40000000-0000-0000-0000-0000000000ff")!),
                name: "iPhone", fingerprint: key("4f2a91c0")),
            devices: [
                PairedDevice(
                    host: air, name: "air", fingerprint: key("9c1d4f77"),
                    pairedAt: reading.addingTimeInterval(-31 * 24 * 3600)),
                PairedDevice(
                    host: mini, name: "mini", fingerprint: key("b73e05c1"),
                    pairedAt: reading.addingTimeInterval(-9 * 24 * 3600)),
                PairedDevice(
                    host: studio, name: "Studio", fingerprint: key("e04a7b12"),
                    pairedAt: reading.addingTimeInterval(-64 * 24 * 3600)),
            ])
    }

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

    /// The trust store is not the fleet. `air` is away and still holds a good
    /// key, which is exactly the machine somebody comes here to revoke, so a
    /// list that dropped it while it was offline would hide the one that
    /// mattered.
    func testTheDevicesAreTheTrustStoreIncludingMachinesThatAreAway() {
        let hosts = store()
        hosts.apply(fleet([host(studio, "Studio", online: true)]))
        XCTAssertTrue(hosts.devices.isEmpty, "a phone that has not read its trust store lists none")

        hosts.apply(.devices(roster))

        XCTAssertEqual(hosts.devices.map(\.name), ["air", "mini", "Studio"])
        XCTAssertEqual(hosts.roster?.identity.name, "iPhone")
        XCTAssertEqual(hosts.roster?.identity.fingerprint.count, 64)
        XCTAssertTrue(
            hosts.devices.allSatisfy { $0.fingerprint.count == 64 },
            "a device arrived without the key it is trusted by")
        XCTAssertNil(hosts.host(air)?.online,
                     "air is trusted but not in the fleet: it is not reachable")
    }

    /// Reading the keys is a state of this screen, not a place to go: the
    /// machines they belong to are the list behind it.
    func testTheKeysOpenAndCloseOverTheMachines() {
        let hosts = store()
        XCTAssertFalse(hosts.readingDevices)
        hosts.readDevices()
        XCTAssertTrue(hosts.readingDevices)
        hosts.stopReadingDevices()
        XCTAssertFalse(hosts.readingDevices)
    }

    /// Revoking reaches the runtime, spelt the way the runtime reads it. It is
    /// the runtime that closes the links, so a command that never left would
    /// be a button that ended nothing.
    func testRevokingReachesTheRuntime() throws {
        var sent: [BridgeCommand] = []
        let stores = StoreBundle(account: AccountId("test"))
        stores.dispatch = { command in
            sent.append(command)
            return OpId(UUID())
        }

        XCTAssertTrue(stores.revoke(studio))

        XCTAssertEqual(sent, [.revoke(host: studio)])
        // The wire spelling, pinned on both sides: a rename here becomes a
        // command the runtime quietly refuses. Read as an object rather than
        // as text, because nothing promises which key an encoder writes first.
        let encoded = try JSONEncoder().encode(BridgeCommand.revoke(host: studio))
        let wire = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: String])
        XCTAssertEqual(wire, ["command": "revoke", "host": "\(studio)"])
        XCTAssertEqual(
            try JSONDecoder().decode(BridgeCommand.self, from: encoded),
            .revoke(host: studio))
    }
}
