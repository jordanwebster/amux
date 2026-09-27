import Foundation
import XCTest

@testable import AmuxCore

/// A session the bundle opened: it catches up the moment it opens, as the
/// runtime's does once the agent's host answers, and says when it closed.
private final class Session: OpenChat, @unchecked Sendable {
    let id: UInt64
    let agent: AgentKey
    var closed = false

    init(id: UInt64, agent: AgentKey) {
        self.id = id
        self.agent = agent
    }

    func close() { closed = true }
    func keys() -> [String] { ["a"] }
    func keys(above newest: String) -> [String]? { [] }
    func keys(below oldest: String) -> [String]? { [] }
    func rows(for keys: [String], options: RowOptions?) -> [Row] { [] }
    func askCard() -> AskCard? { nil }
    func strip() -> Strip? { nil }
    func settings() -> SettingsView? { nil }
    func frame() -> ChatFrame? {
        ChatFrame(
            agent: agent, name: "a", kind: .claudeSdk, phase: .needsYou,
            composer: ComposerView(mode: .send, activity: nil), connection: .live, caughtUp: true,
            hasOlder: false, queue: [], outbox: [], askInput: nil, ended: nil, waiting: nil)
    }
    func takeChanges() -> ChatChanges { ChatChanges(keys: [], reloaded: false, session: false) }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("no")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, picks: [Pick], note: String?) async -> ActOutcome? { nil }
    func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome? { nil }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { nil }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func interrupt() async -> ActOutcome? { nil }
    func change(_ setting: SettingChange) async -> ActOutcome? { nil }
    func resume(with draft: Draft) async -> ActOutcome? { nil }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { nil }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("no"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func review() async -> Result<FrozenReview, RuntimeFailure> { .failure(RuntimeFailure("no")) }
}

@MainActor
final class StoreBundleSessionTests: XCTestCase {
    private var clock = Cards.now
    private var opened: [Session] = []

    private func bundle() -> StoreBundle {
        StoreBundle(
            account: AccountId("a"), clock: { [unowned self] in clock },
            sessionRetention: .seconds(300),
            opener: { [unowned self] agent in
                let session = Session(id: UInt64(opened.count + 1), agent: agent)
                opened.append(session)
                return session
            })
    }

    private func sessions(for agent: AgentKey) -> [Session] { opened.filter { $0.agent == agent } }

    func testANeedsYouAgentsChatIsAlreadyOpenAndCurrentWhenItsPageOpens() throws {
        let stores = bundle()
        let asking = Cards.key(1)
        stores.fleet.show(
            [Cards.row(1, "asking", attention: .needsYou), Cards.row(2, "idle"), Cards.row(3, "working", attention: .working)],
            hosts: [Cards.host()])
        stores.keepSessions()
        XCTAssertEqual(opened.map(\.agent), [asking], "only the agent that needs the person is warmed")

        let model = try stores.chat(asking)
        XCTAssertEqual(opened.count, 1, "the page takes the session already open")
        XCTAssertEqual(model.frame?.caughtUp, true, "and it is current before the page drew it")
    }

    func testAViewedChatStaysForTheRetentionAndThenCloses() throws {
        let stores = bundle()
        let viewed = Cards.key(2)
        stores.fleet.show([Cards.row(2, "idle")], hosts: [Cards.host()])
        _ = try stores.chat(viewed)
        stores.leave(viewed)

        clock = Cards.now.addingTimeInterval(299)
        stores.keepSessions()
        XCTAssertTrue(stores.holds(viewed), "coming back within the retention is instant")
        XCTAssertFalse(sessions(for: viewed)[0].closed)
        _ = try stores.chat(viewed)
        XCTAssertEqual(sessions(for: viewed).count, 1, "no second session is opened")
        stores.leave(viewed)

        clock = Cards.now.addingTimeInterval(299 + 301)
        stores.keepSessions()
        XCTAssertFalse(stores.holds(viewed))
        XCTAssertTrue(sessions(for: viewed)[0].closed)
    }

    func testANeedsYouSessionNobodyViewedClosesOnceTheAgentStopsAsking() {
        let stores = bundle()
        let asking = Cards.key(1)
        stores.fleet.show([Cards.row(1, "asking", attention: .needsYou)], hosts: [Cards.host()])
        stores.keepSessions()
        XCTAssertTrue(stores.holds(asking))

        stores.fleet.show([Cards.row(1, "asking", attention: .working)], hosts: [Cards.host()])
        stores.keepSessions()
        XCTAssertFalse(stores.holds(asking))
        XCTAssertTrue(sessions(for: asking)[0].closed)
    }

    func testAShownChatIsNeverClosedUnderIt() throws {
        let stores = bundle()
        let shown = Cards.key(2)
        stores.fleet.show([Cards.row(2, "idle")], hosts: [Cards.host()])
        _ = try stores.chat(shown)
        clock = Cards.now.addingTimeInterval(3600)
        stores.keepSessions()
        XCTAssertTrue(stores.holds(shown))
    }
}
