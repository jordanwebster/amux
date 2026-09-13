import Network
import XCTest
import dnssd

@testable import AmuxCore

/// What the browser reads off the network, and what it refuses to read.
///
/// The browsing itself needs a network with a machine on it and belongs to the
/// journey that runs one. What is checked here is everything the browser
/// decides on its own: which advertisement is amux's, which resolved address
/// the shared library can dial, and which browser state means a person said no.
final class DiscoveryTests: XCTestCase {
    private let host = HostId(UUID(uuidString: "6D7A4B1E-3C2F-4A58-9B0D-1E2F3A4B5C6D")!)

    func testAnAdvertisementIsReadFromItsOwnRecord() {
        let record = NWTXTRecord(["hid": host.description, "v": "7"])
        let claim = LocalDiscovery.claim(from: record)
        XCTAssertEqual(claim?.host, host)
        XCTAssertEqual(claim?.version, 7)
    }

    func testAnIncompleteOrUnreadableRecordIsPassedOver() {
        // Some other service answering on the same name, or an amux old enough
        // to have advertised itself differently. Neither is guessed at.
        for record in [
            NWTXTRecord(["v": "7"]),
            NWTXTRecord(["hid": host.description]),
            NWTXTRecord(["hid": "kitchen", "v": "7"]),
            NWTXTRecord(["hid": host.description, "v": "soon"]),
        ] {
            XCTAssertNil(LocalDiscovery.claim(from: record))
        }
    }

    func testAResolvedAddressIsSpelledAsTheLibraryParsesOne() {
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: "192.168.1.24", port: 41234)),
            "192.168.1.24:41234")
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: "2001:db8::5", port: 443)),
            "[2001:db8::5]:443")
    }

    func testAnInterfaceScopeIsNotPartOfTheAddress() {
        // The system appends the interface an address was learned on. The
        // library parses an address and a port and has nowhere to put one.
        let scoped = NWEndpoint.Host.ipv4(IPv4Address("192.168.1.24%en0")!)
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: scoped, port: 41234)),
            "192.168.1.24:41234")
    }

    func testAnAddressThatCannotBeDialledIsNotOffered() {
        // A link-local address means nothing without the interface it was
        // learned on, and a name is what was being resolved in the first place.
        XCTAssertNil(LocalDiscovery.address(of: .hostPort(host: "fe80::1%en0", port: 41234)))
        XCTAssertNil(LocalDiscovery.address(of: .hostPort(host: .name("kitchen.local", nil), port: 41234)))
        XCTAssertNil(LocalDiscovery.address(of: .service(
            name: "kitchen", type: LocalDiscovery.service, domain: "local.", interface: nil)))
    }

    func testARefusalIsToldApartFromANetworkThatIsNotReadyYet() {
        let denied = NWError.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))
        XCTAssertEqual(LocalDiscovery.permission(for: .waiting(denied)), .denied)
        XCTAssertEqual(LocalDiscovery.permission(for: .failed(denied)), .denied)
        XCTAssertEqual(LocalDiscovery.permission(for: .ready), .granted)
        // Nothing has been said yet, which is not the same as having been told no.
        XCTAssertEqual(LocalDiscovery.permission(for: .setup), .unknown)
        XCTAssertEqual(
            LocalDiscovery.permission(for: .waiting(.posix(.ENETDOWN))), .unknown)
    }

    @MainActor
    func testStoppingDoesNotWithdrawWhatWasFound() {
        // The library is told separately that the app went away, and closes
        // its direct links itself. Withdrawing here as well would leave a
        // phone coming back with an empty network until the browser has found
        // the same machines a second time.
        var handedOver: [[FoundHost]] = []
        let discovery = LocalDiscovery { handedOver.append($0) }
        discovery.stop()
        XCTAssertEqual(handedOver.count, 0)
    }
}
