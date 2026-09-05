import Foundation
import XCTest
@testable import AmuxCore

final class BridgeClientTests: XCTestCase {
    func testTheLinkedBridgeReportsItsVersion() {
        let version = Bridge.version
        XCTAssertFalse(version.isEmpty)
        XCTAssertEqual(version.split(separator: ".").count, 3, "expected a semantic version, got \(version)")
    }

    func testTheConfigurationIsWrittenTheWayTheBridgeReadsIt() throws {
        let configuration = BridgeConfiguration(
            dataDirectory: URL(fileURLWithPath: "/tmp/data"),
            cacheDirectory: URL(fileURLWithPath: "/tmp/cache"),
            deviceName: "iPhone",
            relay: .init(url: "https://relay.example", tls: .system, token: .fixed("bearer")),
            logPath: URL(fileURLWithPath: "/tmp/amux.log"))
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: AmuxJSON.encoder.encode(configuration)) as? [String: Any])
        XCTAssertEqual(json["data_dir"] as? String, "/tmp/data")
        XCTAssertEqual(json["device_name"] as? String, "iPhone")
        XCTAssertEqual(json["frame_interval_ns"] as? UInt64, 16_666_667)
        let relay = try XCTUnwrap(json["relay"] as? [String: Any])
        XCTAssertEqual(relay["tls"] as? String, "System")
        XCTAssertEqual((relay["token"] as? [String: Any])?["Static"] as? String, "bearer")

        let callback = BridgeConfiguration.Token.callback
        XCTAssertEqual(String(decoding: try AmuxJSON.encoder.encode(callback), as: UTF8.self), "\"Callback\"")
    }

    func testSubscriptionCommandsAreSpelledTheWayTheBridgeExpects() throws {
        let agent = Made.agentId(1)
        let json = String(decoding: try AmuxJSON.encoder.encode(BridgeCommand.subscribe(agent: agent)), as: UTF8.self)
        XCTAssertTrue(json.contains("\"command\":\"subscribe\""), json)
        XCTAssertTrue(json.contains(agent.description), json)
        XCTAssertEqual(
            try AmuxJSON.decoder.decode(BridgeCommand.self, from: Data(json.utf8)),
            .subscribe(agent: agent))
    }

    /// The bridge is the only thing that starts the runtime, so a start that
    /// never reports anything is the failure worth catching here. A relay that
    /// is not there still owes an answer.
    func testStartingAgainstAnUnreachableRelayReportsTheLinkAndStopsCleanly() async throws {
        let root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("amux-bridge-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let client = try BridgeClient(configuration: BridgeConfiguration(
            dataDirectory: root.appendingPathComponent("data"),
            cacheDirectory: root.appendingPathComponent("cache"),
            deviceName: "unit-test",
            relay: .init(url: "https://127.0.0.1:1", tls: .system, token: .fixed("unused")),
            logPath: root.appendingPathComponent("amux.log")))
        defer { client.stop() }

        var seen: [Event] = []
        for await batch in client.events {
            seen.append(contentsOf: batch)
            if seen.contains(where: { if case .connection = $0 { return true } else { return false } }) {
                break
            }
        }
        XCTAssertTrue(seen.contains { if case .connection = $0 { return true } else { return false } },
                      "the bridge reported no link state: \(seen)")
        XCTAssertTrue(client.malformedBatches.isEmpty, "\(client.malformedBatches)")
    }

    /// Batches describe one consistent moment each. They are applied whole and
    /// in the order they arrived, on the main actor, or a fleet and a
    /// conversation end up describing different moments.
    @MainActor
    func testBatchesAreAppliedInOrderOnTheMainActor() async {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let registry = AccountRegistry()
        let account = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")
        registry.add(account)

        var send: AsyncStream<[Event]>.Continuation!
        let stream = AsyncStream<[Event]> { send = $0 }
        let agent = Made.agentId(1)

        send.yield([
            Made.fleet([Made.card(1, name: "alpha", minutesAgo: 5, now: now)], reconciled: false),
            .feed(FeedUpdate(agent: agent, base: 0, append: [
                FeedEntry(layer: .claudePty, row: .object([
                    "id": .int(0), "seq": .int(1),
                    "kind": .object(["entry": .string("message"), "text": .string("first")]),
                ])),
            ], replace: [], evicted: 0)),
        ])
        send.yield([
            Made.fleet([
                Made.card(1, name: "alpha", minutesAgo: 5, now: now),
                Made.card(2, name: "beta", minutesAgo: 1, now: now),
            ], reconciled: true),
            .feed(FeedUpdate(agent: agent, base: 1, append: [
                FeedEntry(layer: .claudePty, row: .object([
                    "id": .int(1), "seq": .int(2),
                    "kind": .object(["entry": .string("message"), "text": .string("second")]),
                ])),
            ], replace: [], evicted: 0)),
        ])
        send.finish()

        var mainThread = true
        for await batch in stream {
            mainThread = mainThread && Thread.isMainThread
            registry.deliver(batch, for: account.id)
        }

        XCTAssertTrue(mainThread)
        let stores = registry.stores
        XCTAssertEqual(stores?.applied, 2)
        XCTAssertEqual(stores?.fleet.rows.map(\.name), ["beta", "alpha"])
        XCTAssertTrue(stores?.fleet.reconciled == true)
        XCTAssertEqual(
            stores?.conversation(agent).entries.compactMap { $0.row["kind"]?["text"]?.stringValue },
            ["first", "second"])
    }
}
