import XCTest
@testable import Amux

@MainActor
final class DoorStreamsTests: XCTestCase {
    func testObservesOpenedStreamsWithoutLegacyAttachments() throws {
        let snapshot = #"""
        {"streams": {
            "reading": {"phase": {"stream_phase": "live"}},
            "catching-up": {"phase": {"stream_phase": "replaying"}},
            "requested": {"phase": {"stream_phase": "opening"}},
            "lost": {"phase": {"stream_phase": "closed", "reason": "disconnected"}}
        }}
        """#
        XCTAssertEqual(try DoorHost.watching(snapshot: snapshot), ["catching-up", "reading"])
    }

    func testAttachmentIntentAndCachedChatsDoNotProveAnOpenStream() throws {
        let snapshot = #"""
        {"attached": {"requested": "host"},
         "store": {"chats": {"cached": {"state": "Absent"}}},
         "streams": {}}
        """#
        XCTAssertEqual(try DoorHost.watching(snapshot: snapshot), [])
    }

    func testUnreadableStreamStateIsAnErrorRatherThanAnEmptyObservation() {
        let snapshots: [String?] = [nil, "not JSON", "{}",
            #"{"streams":{"agent":{"phase":{"stream_phase":"unknown"}}}}"#]
        for snapshot in snapshots {
            XCTAssertThrowsError(try DoorHost.watching(snapshot: snapshot))
        }
    }
}
