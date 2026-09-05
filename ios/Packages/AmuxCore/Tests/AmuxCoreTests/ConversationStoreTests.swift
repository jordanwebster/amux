import Foundation
import XCTest
@testable import AmuxCore

@MainActor
final class ConversationStoreTests: XCTestCase {
    private let agent = Made.agentId(1)

    private func row(_ id: Int, seq: Int, text: String) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(id),
            "seq": .int(seq),
            "kind": .object(["entry": .string("message"), "text": .string(text)]),
        ]))
    }

    private func text(_ store: ConversationStore) -> [String] {
        store.entries.compactMap { $0.row["kind"]?["text"]?.stringValue }
    }

    func testRowsAppendInOrder() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0, append: [row(0, seq: 1, text: "one")], replace: [], evicted: 0)))
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 1,
            append: [row(1, seq: 2, text: "two"), row(2, seq: 3, text: "three")],
            replace: [], evicted: 0)))
        XCTAssertEqual(text(store), ["one", "two", "three"])
    }

    func testARewrittenRowIsRewrittenRatherThanRepeated() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0,
            append: [row(0, seq: 1, text: "Hello"), row(1, seq: 2, text: "two")],
            replace: [], evicted: 0)))
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 2, append: [],
            replace: [FeedReplacement(position: 0, entry: row(0, seq: 1, text: "Hello\n\nUpdated"))],
            evicted: 0)))
        XCTAssertEqual(text(store), ["Hello\n\nUpdated", "two"])
    }

    func testAnEvictedPrefixLeavesWithoutRenumberingWhatSurvives() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0,
            append: (0..<4).map { row($0, seq: $0 + 1, text: "row-\($0)") },
            replace: [], evicted: 0)))
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 4, append: [row(4, seq: 5, text: "row-4")],
            replace: [], evicted: 2)))
        XCTAssertEqual(text(store), ["row-2", "row-3", "row-4"])
        XCTAssertEqual(store.firstPosition, 2)

        // Positions stay absolute across an eviction: replacing position 3 is
        // still the row that was folded third, not the third one left.
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 5, append: [],
            replace: [FeedReplacement(position: 3, entry: row(3, seq: 4, text: "corrected"))],
            evicted: 2)))
        XCTAssertEqual(text(store), ["row-2", "corrected", "row-4"])
    }

    func testAFeedForAnotherAgentIsIgnored() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: Made.agentId(2), base: 0, append: [row(0, seq: 1, text: "elsewhere")],
            replace: [], evicted: 0)))
        XCTAssertTrue(store.entries.isEmpty)
    }

    func testAGapInTheFeedIsRecordedRatherThanHidden() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0, append: [row(0, seq: 1, text: "one")], replace: [], evicted: 0)))
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 5, append: [row(5, seq: 6, text: "six")], replace: [], evicted: 0)))
        XCTAssertEqual(store.invariants, ["feed gap between 1 and 5"])
    }

    func testTheSessionCarriesTheGateAsksAndProviderFacts() {
        let store = ConversationStore(agent: agent)
        store.apply(.session(SessionSnapshot(
            agent: agent,
            gate: .claudePty(.working),
            phase: .claudePty(.object(["phase": .string("running")])),
            stream: .live,
            asks: [Ask(layer: .claudePty, body: .object(["id": .int(1)]))],
            facts: .claudePty(.object(["layer": .string("claude_pty")])),
            provider: ProviderFacts(model: "opus", effort: "high"),
            settingsGate: .ptySettingsUnavailable,
            queue: nil,
            family: [FamilyMember(agent: Made.agentId(2), depth: 1, needs: .permission)])))

        XCTAssertEqual(store.gate, .claudePty(.working))
        XCTAssertFalse(store.gate.accepts)
        XCTAssertEqual(store.phase.phase, "running")
        XCTAssertEqual(store.asks.count, 1)
        XCTAssertEqual(store.provider.model, "opus")
        XCTAssertEqual(store.family.first?.needs, .permission)
        XCTAssertEqual(store.settingsGate, .ptySettingsUnavailable)
    }

    func testChangesArriveAsADocument() {
        let store = ConversationStore(agent: agent)
        store.apply(.diff(DiffUpdate(agent: agent, document: DiffDocument(
            numbering: .absolute,
            hunks: [DiffHunk(oldStart: 1, newStart: 1, header: "@@ -1 +1 @@", lines: ["-old", "+new"])],
            truncated: false))))
        XCTAssertEqual(store.changes?.hunks.first?.lines, ["-old", "+new"])
    }
}
