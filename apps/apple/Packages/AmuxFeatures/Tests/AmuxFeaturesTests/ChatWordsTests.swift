import AmuxCore
import XCTest

@testable import AmuxFeatures

final class ChatWordsTests: XCTestCase {
    private func row(_ kind: RowKind, attention: Bool = false, decision: Decision? = nil) -> Row {
        Row(
            id: "k", order: 1, atMs: 0, kind: kind, collapsed: false, attention: attention,
            decision: decision, parent: nil, run: nil)
    }

    func testDurationsAndCountsReadTheWayTheVocabularyWritesThem() {
        XCTAssertEqual(ChatWords.duration(102_000), "1m 42s")
        XCTAssertEqual(ChatWords.duration(8_000), "8s")
        XCTAssertEqual(ChatWords.duration(4_200), "4.2s")
        XCTAssertEqual(ChatWords.tokens(148_000), "148k")
        XCTAssertEqual(
            ChatWords.compaction(before: 148_000, after: 22_000, automatic: false),
            "Compacted · 148k → 22k")
        XCTAssertEqual(ChatWords.turnEnd(durationMs: 102_000, costUsd: nil, failed: false), "Worked 1m 42s")
        XCTAssertEqual(ChatWords.thinking(open: false, durationMs: 8_000), "Thought for 8s")
    }

    func testARunSaysWhatItDidAndThatItContinuesBelow() {
        let run = RunInfo(
            newest: "b", oldest: "a", reads: 4, searches: 2, len: 6, anchor: "", isSummary: true,
            openBelow: false)
        XCTAssertEqual(ChatWords.run(run), "4 reads · 2 searches")
        let open = RunInfo(
            newest: "b", oldest: "a", reads: 40, searches: 0, len: 41, anchor: "", isSummary: true,
            openBelow: true)
        XCTAssertEqual(ChatWords.run(open), "40+ reads · 1+ more")
    }

    func testACallsVerbSaysWhetherItWaitsRunsWasRefusedOrRan() {
        let words = { (state: ToolStateView, row: Row) in
            ChatWords.verb(state, row, wants: "Wants to run", doing: "Running", done: "Ran")
        }
        XCTAssertEqual(words(.pending, row(.stopped)), "Wants to run")
        XCTAssertEqual(words(.running, row(.stopped, attention: true)), "Wants to run")
        XCTAssertEqual(words(.running, row(.stopped)), "Running")
        XCTAssertEqual(words(.succeeded, row(.stopped)), "Ran")
        let denied = Decision(outcome: .denied, elsewhere: false, note: "Use cargo clean", scope: nil)
        XCTAssertEqual(words(.succeeded, row(.stopped, decision: denied)), "Denied")
    }

    func testADecisionIsMetaWithScopeNoteAndWhereItWasAnswered() {
        let allowed = Decision(outcome: .allowed, elsewhere: false, note: nil, scope: "this session")
        XCTAssertEqual(ChatWords.meta(["12s"], row(.stopped, decision: allowed)), "12s · allowed · this session")
        let denied = Decision(outcome: .denied, elsewhere: false, note: "Use cargo clean\nplease", scope: nil)
        XCTAssertEqual(
            ChatWords.meta(["denied"], row(.stopped, decision: denied), verb: "Denied"),
            "“Use cargo clean”")
        let elsewhere = Decision(outcome: .allowed, elsewhere: true, note: nil, scope: nil)
        XCTAssertEqual(ChatWords.decision(elsewhere), "allowed · in the terminal")
    }

    func testChoicesAreStatedAsOutcomes() {
        let always = Choice(
            outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", scope: .project, label: ""),
            primary: false, takesNote: false)
        XCTAssertEqual(ChatWords.choice(always), "Always allow cargo test in this project")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false)),
            "Allow commands starting with curl -s")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .deny(stops: false), primary: false, takesNote: true)), "Deny…")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false)),
            "Allow network access to localhost")
    }

    func testTheActivityLineNamesWhatIsHappeningAndForHowLong() {
        XCTAssertEqual(
            ChatWords.activity(.running(key: "k"), elapsedMs: 12_400, subject: "cargo test"),
            "Running cargo test · 12s")
        XCTAssertEqual(ChatWords.activity(.subagents(count: 3), elapsedMs: 5_000, subject: nil), "3 subagents working · 5s")
        XCTAssertEqual(
            ChatWords.activity(.retrying(attempt: 2, maxAttempts: 10, retryAtMs: nil), elapsedMs: 0, subject: nil),
            "Retrying · attempt 2 of 10")
    }

    func testTheEmptyFieldSaysWhoAMessageGoesToOrWhySendingWaits() {
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", away: nil, working: false),
            "Message refactor-auth")
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", away: nil, working: true),
            "Queue a message")
        XCTAssertEqual(
            ChatWords.placeholder(.disabled(.detached), agent: "a", host: "Studio", away: .plain, working: false),
            "Studio is out of reach · your draft is kept")
        XCTAssertEqual(
            ChatWords.placeholder(.resume, agent: "a", host: "Studio", away: nil, working: false),
            "a has exited · a message resumes it")
    }

    func testAQueuedPromptSaysWhetherItWasSteered() {
        let queued = QueuedRow(
            inputId: [1], text: [], mine: true, steered: false, canWithdraw: true, canSendNow: true,
            fromAgent: nil)
        XCTAssertEqual(ChatWords.queued(queued), "queued")
        var steered = queued
        steered.steered = true
        XCTAssertEqual(ChatWords.queued(steered), "steered")
        XCTAssertEqual(ChatWords.outbox(.notConfirmed), "not confirmed")
    }

    func testASecretAnswerIsNeverShown() {
        XCTAssertEqual(ChatWords.answer(AnswerView(picked: [], hidden: true, other: nil)), "answered (hidden)")
        XCTAssertEqual(
            ChatWords.answer(AnswerView(picked: ["macOS"], hidden: false, other: "BSD")), "macOS, “BSD”")
    }

    func testFormFieldsComeFromTheSchemaAndRequiredOnesGateSubmit() {
        let fields = FormField.parse("""
            {"required":["repo"],"properties":{"repo":{"type":"string","title":"Repository"},
            "count":{"type":"integer"},"assign":{"type":"boolean","default":true}}}
            """)
        XCTAssertEqual(fields.map(\.name), ["repo", "assign", "count"])
        XCTAssertFalse(fields[0].valid)
        var filled = fields
        filled[0].value = "jlw/amux"
        filled[2].value = "3"
        XCTAssertTrue(filled.allSatisfy(\.valid))
        XCTAssertEqual(FormField.content(filled), #"{"assign":true,"count":3,"repo":"jlw\/amux"}"#)
    }
}
