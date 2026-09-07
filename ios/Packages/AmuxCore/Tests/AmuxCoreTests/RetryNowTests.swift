import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class RetryNowTests: XCTestCase {
    private final class Sent {
        var commands: [BridgeCommand] = []
    }

    private func bundle() -> (StoreBundle, Sent) {
        let sent = Sent()
        let stores = StoreBundle(account: AccountId("test"))
        stores.dispatch = { command in
            sent.commands.append(command)
            return OpId(UUID())
        }
        return (stores, sent)
    }

    /// The offer on an unreachable machine's conversation reaches the runtime.
    /// It used to reach nothing, which made a control that could be pressed
    /// forever without anything happening.
    func testRetryNowReachesTheRuntime() {
        let (stores, sent) = bundle()

        XCTAssertTrue(stores.retryNow())

        XCTAssertEqual(sent.commands, [.retryNow])
    }

    /// Nothing to ask means nothing pretended. A phone with no account and no
    /// runtime says the press did not leave rather than reporting success.
    func testRetryNowWithNoRuntimeBehindItSaysSo() {
        let stores = StoreBundle(account: AccountId("signed-out"))

        XCTAssertFalse(stores.retryNow())
    }

    /// The wire spelling, pinned on both sides of the boundary: a rename here
    /// would otherwise become a command the runtime quietly refuses.
    func testTheCommandIsSpeltTheWayTheRuntimeReadsIt() throws {
        let encoded = try JSONEncoder().encode(BridgeCommand.retryNow)
        XCTAssertEqual(String(decoding: encoded, as: UTF8.self), #"{"command":"retry_now"}"#)
        XCTAssertEqual(try JSONDecoder().decode(BridgeCommand.self, from: encoded), .retryNow)
    }

    /// What comes back says the request was made, not that a relay answered.
    /// Recovery is a connection state, and it arrives on its own.
    func testTheAnswerIsThatTheRequestWasMade() throws {
        let outcome = try AmuxJSON.decoder.decode(
            OpOutcome.self, from: Data(#"{"outcome":"retry_requested"}"#.utf8))

        XCTAssertEqual(outcome, .retryRequested)
    }
}
