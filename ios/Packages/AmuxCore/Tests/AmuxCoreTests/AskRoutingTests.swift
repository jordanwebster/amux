import Foundation
import XCTest
@testable import AmuxCore

/// Where an ask that is not this agent's own gets answered, and what a
/// decision leaves behind.
///
/// Two facts are under test and both are about addressing. A child's ask is
/// answered in the child's conversation and addressed to the child, so the
/// list a parent draws has to carry an identity a person can be taken to —
/// and work the provider runs inside this session has no identity at all and
/// must say so rather than offering a tap that goes nowhere. A plan verdict is
/// addressed to the past: the transcript keeps the document that was judged,
/// so reopening the decision shows what was in front of the person then.
@MainActor
final class AskRoutingTests: XCTestCase {
    private let parent = Made.agentId(1)
    private let child = Made.agentId(2)

    // MARK: - Children

    func testAChildsAskIsListedAgainstTheChildItRoutesTo() throws {
        let store = ConversationStore(agent: parent)
        store.apply(session(family: [
            FamilyMember(agent: child, depth: 1, needs: .permission),
            FamilyMember(agent: Made.agentId(3), depth: 1, needs: nil),
        ]))
        let children = store.children(named: { $0 == self.child ? "spec-fixer" : "docs-sweep" })
        XCTAssertEqual(children.map(\.name), ["spec-fixer", "docs-sweep"])
        let waiting = try XCTUnwrap(children.first)
        XCTAssertEqual(waiting.needs, .permission)
        XCTAssertEqual(waiting.openable, child)
        XCTAssertNil(waiting.unopenable)
    }

    /// The routing this is really about: the answer goes to the agent the
    /// list pointed at. Addressing it to the conversation the person was
    /// reading would answer the parent's ask, which is a different ask or no
    /// ask at all.
    func testAnsweringAChildsAskAddressesTheChild() throws {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.apply(session(family: [
            FamilyMember(agent: child, depth: 1, needs: .permission),
        ]))
        bundle.apply(.session(SessionSnapshot(
            agent: child, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [Self.permissionAsk], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))

        let listed = try XCTUnwrap(bundle.conversation(parent).children().first)
        let goingTo = try XCTUnwrap(listed.openable, "an agent's child is somewhere to go")
        let panel = try XCTUnwrap(bundle.conversation(goingTo).asks.panel)
        let command = try XCTUnwrap(panel.command(.allowOnce, agent: goingTo))
        XCTAssertEqual(command["agent"]?.stringValue, child.description)
        XCTAssertNotEqual(command["agent"]?.stringValue, parent.description)
        XCTAssertEqual(command["ask"]?.intValue, 7)
    }

    /// The answer leaves the phone, and the reply belongs to whoever answered.
    ///
    /// A tap on a panel decides nothing on its own: the bundle is what reaches
    /// the machine, and the operation it comes back with is what makes the
    /// host's reply this conversation's rather than whichever one is on
    /// screen. A bundle with no connection behind it says the answer did not
    /// go, which is what a fixture and a replay want.
    func testAnsweringSendsTheSharedCommandAndKeepsTheOperation() throws {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.apply(.session(SessionSnapshot(
            agent: child, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [Self.permissionAsk], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))
        let panel = try XCTUnwrap(bundle.conversation(child).asks.panel)

        XCTAssertFalse(bundle.answer(panel, .allowOnce, of: child),
                       "a bundle with nothing connected claimed the answer had gone")

        var sent: [BridgeCommand] = []
        let op = OpId(UUID())
        bundle.dispatch = { command in
            sent.append(command)
            return op
        }
        XCTAssertTrue(bundle.answer(panel, .deny(feedback: nil), of: child))
        guard case .shared(let command) = try XCTUnwrap(sent.first) else {
            return XCTFail("the answer was not sent as a shared command: \(sent)")
        }
        XCTAssertEqual(command["command"]?.stringValue, "claude")
        XCTAssertEqual(command["claude_command"]?.stringValue, "answer_ask")
        XCTAssertEqual(command["agent"]?.stringValue, child.description)
        XCTAssertEqual(command["answer"]?["permission"]?.stringValue, "deny")
        // Claimed, shown by the one thing a claim is for: the host's reply to
        // that operation lands in the conversation that answered and nowhere
        // else.
        bundle.apply(.opResult(OpResult(op: op, outcome: .inputSent)))
        XCTAssertEqual(bundle.conversation(child).results.map(\.op), [op])
        XCTAssertTrue(bundle.conversation(parent).results.isEmpty,
                      "the reply to the child's answer landed in the parent as well")
    }

    /// A subagent is the provider's own work and has no session behind it.
    /// The list says why instead of offering a control that would do nothing.
    func testProviderInternalWorkCannotBeOpenedAndSaysWhy() throws {
        let store = ConversationStore(agent: parent)
        store.apply(session(family: []))
        store.apply(.feed(FeedUpdate(
            agent: parent, base: 0,
            append: [Self.taskLaunched, Self.taskCompleted], replace: [], evicted: 0)))
        let children = store.children()
        XCTAssertEqual(children.count, 1, "one subagent, launched and then finished")
        let inside = try XCTUnwrap(children.first)
        XCTAssertNil(inside.openable)
        XCTAssertEqual(inside.state, "finished")
        XCTAssertNil(inside.needs, "a provider's own subagent never asks under its own name")
        XCTAssertEqual(
            inside.unopenable,
            "This one runs inside the session and has no conversation of its own.")
    }

    func testAgentsAndProviderInternalWorkAreListedTogetherAgentsFirst() {
        let store = ConversationStore(agent: parent)
        store.apply(session(family: [
            FamilyMember(agent: child, depth: 1, needs: .finished),
        ]))
        store.apply(.feed(FeedUpdate(
            agent: parent, base: 0, append: [Self.taskLaunched], replace: [], evicted: 0)))
        let children = store.children(named: { _ in "spec-fixer" })
        XCTAssertEqual(children.map(\.openable), [child, nil])
        XCTAssertEqual(children.map(\.name), ["spec-fixer", "Audit the retry budget · explore"])
    }

    // MARK: - Recorded verdicts

    func testAnApprovedPlanIsRecordedWithTheDocumentThatWasJudged() throws {
        let rows = [Self.planTool(outcome: .object([
            "outcome": .string("success"),
            "facts": .object([
                "facts": .string("plan_approved"),
                "plan_file_path": .string("/work/plan.md"),
            ]),
        ]))].transcriptRows()
        guard case .planVerdict(let verdict) = try XCTUnwrap(rows.first).kind else {
            return XCTFail("expected a plan verdict, got \(rows.first?.kind as Any)")
        }
        XCTAssertEqual(verdict.decision, .approved)
        XCTAssertEqual(verdict.markdown, Self.planMarkdown)
        XCTAssertEqual(verdict.path, "/work/plan.md")
    }

    func testAPlanSentBackKeepsTheLayersOwnReasonAndTheSameDocument() throws {
        let rows = [Self.planTool(outcome: .object([
            "outcome": .string("denied"), "kind": .string("user_reject"),
        ]))].transcriptRows()
        guard case .planVerdict(let verdict) = try XCTUnwrap(rows.first).kind else {
            return XCTFail("expected a plan verdict, got \(rows.first?.kind as Any)")
        }
        XCTAssertEqual(verdict.decision, .sentBack(reason: "You said no"))
        XCTAssertEqual(verdict.markdown, Self.planMarkdown)
    }

    /// A plan nobody has answered is still an ask, not a decision. Recording a
    /// verdict for it would put a judgement in the transcript that nobody made.
    func testAPlanNobodyHasAnsweredIsNotAVerdict() throws {
        let rows = [Self.planTool(outcome: .object(["outcome": .string("pending")]))]
            .transcriptRows()
        if case .planVerdict = try XCTUnwrap(rows.first).kind {
            XCTFail("a pending plan is not a verdict")
        }
    }

    // MARK: - Fixtures

    private func session(family: [FamilyMember]) -> Event {
        .session(SessionSnapshot(
            agent: parent, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: family))
    }

    /// Claude asking the child, not the parent, to run something.
    private static let permissionAsk = Ask(layer: .claudePty, body: .object([
        "id": .int(7), "seq": .int(3),
        "session_ask_id": .string("ask-7"),
        "kind": .object([
            "ask": .string("permission"),
            "tool_name": .string("Bash"),
            "invocation": .object([
                "tool": .string("bash"), "command": .string("cargo test --workspace"),
            ]),
            "suggestions": .array([.object([
                "kind": .string("add_directories"),
                "destination": .string("session"),
                "directories": .array([.string("/work")]),
            ])]),
        ]),
        "state": .object(["state": .string("pending")]),
    ]))

    private static let taskLaunched = FeedEntry(layer: .claudePty, row: .object([
        "id": .int(0), "seq": .int(1),
        "kind": .object([
            "entry": .string("tool"), "name": .string("Task"),
            "invocation": .object([
                "tool": .string("task"),
                "description": .string("Audit the retry budget"),
                "subagent_type": .string("explore"),
            ]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object([
                    "facts": .string("task_launched"),
                    "agent_id": .string("Audit the retry budget"),
                ]),
            ]),
        ]),
    ]))

    private static let taskCompleted = FeedEntry(layer: .claudePty, row: .object([
        "id": .int(1), "seq": .int(2),
        "kind": .object([
            "entry": .string("tool"), "name": .string("Task"),
            "invocation": .object([
                "tool": .string("task"),
                "description": .string("Audit the retry budget"),
                "subagent_type": .string("explore"),
            ]),
            "outcome": .object([
                "outcome": .string("success"),
                "facts": .object([
                    "facts": .string("task_completed"),
                    "agent_id": .string("Audit the retry budget"),
                ]),
            ]),
        ]),
    ]))

    private static let planMarkdown = """
        ## Approach

        1. Replace the match in `pairing.rs` with one arm
        2. Delete the three constants nothing reads
        """

    private static func planTool(outcome: JSONValue) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(4), "seq": .int(9),
            "kind": .object([
                "entry": .string("tool"), "name": .string("ExitPlanMode"),
                "invocation": .object([
                    "tool": .string("plan"), "plan": .string(planMarkdown),
                    "plan_file_path": .null,
                ]),
                "outcome": outcome,
            ]),
        ]))
    }
}
