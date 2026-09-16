import Foundation
import XCTest
@testable import AmuxCore

/// One event this build cannot read costs that event, never the batch it
/// arrived in, and the conversation it was about says something is missing.
@MainActor
final class BridgeBatchTests: XCTestCase {
    private let agent = "00000000-0000-0000-0000-000000000001"

    private var batch: Data {
        Data("""
        [
          {"Connection": {"state": "connecting", "reason": null}},
          {"Session": {"agent": "\(agent)", "gate": {"layer": "a_layer_from_the_future"}}},
          {"Hologram": {"agent": "\(agent)"}},
          {"Feed": {"agent": "\(agent)", "base": 0, "append": [{"layer": "claude_pty",
            "row": {"id": 0, "seq": 1, "kind": {"entry": "message", "text": "Hello"}}}],
            "replace": [], "evicted": 0}}
        ]
        """.utf8)
    }

    func testAnEventThatWillNotDecodeStandsInItsPlace() throws {
        let decoded = try Event.batch(from: batch, decoder: AmuxJSON.decoder)

        XCTAssertEqual(decoded.events.count, 4)
        guard case .connection = decoded.events[0] else {
            return XCTFail("the event before the unreadable ones was lost: \(decoded.events)")
        }
        guard case .unreadable(let session) = decoded.events[1],
              case .unreadable(let unknown) = decoded.events[2]
        else { return XCTFail("expected two unreadable events: \(decoded.events)") }
        XCTAssertEqual(session.kind, "Session")
        XCTAssertEqual(session.agent, AgentId(agent))
        XCTAssertEqual(unknown.kind, "Hologram")
        guard case .feed(let feed) = decoded.events[3] else {
            return XCTFail("the event after the unreadable ones was lost: \(decoded.events)")
        }
        XCTAssertEqual(feed.append.count, 1)
        // Their bytes are kept for a report.
        XCTAssertEqual(decoded.unreadable.count, 2)
    }

    func testABatchThatDecodesWholeReportsNothingUnreadable() throws {
        let data = Data(#"[{"Connection": {"state": "connecting", "reason": null}}]"#.utf8)
        let decoded = try Event.batch(from: data, decoder: AmuxJSON.decoder)
        XCTAssertEqual(decoded.events.count, 1)
        XCTAssertTrue(decoded.unreadable.isEmpty)
    }

    func testTheConversationShowsTheRestAndAdmitsTheGap() throws {
        let stores = StoreBundle(account: AccountId("test"))
        let decoded = try Event.batch(from: batch, decoder: AmuxJSON.decoder)

        stores.apply(decoded.events)

        let conversation = stores.conversation(try XCTUnwrap(AgentId(agent)))
        XCTAssertEqual(conversation.confirmedRows().count, 1, "the feed in the same batch applied")
        XCTAssertEqual(conversation.unreadable.map(\.kind), ["Session", "Hologram"])
        XCTAssertEqual(stores.fleet.connection.state, .connecting)
    }
}
