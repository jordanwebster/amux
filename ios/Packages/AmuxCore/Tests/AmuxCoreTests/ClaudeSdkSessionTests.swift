import XCTest
@testable import AmuxCore

final class ClaudeSdkSessionTests: XCTestCase {
    private let agent = AgentId("00000000-0000-0000-0000-000000000001")!

    func testSDKComposerUsesItsOwnGateAndInterrupt() throws {
        for (gate, expected) in [(ClaudeSdkSendGate.ready, true), (.working, false),
                                 (.replaying, false), (.inputInFlight, false)] {
            let value = SendGate.claudeSdk(gate)
            XCTAssertEqual(value.accepts, expected)
            XCTAssertEqual(try AmuxJSON.decoder.decode(SendGate.self, from: AmuxJSON.encoder.encode(value)), value)
        }
        XCTAssertEqual(ComposerState(gate: .claudeSdk(.ready), tail: nil, elapsed: nil), .writing)
        XCTAssertTrue(ComposerState(gate: .claudeSdk(.working), tail: nil, elapsed: nil)?.busy == true)
        XCTAssertNil(ComposerState(gate: .claudeSdk(.inputInFlight), tail: nil, elapsed: nil))
        XCTAssertEqual(SendGate.claudeSdk(.working).interrupt(agent)?["claude_sdk_command"], .string("interrupt"))
    }

    func testSDKPermissionAndPlanUseTheSDKAddressAndAnswerEnvelope() throws {
        let ask = Ask(layer: .claudeSdk, body: .object([
            "id": .int(7), "request_id": .string("sdk-permission"),
            "state": .object(["state": .string("pending")]),
            "kind": .object([
                "kind": .string("permission"), "tool_name": .string("Bash"),
                "invocation": .object(["tool": .string("bash"), "command": .string("pwd")]),
                "suggestions": .array([]),
            ]),
        ]))
        let panel = try XCTUnwrap(ask.panel)
        XCTAssertEqual(panel.address, .claudeSdk(ask: 7))
        guard case .permission(let permission) = panel.kind else { return XCTFail("expected permission") }
        XCTAssertNil(permission.unanswerable, "SDK allows once even without a standing-grant suggestion")
        XCTAssertEqual(permission.subject, "pwd")
        let command = try XCTUnwrap(panel.command(.allowOnce, agent: agent))
        XCTAssertEqual(command["command"], .string("claude_sdk"))
        XCTAssertEqual(command["ask"], .int(7))
        XCTAssertEqual(command["answer"], .object([
            "answer": .string("permission"), "value": .object(["permission": .string("allow_once")]),
        ]))
        var plan = ask
        plan.body = .object([
            "id": .int(9), "state": .object(["state": .string("pending")]),
            "kind": .object(["kind": .string("plan"), "plan": .string("# Change the parser")]),
        ])
        XCTAssertEqual(plan.panel?.kind, .plan(.init(markdown: "# Change the parser", path: nil)))
        XCTAssertEqual(plan.panel?.command(.approvePlan, agent: agent)?["answer"], .object([
            "answer": .string("plan"), "value": .object(["plan": .string("approve_manual")]),
        ]))
    }
    func testSDKQuestionKeepsTheSelectedOptionAndUsesTheSDKEnvelope() throws {
        let ask = Ask(layer: .claudeSdk, body: .object([
            "id": .int(4), "state": .object(["state": .string("pending")]),
            "kind": .object([
                "kind": .string("question"), "questions": .array([.object([
                    "question": .string("Which parser?"), "multi_select": .bool(false),
                    "options": .array([.object(["label": .string("The tokenizer")])]),
                ])]),
            ]),
        ]))
        let panel = try XCTUnwrap(ask.panel)
        guard case .question(let questions) = panel.kind else { return XCTFail("expected question") }
        XCTAssertEqual(questions.first?.options.first?.label, "The tokenizer")
        XCTAssertEqual(panel.command(.answered([.init(selected: [0])]), agent: agent)?["answer"], .object([
            "answer": .string("question"),
            "value": .array([.object(["selected": .array([.int(0)]), "other": .null])]),
        ]))
    }

    @MainActor
    func testSDKPromptEchoKeepsItsLayerAndReconcilesAgainstTheNativePrompt() {
        let store = ConversationStore(agent: agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudeSdk(.ready), phase: .claudeSdk(.object(["phase": .string("idle")])),
            stream: .live, asks: [], facts: .claudeSdk(.object(["layer": .string("claude_sdk")])),
            provider: ProviderFacts(), settingsGate: .ready, queue: nil, family: [])))
        store.sent("Read the parser")
        XCTAssertEqual(store.rows().map(\.layer), [.claudeSdk])
        store.apply(.feed(FeedUpdate(agent: agent, base: 0, append: [FeedEntry(layer: .claudeSdk, row: .object([
            "id": .int(0), "seq": .int(2),
            "kind": .object(["kind": .string("prompt"), "entry": .object(["text": .string("Read the parser")])]),
        ]))], replace: [], evicted: 0)))
        XCTAssertTrue(store.unacknowledged.isEmpty)
        XCTAssertEqual(store.rows().count, 1)
        XCTAssertEqual(store.rows().first?.layer, .claudeSdk)
        XCTAssertEqual(store.rows().first?.kind, .prompt(text: "Read the parser"))
    }

}
