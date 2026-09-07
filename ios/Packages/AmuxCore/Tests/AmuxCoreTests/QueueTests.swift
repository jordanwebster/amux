import Foundation
import XCTest
@testable import AmuxCore

/// The one message an agent holds until its turn ends, and the two gestures
/// that change it.
///
/// The core owns the queue and the phone owns nothing of it: what is held, how
/// it is spelled and whether it can still be touched are all read back from
/// the host. These pin that reading, and the exact bytes the phone writes when
/// somebody replaces or takes back what they queued.
@MainActor
final class QueueTests: XCTestCase {
    private let agent = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000B1")!)

    private func bundle(recording sent: @escaping (JSONValue) -> Void) -> StoreBundle {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            sent(body)
            return OpId(UUID())
        }
        return bundle
    }

    private func working(
        _ bundle: StoreBundle, queue: QueuedMessage? = nil, provider: ProviderFacts = ProviderFacts(),
        family: [FamilyMember] = []
    ) -> ConversationStore {
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.working), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable, provider: provider,
            settingsGate: .unavailable, queue: queue, family: family)))
        return store
    }

    private func held(
        _ text: String, delivery: QueueDelivery = .held
    ) -> QueuedMessage {
        QueuedMessage(
            draft: HeldDraft(segments: [.text(text)]),
            heldAt: Date(timeIntervalSince1970: 1_700_000_000), delivery: delivery)
    }

    // MARK: - What the host says is held

    /// The held draft is read the way the shared library writes it, straight
    /// off the projection's own pinned file. A change to the shape fails here
    /// rather than at a strip that quietly stops saying anything.
    func testTheHeldMessageIsReadFromThePinnedProjection() throws {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "queue", withExtension: "json"),
            "the pinned held-queue projection is missing from the test bundle")
        let event = try AmuxJSON.decoder.decode(Event.self, from: Data(contentsOf: url))
        guard case .session(let session) = event else {
            return XCTFail("expected a session, got \(event)")
        }
        let queued = try XCTUnwrap(session.queue)
        XCTAssertEqual(queued.draft.segments, [.text("next step")])
        XCTAssertEqual(queued.text, "next step")
        XCTAssertEqual(queued.delivery, .held)
        XCTAssertTrue(queued.changeable)
        XCTAssertTrue(queued.draft.attachments.isEmpty)
    }

    /// A command a person queued is still the command they picked. It travels
    /// as its own segment rather than as typed characters, and reads back as
    /// the command it is.
    func testAQueuedCommandReadsBackAsTheCommandItIs() {
        let queued = QueuedMessage(
            draft: HeldDraft(segments: [.command(name: "compact"), .text(" the history")]),
            heldAt: Date(timeIntervalSince1970: 1_700_000_000), delivery: .held)
        XCTAssertEqual(queued.text, "/compact the history")
    }

    /// A message the turn already released cannot be changed. The core refuses
    /// to replace or cancel one, so the phone does not offer the gesture.
    func testAMessageOnItsWayCannotBeTakenBack() {
        let bundle = bundle { _ in XCTFail("a message already sending left the phone") }
        let store = working(bundle, queue: held("too late", delivery: .sending(op: OpId(UUID()))))
        XCTAssertFalse(store.queued?.changeable ?? true)
        XCTAssertFalse(bundle.unqueue(agent))
        XCTAssertTrue(store.draft.isEmpty)
    }

    // MARK: - Taking it back

    /// Unqueueing is not editing in place and not discarding: the text lands
    /// in the field as an ordinary unsent message, and the host is told to
    /// stop holding it.
    func testUnqueueingPutsTheTextInTheFieldAndCancelsTheHold() {
        var sent: [JSONValue] = []
        let bundle = bundle { sent.append($0) }
        let store = working(bundle, queue: held("Squash it once the suite is green."))
        XCTAssertTrue(bundle.unqueue(agent))
        XCTAssertEqual(store.draft.body, "Squash it once the suite is green.")
        XCTAssertEqual(store.draft.caret, store.draft.body.count)
        XCTAssertEqual(sent.first?["command"]?.stringValue, "queue")
        XCTAssertEqual(sent.first?["action"]?.stringValue, "cancel")
        XCTAssertEqual(sent.first?["agent"]?.stringValue, agent.description)
        XCTAssertNil(sent.first?["draft"])
    }

    func testUnqueueingWithNothingHeldSendsNothing() {
        let bundle = bundle { _ in XCTFail("nothing was held and something left the phone") }
        _ = working(bundle)
        XCTAssertFalse(bundle.unqueue(agent))
    }

    // MARK: - Writing another one

    /// One message is queued, not a queue. A second hold is refused by the
    /// core outright, so writing another while one waits replaces it.
    func testWritingASecondMessageReplacesTheOneAlreadyHeld() {
        var sent: [JSONValue] = []
        let bundle = bundle { sent.append($0) }
        let store = working(bundle, queue: held("the first thought"))
        store.draft.body = "the second thought"
        XCTAssertTrue(bundle.send(to: agent))
        XCTAssertEqual(sent.first?["command"]?.stringValue, "queue")
        XCTAssertEqual(sent.first?["action"]?.stringValue, "replace")
        XCTAssertEqual(
            sent.first?["draft"]?["segments"]?.arrayValue?.first?["text"]?.stringValue,
            "the second thought")
        // Held, not sent: nothing optimistic belongs in the transcript for a
        // message that is not in it yet.
        XCTAssertTrue(store.unacknowledged.isEmpty)
        XCTAssertTrue(store.draft.isEmpty)
    }

    /// The first one is a hold. Which of the two it is comes off what the host
    /// says it is holding, not off anything the screen believes.
    func testTheFirstMessageOfATurnIsHeldRatherThanReplacing() {
        var sent: [JSONValue] = []
        let bundle = bundle { sent.append($0) }
        let store = working(bundle)
        store.draft.body = "the only thought"
        XCTAssertTrue(bundle.send(to: agent))
        XCTAssertEqual(sent.first?["action"]?.stringValue, "hold")
    }

    // MARK: - What the strip is told

    /// Nothing true means no strip. A quiet conversation must not pay a band
    /// along its bottom edge to say nothing.
    func testAConversationWithNothingToReportHasNoFacts() {
        let bundle = StoreBundle(account: AccountId("test"))
        XCTAssertTrue(ConversationFacts(working(bundle)).isEmpty)
    }

    /// The children alone are not a strip. They are named in full in the
    /// chrome, one chip each, and a strip that existed only to repeat their
    /// number would be a second answer to a question already answered.
    func testStartedAgentsAloneAreNotEnoughToDrawAStrip() {
        let bundle = StoreBundle(account: AccountId("test"))
        let store = working(
            bundle, family: [FamilyMember(agent: AgentId(UUID()), depth: 1, needs: nil)])
        let facts = ConversationFacts(store)
        XCTAssertTrue(facts.isEmpty)
        XCTAssertEqual(facts.children?.count, 1)
    }

    /// The count is the provider's own two numbers, and the current task its
    /// own sentence. Neither is recomputed from the items.
    func testTheCountAndCurrentTaskAreTheProvidersOwn() {
        let bundle = StoreBundle(account: AccountId("test"))
        let tasks = TaskList(
            done: 3, total: 7, current: "Update the spec tests",
            items: [TaskItem(text: "Update the spec tests", state: .inProgress)])
        let facts = ConversationFacts(
            working(bundle, provider: ProviderFacts(todos: tasks)))
        XCTAssertFalse(facts.isEmpty)
        XCTAssertEqual(facts.progress, "3/7")
        XCTAssertEqual(facts.current, "Update the spec tests")
        XCTAssertTrue(facts.opens)
    }

    /// A list the provider reported with no items behind it has nothing to
    /// grow into, so there is no way in and no chevron drawn.
    func testAListWithNoItemsDoesNotOfferToOpen() {
        let bundle = StoreBundle(account: AccountId("test"))
        let facts = ConversationFacts(working(
            bundle,
            provider: ProviderFacts(todos: TaskList(
                done: 0, total: 2, current: nil, items: []))))
        XCTAssertFalse(facts.isEmpty)
        XCTAssertEqual(facts.progress, "0/2")
        XCTAssertNil(facts.current)
        XCTAssertFalse(facts.opens)
    }

    /// One child that cannot continue is the fact the strip colours for, and
    /// it is read off the same roster the chrome draws its chips from.
    func testAChildThatCannotContinueIsCarriedByTheCount() {
        let bundle = StoreBundle(account: AccountId("test"))
        let facts = ConversationFacts(working(
            bundle,
            provider: ProviderFacts(todos: TaskList(
                done: 1, total: 2, current: "still going", items: [])),
            family: [
                FamilyMember(agent: AgentId(UUID()), depth: 1, needs: nil),
                FamilyMember(agent: AgentId(UUID()), depth: 1, needs: .permission),
            ]))
        XCTAssertEqual(facts.children?.count, 2)
        XCTAssertEqual(facts.children?.needs, .permission)
    }

    /// A message waiting is a fact on its own: a provider that keeps no list
    /// still has a strip while one is held.
    func testAHeldMessageIsEnoughOnItsOwn() {
        let bundle = StoreBundle(account: AccountId("test"))
        let facts = ConversationFacts(working(bundle, queue: held("later, then")))
        XCTAssertFalse(facts.isEmpty)
        XCTAssertNil(facts.progress)
        XCTAssertEqual(facts.queued?.text, "later, then")
    }
}
