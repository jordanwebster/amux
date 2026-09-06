import Foundation
import XCTest
@testable import AmuxCore

/// What the transcript is allowed to say about a row it was sent.
///
/// Every case here is a row shape one of the three layers actually emits, taken
/// from that layer's own vocabulary. The point of the suite is that no layer is
/// read as another and that nothing arriving is dropped.
final class TranscriptRowsTests: XCTestCase {
    private func claude(_ id: Int, _ kind: JSONValue) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(id), "seq": .int(id + 1), "kind": kind,
        ]))
    }

    private func sdk(_ id: Int, _ kind: String, _ entry: JSONValue) -> FeedEntry {
        FeedEntry(layer: .claudeSdk, row: .object([
            "id": .int(id), "seq": .int(id + 1),
            "kind": .object(["kind": .string(kind), "entry": entry]),
        ]))
    }

    private func codex(_ id: Int, _ kind: JSONValue) -> FeedEntry {
        FeedEntry(layer: .codex, row: .object([
            "id": .int(id), "seq": .int(id + 1), "kind": kind,
        ]))
    }

    private func look(_ id: Int, path: String, grouped: Bool) -> FeedEntry {
        claude(id, .object([
            "entry": .string("tool"), "name": .string("Read"),
            "invocation": .object(["tool": .string("read"), "file_path": .string(path)]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("none")]),
            ]),
            "group_with_previous": .bool(grouped),
        ]))
    }

    // MARK: - Folding

    func testAGroupedRunOfLooksFoldsToItsCounts() {
        let rows = [
            look(0, path: "a.rs", grouped: false),
            look(1, path: "b.rs", grouped: true),
            claude(2, .object([
                "entry": .string("tool"), "name": .string("Grep"),
                "invocation": .object(["tool": .string("query"), "text": .string("INVALID_PIN")]),
                "outcome": .object(["outcome": .string("success"),
                                    "facts": .object(["facts": .string("none")])]),
                "group_with_previous": .bool(true),
            ])),
        ].transcriptRows()

        XCTAssertEqual(rows.count, 1)
        guard case .exploration(let reads, let searches, let last, let inside) = rows[0].kind else {
            return XCTFail("expected a folded run, got \(rows[0].kind)")
        }
        XCTAssertEqual(reads, 2)
        XCTAssertEqual(searches, 1)
        XCTAssertEqual(last, "INVALID_PIN")
        XCTAssertEqual(inside.map(\.verb), ["Read", "Read", "Searched"])
    }

    /// Grouping is the core's fact. Two looks the core did not group are two
    /// runs, however adjacent they happen to be.
    func testTwoRunsTheCoreKeptApartStayApart() {
        let rows = [
            look(0, path: "a.rs", grouped: false),
            look(1, path: "b.rs", grouped: false),
        ].transcriptRows()
        XCTAssertEqual(rows.count, 2)
    }

    func testAFoldEndsWhenSomethingElseHappens() {
        let rows = [
            look(0, path: "a.rs", grouped: false),
            claude(1, .object(["entry": .string("interruption"), "kind": .string("user")])),
            look(2, path: "b.rs", grouped: false),
        ].transcriptRows()
        XCTAssertEqual(rows.count, 3)
        XCTAssertEqual(rows[1].kind, .interrupted(toolUse: false))
    }

    // MARK: - Claude over a terminal

    func testAnEditIsItsPathAndItsArithmetic() {
        let row = claude(0, .object([
            "entry": .string("tool"), "name": .string("Edit"),
            "invocation": .object(["tool": .string("edit"),
                                   "file_path": .string("crates/amux-ui/src/pairing.rs")]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object([
                    "facts": .string("edit"),
                    "file_path": .string("crates/amux-ui/src/pairing.rs"),
                    "added": .int(9), "removed": .int(14),
                ]),
            ]),
        ])).kind
        XCTAssertEqual(row, .edit(path: "crates/amux-ui/src/pairing.rs", added: 9, removed: 14))
    }

    func testOutputIsKeptToItsHeadWithTheRestCounted() {
        let head = (1...6).map { "line \($0)" }.joined(separator: "\n")
        let row = claude(0, .object([
            "entry": .string("tool"), "name": .string("Bash"),
            "invocation": .object(["tool": .string("bash"),
                                   "command": .string("cargo test")]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"), "head": .string(head),
                                  "truncated": .bool(false)]),
            ]),
        ])).kind
        guard case .ran(let command, _, let output) = row else {
            return XCTFail("expected a ran row, got \(row)")
        }
        XCTAssertEqual(command, "cargo test")
        XCTAssertEqual(output?.head, "line 1\nline 2")
        XCTAssertEqual(output?.hidden, 4)
    }

    /// A clipped head cannot be counted, so the row must not claim a number.
    func testAClippedHeadSaysThereIsMoreWithoutInventingACount() {
        let row = claude(0, .object([
            "entry": .string("tool"), "name": .string("Bash"),
            "invocation": .object(["tool": .string("bash"), "command": .string("cargo test")]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"), "head": .string("boom\nand"),
                                  "truncated": .bool(true)]),
            ]),
        ])).kind
        guard case .ran(_, _, let output) = row else {
            return XCTFail("expected a ran row, got \(row)")
        }
        XCTAssertEqual(output?.hidden, -1)
    }

    func testADenialIsTypedAndItsReasonIsTheCoresOwn() {
        let row = claude(0, .object([
            "entry": .string("tool"), "name": .string("Bash"),
            "invocation": .object(["tool": .string("bash"), "command": .string("rm -rf target")]),
            "outcome": .object(["outcome": .string("denied"), "kind": .string("user_reject")]),
        ])).kind
        XCTAssertEqual(row, .denied(label: "rm -rf target", reason: "You said no"))
    }

    func testAMessageBetweenAgentsKeepsWhoSentIt() {
        let row = claude(0, .object([
            "entry": .string("agent_message"), "from": .string("relay-cleanup/mini"),
            "kind": .object(["message_kind": .string("message")]),
            "text": .string("Nothing here matches on it."),
        ])).kind
        XCTAssertEqual(row, .agentMessage(
            from: "relay-cleanup/mini", text: "Nothing here matches on it.",
            outbound: false, note: nil))
    }

    /// The envelope kind arrives as a tagged object, and two of its kinds are
    /// not messages at all. Read as a bare string — which is not what the core
    /// sends — every one of them would be drawn as an ordinary message and the
    /// line saying another agent's session ended would never appear.
    func testASenderThatEndedIsAnEventRatherThanAMessage() {
        for (kind, text) in [("exited", "relay-cleanup/mini ended its session"),
                             ("completed", "relay-cleanup/mini finished its turn")] {
            let row = claude(0, .object([
                "entry": .string("agent_message"), "from": .string("relay-cleanup/mini"),
                "kind": .object(["message_kind": .string(kind)]), "text": .string(""),
            ])).kind
            XCTAssertEqual(row, .exit(text: text))
        }
    }

    /// A kind this build has no case for keeps the label the carrier wrote,
    /// beside the message itself.
    func testAnUnknownEnvelopeKindKeepsItsLabel() {
        let row = claude(0, .object([
            "entry": .string("agent_message"), "from": .string("relay-cleanup/mini"),
            "kind": .object(["message_kind": .string("other"), "label": .string("handover")]),
            "text": .string("Taking this one over."),
        ])).kind
        XCTAssertEqual(row, .agentMessage(
            from: "relay-cleanup/mini", text: "Taking this one over.",
            outbound: false, note: "handover"))
    }

    func testAMessageThisAgentSentIsOutbound() {
        let row = claude(0, .object([
            "entry": .string("tool"), "name": .string("mcp__amux__send"),
            "invocation": .object(["tool": .string("amux_send"),
                                   "to": .string("relay-cleanup/mini"),
                                   "text": .string("Are you holding anything?")]),
            "outcome": .object(["outcome": .string("success"),
                                "facts": .object(["facts": .string("none")])]),
        ])).kind
        XCTAssertEqual(row, .agentMessage(
            from: "relay-cleanup/mini", text: "Are you holding anything?",
            outbound: true, note: nil))
    }

    func testCompactionKeepsWhatItCostAndInventsNothingWhenNobodyCounted() {
        XCTAssertEqual(
            claude(0, .object(["entry": .string("compaction"), "pre_tokens": .int(148_000),
                               "post_tokens": .int(22_000)])).kind,
            .compaction(before: 148_000, after: 22_000))
        XCTAssertEqual(
            claude(0, .object(["entry": .string("compaction")])).kind,
            .compaction(before: nil, after: nil))
    }

    func testAnUnknownRowIsKeptAndNamedByWhateverItSaidAboutItself() {
        XCTAssertEqual(
            claude(0, .object(["entry": .string("unrecognized"),
                               "row_type": .string("checkpoint")])).kind,
            .unreadable(label: "checkpoint"))
        XCTAssertEqual(
            claude(0, .object(["entry": .string("something_new")])).kind,
            .unreadable(label: "something_new"))
    }

    // MARK: - Claude over the SDK

    /// The SDK tags its kind on the row and carries the entry beside it. A
    /// terminal-shaped reader would find no `entry` string and call every SDK
    /// row unreadable, which is the mistake this asserts against.
    func testTheSdkLayerIsReadInItsOwnShape() {
        XCTAssertEqual(
            sdk(0, "message", .object(["text": .string("Done."),
                                       "finality": .string("complete")])).kind,
            .prose(markdown: "Done.", open: false))
        XCTAssertEqual(
            sdk(1, "task", .object(["task_id": .string("t"), "description": .string("spec-suite"),
                                    "subagent_type": .string("general-purpose"),
                                    "state": .string("completed")])).kind,
            .subagent(name: "spec-suite", kind: "general-purpose", state: "completed"))
    }

    func testAnSdkEditIsReadOffTheLandedPatch() {
        let row = sdk(0, "tool", .object([
            "tool_use_id": .string("toolu_1"), "name": .string("Edit"),
            "invocation": .object(["tool": .string("edit"), "file_path": .string("a.rs")]),
            "finality": .string("complete"),
            "result": .object([
                "text": .string("ok"), "is_error": .bool(false),
                "edit": .object(["file_path": .string("a.rs"), "added": .int(3),
                                 "removed": .int(1)]),
            ]),
        ])).kind
        XCTAssertEqual(row, .edit(path: "a.rs", added: 3, removed: 1))
    }

    func testAnSdkTurnThatErroredIsAProviderErrorRatherThanATurnEnd() {
        let row = sdk(0, "turn", .object([
            "outcome": .string("error"), "is_error": .bool(true),
            "errors": .array([.string("overloaded")]),
            "usage": .object([:]),
        ])).kind
        XCTAssertEqual(row, .providerError(message: "overloaded"))
    }

    // MARK: - Codex

    func testCodexSaysEverythingAsWorkAndEachKindBecomesItsOwnLine() {
        XCTAssertEqual(
            codex(0, .object([
                "entry": .string("work"), "item_id": .string("i"),
                "kind": .object(["work": .string("command"),
                                 "command": .string("cargo check"), "exit_code": .int(0)]),
                "state": .object(["state": .string("done"), "outcome": .string("succeeded")]),
                "stdout_head": .string(""), "stderr_head": .string(""),
                "output_truncated": .bool(false),
            ])).kind,
            .ran(command: "cargo check", meta: "exit 0", output: nil))

        XCTAssertEqual(
            codex(1, .object([
                "entry": .string("work"), "item_id": .string("i"),
                "kind": .object(["work": .string("command"), "command": .string("rm -rf /")]),
                "state": .object(["state": .string("denied")]),
                "stdout_head": .string(""), "stderr_head": .string(""),
                "output_truncated": .bool(false),
            ])).kind,
            .denied(label: "rm -rf /", reason: nil))
    }

    func testACodexTurnCarriesItsOwnEnding() {
        XCTAssertEqual(
            codex(0, .object([
                "entry": .string("turn"), "turn_id": .string("t"),
                "status": .object(["status": .string("interrupted")]),
            ])).kind,
            .interrupted(toolUse: false))
        XCTAssertEqual(
            codex(1, .object([
                "entry": .string("turn"), "turn_id": .string("t"),
                "status": .object(["status": .string("failed"),
                                   "message": .string("stream closed")]),
            ])).kind,
            .providerError(message: "stream closed"))
    }

    // MARK: - The rail

    func testOnlyWorkHangsOffTheRail() {
        let rows = [
            claude(0, .object(["entry": .string("prompt"), "text": .string("go")])),
            claude(1, .object(["entry": .string("message"),
                               "segments": .array([.string("done")]),
                               "finality": .object(["finality": .string("open")])])),
            claude(2, .object(["entry": .string("turn")])),
            claude(3, .object(["entry": .string("interruption"), "kind": .string("user")])),
        ].transcriptRows()
        XCTAssertEqual(rows.map(\.onRail), [false, false, false, true])
    }
}
