import Foundation
import XCTest

@testable import AmuxCore

/// Typing a command, picking one, and what the core is told when it is sent.
///
/// A command is not a word and not an attachment: it is what the message *is*,
/// and the core takes it as its own segment, first and alone. So these check
/// both halves — what the composer offers while it is being typed, and the
/// shape that reaches the core once it has been picked.
@MainActor
final class SlashCommandTests: XCTestCase {
    private let commands = [
        ProviderCommand(name: "code-review", source: .string("codex")),
        ProviderCommand(
            name: "stripe:connect-recommend", source: .object(["plugin": .string("stripe")])),
        ProviderCommand(name: "compact", source: .string("codex")),
        ProviderCommand(name: "context", source: .string("codex")),
        ProviderCommand(name: "copy-transcript", source: .string("codex"), terminalOnly: true),
        ProviderCommand(name: "handoff", source: .string("codex")),
    ]

    private var agent: AgentId {
        AgentId(UUID(uuidString: "00000000-0000-4000-8000-0000000000a1")!)
    }

    private func typed(_ text: String) -> MessageDraft {
        var draft = MessageDraft()
        draft.insert(text: text)
        return draft
    }

    private func offered(
        _ text: String, layer: SessionFacts = .codex(.object([:]))
    ) -> SlashCommands? {
        SlashCommands.offered(
            for: typed(text), facts: layer, provider: ProviderFacts(commands: commands))
    }

    private func named(_ name: String) throws -> ProviderCommand {
        try XCTUnwrap(commands.first { $0.name == name })
    }

    private func sentSegments(_ draft: MessageDraft) throws -> [JSONValue] {
        let command = try XCTUnwrap(draft.command(to: agent))
        guard case .shared(let body) = command else {
            XCTFail("a send is a shared command")
            return []
        }
        return try XCTUnwrap(body["draft"]?["segments"]?.arrayValue)
    }

    func testASlashRaisesTheSessionsOwnCommands() throws {
        let raised = try XCTUnwrap(offered("/"))
        XCTAssertEqual(raised.typed, "")
        // Five at most, in the order the session reported them, and the
        // terminal-only one is not among them.
        XCTAssertEqual(raised.rows.map(\.name), [
            "code-review", "stripe:connect-recommend", "compact", "context", "handoff",
        ])
    }

    func testWhatFollowsFiltersThem() throws {
        let raised = try XCTUnwrap(offered("/co"))
        XCTAssertEqual(raised.typed, "co")
        // The plugin's command is here because the name after its namespace
        // starts with what was typed. Nobody reaching for connect-recommend is
        // thinking of the plugin it came from.
        XCTAssertEqual(raised.rows.map(\.name), [
            "code-review", "stripe:connect-recommend", "compact", "context",
        ])
    }

    func testAMatchInTheMiddleOfANameIsNotAMatch() {
        // "nect" is inside "connect-recommend" and "view" is inside
        // "code-review", and neither is the start of a name or of the part
        // after a namespace.
        XCTAssertNil(offered("/nect"))
        XCTAssertNil(offered("/view"))
    }

    func testATerminalOnlyCommandIsNeverOffered() {
        XCTAssertNil(offered("/copy"))
    }

    func testEachRowNamesItsSource() throws {
        let raised = try XCTUnwrap(offered("/"))
        XCTAssertEqual(raised.rows[0].origin, "Codex")
        XCTAssertEqual(raised.rows[1].origin, "stripe")
    }

    func testNothingIsOfferedToAPtyClaudeAgent() {
        XCTAssertNil(offered("/co", layer: .claudePty(.object([:]))))
        XCTAssertNil(offered("/co", layer: .claudeSdk(supported: false)))
        XCTAssertNotNil(offered("/co", layer: .claudeSdk(supported: true)))
    }

    func testASlashInTheMiddleOfASentenceIsASlash() {
        XCTAssertNil(offered("see /co"))
        // The space is where the command stops being typed: by then it has
        // either been picked or it is prose.
        XCTAssertNil(offered("/co "))
    }

    func testPickingCommitsTheCommandAndWhatFollowsIsItsArguments() throws {
        var draft = typed("/co")
        draft.pick(try named("code-review"))
        XCTAssertEqual(draft.command, "code-review")
        // What was typed is gone: the token stands where it was, and in the
        // draft it is one character wide.
        XCTAssertEqual(draft.body.count, 1)
        draft.insert(text: "the pairing change only")
        XCTAssertEqual(draft.command, "code-review")
        XCTAssertEqual(draft.text, "the pairing change only")
        // Nothing is offered any more: the command has been picked.
        XCTAssertNil(SlashCommands.offered(
            for: draft, facts: .codex(.object([:])),
            provider: ProviderFacts(commands: commands)))
    }

    func testOneBackspaceTakesTheWholeCommand() throws {
        var draft = typed("/co")
        draft.pick(try named("compact"))
        draft.place(caret: 1)
        draft.backspace()
        XCTAssertNil(draft.command)
        XCTAssertTrue(draft.isEmpty)
    }

    func testTheCoreIsToldTheCommandFirstAndTheArgumentsAfterIt() throws {
        var draft = typed("/co")
        draft.pick(try named("compact"))
        draft.insert(text: "keep the pairing decisions")
        let segments = try sentSegments(draft)
        XCTAssertEqual(segments.count, 2)
        XCTAssertEqual(segments[0]["segment"]?.stringValue, "command_token")
        XCTAssertEqual(segments[0]["name"]?.stringValue, "compact")
        XCTAssertEqual(segments[1]["segment"]?.stringValue, "text")
        XCTAssertEqual(segments[1]["text"]?.stringValue, "keep the pairing decisions")
        // A command segment carries a name and nothing else: a null beside the
        // tag is a field the core never wrote.
        XCTAssertNil(segments[0]["text"])
    }

    func testADraftWithNoCommandIsStillOneTextSegment() throws {
        var draft = MessageDraft()
        draft.insert(text: "just words")
        let segments = try sentSegments(draft)
        XCTAssertEqual(segments.count, 1)
        XCTAssertEqual(segments[0]["segment"]?.stringValue, "text")
        XCTAssertEqual(segments[0]["text"]?.stringValue, "just words")
    }
}
