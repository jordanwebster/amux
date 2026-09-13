import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What the strip above the composer says out loud.
///
/// The marks on it are shapes — a clock, a pencil, a branch and a number — and
/// a shape is unreadable to anybody using VoiceOver. These pin the sentences
/// that stand in for them, which is also what a journey driving the app reads
/// back.
@MainActor
final class FactsStripTests: XCTestCase {
    private func held(_ delivery: QueueDelivery) -> QueuedMessage {
        QueuedMessage(
            draft: HeldDraft(segments: [.text("squash it")]),
            heldAt: Date(timeIntervalSince1970: 1_700_000_000), delivery: delivery)
    }

    func testAWaitingMessageSaysItIsWaiting() {
        XCTAssertEqual(held(.held).spoken, "waiting for the turn to end")
    }

    /// A message the turn released is said to be on its way, which is also why
    /// the row stops offering to take it back.
    func testAMessageThatWentSaysSo() {
        let queued = held(.sending(op: OpId(UUID())))
        XCTAssertEqual(queued.spoken, "on its way")
        XCTAssertFalse(queued.changeable)
    }

    /// A refusal is the core's own sentence. It is kept rather than dropped:
    /// what was written is not lost to a failure, and the row says what
    /// happened to it.
    func testARefusedMessageKeepsTheHostsWords() throws {
        let json = Data("""
            {"error":"general","message":"the session is replaying history",\
            "auth_required":false,"subscription_required":false}
            """.utf8)
        let failure = try AmuxJSON.decoder.decode(OpFailure.self, from: json)
        let queued = held(.failed(failure))
        XCTAssertEqual(queued.spoken, "the session is replaying history")
        // Still changeable: a message that was refused is one somebody will
        // want to rewrite, which is exactly the gesture the row offers.
        XCTAssertTrue(queued.changeable)
    }
}
