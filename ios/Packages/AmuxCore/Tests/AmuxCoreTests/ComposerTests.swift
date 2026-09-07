import Foundation
import XCTest
@testable import AmuxCore

/// What the composer is, before anything draws it.
///
/// The box has three jobs a screenshot cannot check: it must not appear at the
/// bottom of a conversation nobody can write to, it must name what the agent
/// is doing in the agent's own words, and a message must be on screen before
/// the machine has acknowledged it and gone again when the machine's own row
/// arrives.
@MainActor
final class ComposerTests: XCTestCase {
    private let agent = Made.agentId(1)

    private func prompt(_ id: Int, seq: Int, text: String) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(id), "seq": .int(seq),
            "kind": .object(["entry": .string("prompt"), "text": .string(text)]),
        ]))
    }

    private func running(_ id: Int, seq: Int, command: String) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(id), "seq": .int(seq),
            "kind": .object([
                "entry": .string("tool"),
                "tool_use_id": .string("toolu_\(id)"),
                "name": .string("Bash"),
                "invocation": .object([
                    "tool": .string("bash"), "command": .string(command),
                    "description": .null,
                ]),
                "outcome": .object(["outcome": .string("pending")]),
                "message_final": .bool(true),
                "group_with_previous": .bool(false),
                "message_id": .string("msg_\(id)"),
            ]),
        ]))
    }

    // MARK: - Whether there is a box at all

    func testAReadyLayerIsWrittenToAndAWorkingOneIsQueuedTo() {
        XCTAssertEqual(
            ComposerState(gate: .claudePty(.ready), tail: nil, elapsed: nil), .writing)
        XCTAssertEqual(
            ComposerState(gate: .codex(.ready), tail: nil, elapsed: nil), .writing)
        XCTAssertEqual(
            ComposerState(gate: .claudePty(.working), tail: nil, elapsed: "8s")?.activity?.elapsed,
            "8s")
        XCTAssertEqual(
            ComposerState(gate: .codex(.activeTurn), tail: nil, elapsed: nil)?.activity?.name,
            "Working")
    }

    /// Every gate that will not take a message draws no box. What is said in
    /// its place is the ask panel's or the foot's, and two of them at once
    /// would be two answers to the same question.
    func testNothingThatWillNotTakeAMessageDrawsAComposer() {
        let closed: [SendGate] = [
            .claudePty(.exited), .claudePty(.readOnly), .claudePty(.replaying),
            .claudePty(.needsYou), .claudePty(.unknown), .claudePty(.sendInFlight),
            .claudePty(.unavailable),
            .codex(.exited), .codex(.closed), .codex(.replaying), .codex(.needsYou),
            .codex(.observerReadOnly), .codex(.readOnly), .codex(.unknown),
            .codex(.inputInFlight), .codex(.unavailable),
            .unavailable,
        ]
        for gate in closed {
            XCTAssertNil(
                ComposerState(gate: gate, tail: nil, elapsed: nil),
                "\(gate) offered a composer nobody can write in")
        }
    }

    func testTheEmptyFieldNamesTheAgentUntilATurnIsRunning() {
        XCTAssertEqual(
            ComposerState.writing.placeholder(agent: "refactor-auth"),
            "Message refactor-auth")
        XCTAssertEqual(
            ComposerState.working(ComposerActivity(name: "Thinking", elapsed: "8s"))
                .placeholder(agent: "refactor-auth"),
            "Queue a message")
    }

    // MARK: - What it says the agent is doing

    /// One word for the row, not the row again: `cargo test --workspace` is
    /// on the screen directly above and would only truncate down here.
    func testTheActivityIsNamedByTheRowThatIsStillOpen() {
        let store = ConversationStore(agent: agent)
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0,
            append: [prompt(0, seq: 1, text: "Run the suite"),
                     running(1, seq: 2, command: "cargo test --workspace")],
            replace: [], evicted: 0)))
        let state = ComposerState(
            gate: .claudePty(.working), tail: store.tailRow, elapsed: "8s")
        XCTAssertEqual(
            state?.activity,
            ComposerActivity(name: "Running", elapsed: "8s"))
    }

    /// The elapsed time is the fleet's arithmetic. A machine that has not said
    /// when the work started gets a name and no number rather than a zero.
    func testAnActivityWithNoStartingTimeCarriesNoNumber() {
        let state = ComposerState(gate: .claudePty(.working), tail: nil, elapsed: nil)
        XCTAssertEqual(state?.activity?.elapsed, nil)
    }

    func testTheFleetCountsFromWhenTheAgentSaidItStarted() {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        var card = Made.card(1, name: "refactor-auth", attention: .working,
                             minutesAgo: 40, now: now)
        card.agent.workingOn = WorkingOn(
            text: "Running the spec suite", updatedAt: now.addingTimeInterval(-8))
        let row = AgentRow(card: card, unread: false)
        // Forty minutes since it last spoke, eight seconds on this piece of
        // work. The composer reports the second, which is the one that answers
        // "how long has this been going".
        XCTAssertEqual(row.age(at: now), "40m")
        XCTAssertEqual(row.working(at: now), "8s")
    }

    // MARK: - A sent message before it is acknowledged

    func testASentMessageIsInTheFeedBeforeTheHostHasEchoedIt() {
        let store = ConversationStore(agent: agent)
        store.sent("Run the suite")
        XCTAssertEqual(store.rows().count, 1)
        XCTAssertEqual(store.rows().last?.kind, .prompt(text: "Run the suite"))
        XCTAssertTrue(store.rows().last?.id.hasPrefix("pending-") ?? false)
    }

    func testTheHostsOwnRowReplacesTheOptimisticOne() {
        let store = ConversationStore(agent: agent)
        store.sent("Run the suite")
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0, append: [prompt(0, seq: 1, text: "Run the suite")],
            replace: [], evicted: 0)))
        XCTAssertTrue(store.unacknowledged.isEmpty)
        XCTAssertEqual(store.rows().count, 1)
        XCTAssertEqual(store.rows().first?.id, "claude_pty:0")
    }

    /// A message that is a command is drawn as the command it is, not as its
    /// arguments on their own — which is also the only way the host's echo of
    /// it can ever match the row already on screen.
    func testACommandsOwnRowReplacesTheOptimisticOne() {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.dispatch = { _ in OpId(UUID()) }
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))
        store.draft.body = "/plan"
        store.draft.pick(ProviderCommand(name: "plan", source: .null))
        store.draft.insert(text: " the parser before the wire format")
        XCTAssertTrue(bundle.send(to: agent))
        XCTAssertEqual(
            store.unacknowledged.map(\.text),
            ["/plan the parser before the wire format"])

        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0,
            append: [prompt(0, seq: 1, text: "/plan the parser before the wire format")],
            replace: [], evicted: 0)))
        XCTAssertTrue(store.unacknowledged.isEmpty)
        XCTAssertEqual(store.rows().count, 1)
        XCTAssertEqual(store.rows().first?.id, "claude_pty:0")
    }

    /// A row arriving that is not the message that was sent leaves the
    /// optimistic one where it is. Dropping it on any traffic at all would
    /// take a message off the screen the host had not taken yet.
    func testAnUnrelatedRowDoesNotAcknowledgeAnything() {
        let store = ConversationStore(agent: agent)
        store.sent("Run the suite")
        store.apply(.feed(FeedUpdate(
            agent: agent, base: 0, append: [prompt(0, seq: 1, text: "Something else")],
            replace: [], evicted: 0)))
        XCTAssertEqual(store.unacknowledged.map(\.text), ["Run the suite"])
        XCTAssertEqual(store.rows().count, 2)
    }

    // MARK: - What leaves the phone

    func testSendingDispatchesTheSharedSendAndClearsTheDraft() throws {
        let bundle = StoreBundle(account: AccountId("test"))
        var sent: [JSONValue] = []
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            sent.append(body)
            return OpId(UUID())
        }
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))
        store.draft.body = "Run the suite"
        XCTAssertTrue(bundle.send(to: agent))
        XCTAssertEqual(sent.first?["command"]?.stringValue, "send")
        XCTAssertNil(sent.first?["action"])
        XCTAssertTrue(store.draft.isEmpty)
        XCTAssertEqual(store.unacknowledged.map(\.text), ["Run the suite"])
    }

    /// A message written while a turn runs is held by the machine, not by the
    /// phone. It is not in the transcript yet either, so nothing optimistic is
    /// drawn for it.
    func testWritingDuringATurnHoldsTheMessageInsteadOfSendingIt() {
        let bundle = StoreBundle(account: AccountId("test"))
        var sent: [JSONValue] = []
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            sent.append(body)
            return OpId(UUID())
        }
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.working), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))
        store.draft.body = "And then the docs"
        XCTAssertTrue(bundle.send(to: agent))
        XCTAssertEqual(sent.first?["command"]?.stringValue, "queue")
        XCTAssertEqual(sent.first?["action"]?.stringValue, "hold")
        XCTAssertTrue(store.unacknowledged.isEmpty)
    }

    /// Interrupting says so in the layer's own words and leaves the draft
    /// alone: stopping an agent is usually the first half of telling it what
    /// to do instead.
    func testInterruptingKeepsWhatWasWritten() {
        let bundle = StoreBundle(account: AccountId("test"))
        var sent: [JSONValue] = []
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            sent.append(body)
            return OpId(UUID())
        }
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.working), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [])))
        store.draft.body = "wait"
        XCTAssertTrue(bundle.interrupt(agent))
        XCTAssertEqual(sent.first?["command"]?.stringValue, "claude")
        XCTAssertEqual(sent.first?["claude_command"]?.stringValue, "interrupt")
        XCTAssertEqual(store.draft.body, "wait")
    }

    func testCodexStopsItsOwnWay() {
        let interrupt = SendGate.codex(.activeTurn).interrupt(agent)
        XCTAssertEqual(interrupt?["command"]?.stringValue, "codex")
        XCTAssertEqual(interrupt?["codex_command"]?.stringValue, "interrupt")
        XCTAssertNil(SendGate.unavailable.interrupt(agent))
    }

    func testAnEmptyDraftSendsNothing() {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.dispatch = { _ in XCTFail("an empty draft left the phone"); return nil }
        bundle.conversation(agent).draft.body = "   \n "
        XCTAssertFalse(bundle.send(to: agent))
    }
}
