import AmuxDesign
import SwiftUI
import XCTest

@testable import AmuxTestSupport

/// The door is a protocol between a Swift app and a Rust driver, so what it
/// puts on the wire is pinned here rather than left to whatever Codable does
/// with an enum this week.
final class DoorTests: XCTestCase {
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    private func wire(_ request: DoorRequest) throws -> [String: Any] {
        let data = try encoder.encode(request)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func testEveryRequestSurvivesTheWire() throws {
        let requests: [DoorRequest] = [
            .open(screen: "home", fixture: nil),
            .open(screen: "home", fixture: "home-quiet"),
            .cloud(.firstRun),
            .connect(relay: "http://127.0.0.1:8080", token: "bearer", user: "ada"),
            .awaitReconciled(seconds: 90),
            .bridge,
            .appearance(.dark),
            .dynamicType("accessibility3"),
            .perturb(token: "accent"),
            .perturb(token: nil),
            .settle,
            .query,
            .capture(path: "/tmp/home.png"),
            .tap(identifier: "home.row.aurora"),
            .type(identifier: "composer.field", text: "hello"),
            .shutdown,
        ]
        for request in requests {
            let round = try decoder.decode(DoorRequest.self, from: encoder.encode(request))
            XCTAssertEqual(round, request)
        }
    }

    func testRequestsAreTaggedAndFlat() throws {
        let opened = try wire(.open(screen: "home", fixture: "home-quiet"))
        XCTAssertEqual(opened["kind"] as? String, "open")
        XCTAssertEqual(opened["screen"] as? String, "home")
        XCTAssertEqual(opened["fixture"] as? String, "home-quiet")

        // A fixture the request does not name is left out rather than sent as
        // null, so the driver's own requests read the same as the app's.
        XCTAssertNil(try wire(.open(screen: "home", fixture: nil))["fixture"])

        XCTAssertEqual(try wire(.appearance(.dark))["appearance"] as? String, "dark")
        XCTAssertEqual(try wire(.dynamicType("accessibility3"))["size"] as? String, "accessibility3")
        XCTAssertEqual(try wire(.capture(path: "/tmp/x.png"))["path"] as? String, "/tmp/x.png")
        XCTAssertEqual(try wire(.settle)["kind"] as? String, "settle")
        XCTAssertEqual(try wire(.awaitReconciled(seconds: 90))["seconds"] as? Double, 90)
        XCTAssertEqual(try wire(.bridge)["kind"] as? String, "bridge")
        XCTAssertEqual(try wire(.perturb(token: "accent"))["token"] as? String, "accent")
        // Nothing named puts the design back, and is sent as an absent field
        // rather than a null, like every other request the door takes.
        XCTAssertNil(try wire(.perturb(token: nil))["token"])
    }

    func testAnUnknownRequestIsRefused() {
        let unknown = Data(#"{"kind":"levitate"}"#.utf8)
        XCTAssertThrowsError(try decoder.decode(DoorRequest.self, from: unknown))
    }

    func testEveryReplySurvivesTheWire() throws {
        let state = VisibleState(
            screen: "home",
            elements: [VisibleElement(
                identifier: "home.title", label: "Agents", value: nil,
                frame: VisibleFrame(x: 16, y: 64, width: 200, height: 32), enabled: true)],
            reconciled: true, shimmering: 3)
        let bridge = BridgeState(
            build: "0.1.0+debug-tools", started: true, connection: "connected",
            reconciled: true, hosts: [], agents: ["helper"], discovered: ["desktop", "laptop"])
        let replies: [DoorReply] = [
            .ack,
            .state(state),
            .bridge(bridge),
            .captured(path: "/tmp/home.png", width: 1206, height: 2622, scale: 3),
            .error("unimplemented: home"),
        ]
        for reply in replies {
            let round = try decoder.decode(DoorReply.self, from: encoder.encode(reply))
            XCTAssertEqual(round, reply)
        }
    }

    func testACaptureNamesItsSizeInPixels() throws {
        let data = try encoder.encode(
            DoorReply.captured(path: "/tmp/home.png", width: 1206, height: 2622, scale: 3))
        let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(wire["kind"] as? String, "captured")
        XCTAssertEqual(wire["width"] as? Int, 1206)
        XCTAssertEqual(wire["height"] as? Int, 2622)
        XCTAssertEqual(wire["scale"] as? Int, 3)
    }

    func testTheReadinessFileNamesThePort() throws {
        let ready = Door.Ready(port: 51201, pid: 4242)
        let round = try decoder.decode(Door.Ready.self, from: encoder.encode(ready))
        XCTAssertEqual(round, ready)
        XCTAssertEqual(Door.readyArgument, "amux-door-ready")
    }

    /// Fixtures and door requests both name a type size in words; a name one
    /// of them uses and the other cannot read would silently render the wrong
    /// size in a capture.
    func testFixtureTypeSizesAreDoorNames() throws {
        for fixture in Fixtures.all {
            guard let named = fixture.typeSize else { continue }
            XCTAssertNotNil(
                DynamicTypeSize(doorName: named),
                "\(fixture.id) asks for a type size the door cannot name: \(named)")
        }
        XCTAssertEqual(DynamicTypeSize(doorName: "large"), .large)
        XCTAssertEqual(DynamicTypeSize(doorName: "accessibility5"), .accessibility5)
        XCTAssertNil(DynamicTypeSize(doorName: "enormous"))
    }
}
