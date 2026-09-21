import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a conversation says where the composer will go, and why.
///
/// The screen never decides whether a message may be sent. It reads the core's
/// typed gate and the core's own refusal and says what it was told, so these
/// pin the reading rather than the drawing: which states speak at all, whose
/// words are on screen, and the one state that deliberately offers nothing.
@MainActor
final class ConversationFootTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let host = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)
    private let agent = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000B1")!)

    private func subject(
        hostReachable: Bool = true, hostAway: Bool = false, age: String? = "14m",
        ended: ConversationSubject.Ended? = nil
    ) -> ConversationSubject {
        ConversationSubject(
            name: "refactor-auth", host: "Studio", directory: "~/src/amux",
            hostReachable: hostReachable, hostAway: hostAway, age: age, ended: ended)
    }

    private func failure(_ message: String) -> OpFailure {
        let json = Data("""
            {"error":"general","message":"\(message)",\
            "auth_required":false,"subscription_required":false}
            """.utf8)
        return try! AmuxJSON.decoder.decode(OpFailure.self, from: json)
    }

    private func refusal(_ message: String, op: OpId = OpId(UUID())) -> OpResult {
        OpResult(op: op, outcome: .failed(failure(message)))
    }

    /// The ordinary case, which is most conversations: the layer is taking
    /// messages and nothing is written along the bottom of the screen.
    func testAConversationThatTakesMessagesSaysNothing() {
        XCTAssertNil(ConversationFootState(
            gate: .claudePty(.ready), refusal: nil, subject: subject()))
    }

    /// A gate that is waiting on the reader or working belongs to the ask
    /// panel and the composer. Reporting it here would put two answers to the
    /// same question on one screen.
    func testTheStatesOtherSurfacesOwnAreNotReportedHere() {
        for gate in [ClaudePtySendGate.needsYou, .working, .unavailable, .exited] {
            XCTAssertNil(
                ConversationFootState(gate: .claudePty(gate), refusal: nil, subject: subject()),
                "\(gate) should be left to the surface that owns it")
        }
    }

    /// Replaying refuses, in both layers' vocabularies. Nothing about the two
    /// providers is flattened into one.
    func testReplayingRefuses() {
        let gates: [SendGate] = [.claudePty(.replaying), .codex(.replaying)]
        for gate in gates {
            guard case .refused(let headline, let reason)? =
                ConversationFootState(gate: gate, refusal: nil, subject: subject())
            else {
                XCTFail("\(gate) should refuse")
                continue
            }
            XCTAssertEqual(headline, "Cannot send")
            XCTAssertFalse(reason.isEmpty)
        }
    }

    /// When the core has refused an actual send, its sentence is what is on
    /// screen. A refusal rewritten on the phone would be a second opinion
    /// about something only the host knows.
    func testARefusedSendShowsTheCoresOwnSentence() {
        guard case .refused(let headline, let reason)? = ConversationFootState(
            gate: .claudePty(.replaying),
            refusal: failure("the session is replaying history"),
            subject: subject())
        else { return XCTFail("a refused send should be reported") }

        XCTAssertEqual(headline, "Not sent")
        XCTAssertEqual(reason, "the session is replaying history")
    }

    /// Two conversations are open and a send fails on one of them. The other
    /// agent's foot says what its own gate says: a host that never spoke about
    /// this agent must not be quoted under its name.
    func testAFailureOnAnotherAgentIsNotThisAgentsRefusal() {
        let bundle = StoreBundle(account: AccountId("test"))
        let mine = bundle.conversation(agent)
        let other = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000B2")!)
        let theirs = bundle.conversation(other)
        let result = refusal("that layer is replaying history")
        theirs.dispatched(result.op)

        bundle.apply([.opResult(result)])

        guard case .refused(let headline, let reason)? = ConversationFootState(
            gate: .claudePty(.replaying), refusal: mine.refusal, subject: subject())
        else { return XCTFail("a replaying gate should refuse") }
        XCTAssertEqual(headline, "Cannot send")
        XCTAssertEqual(reason, "This session is replaying what it missed.")

        guard case .refused(_, let theirReason)? = ConversationFootState(
            gate: .claudePty(.replaying), refusal: theirs.refusal, subject: subject())
        else { return XCTFail("the agent that was refused should say so") }
        XCTAssertEqual(theirReason, "that layer is replaying history")
    }

    /// A message on its way is the composer's to show, not a refusal. The box
    /// that sent it stays, so the keyboard does too.
    func testASendInFlightIsNotAFoot() {
        for gate in [SendGate.claudePty(.sendInFlight), .claudeSdk(.inputInFlight),
                     .codex(.inputInFlight)] {
            XCTAssertNil(
                ConversationFootState(gate: gate, refusal: nil, subject: subject()),
                "\(gate) replaced the composer that sent the message")
        }
    }

    /// A reader is refused, sends again, and is refused again. The panel is
    /// about the message in flight at each step: the host's first sentence,
    /// then nothing while the second message is genuinely on its way, then the
    /// host's second sentence. It never captions a message in flight with an
    /// earlier message's reason.
    func testTheDrawnRefusalFollowsTheMessageInFlight() {
        let store = ConversationStore(agent: agent)
        let first = OpId(UUID(uuidString: "00000000-0000-0000-0000-0000000000C1")!)
        let second = OpId(UUID(uuidString: "00000000-0000-0000-0000-0000000000C2")!)

        store.dispatched(first)
        store.apply(.opResult(refusal("the session is replaying history", op: first)))
        guard case .refused(let headline, let reason)? = ConversationFootState(
            gate: .claudePty(.replaying), refusal: store.refusal, subject: subject())
        else { return XCTFail("a refused send should be reported") }
        XCTAssertEqual(headline, "Not sent")
        XCTAssertEqual(reason, "the session is replaying history")

        store.dispatched(second)
        XCTAssertNil(
            ConversationFootState(
                gate: .claudePty(.sendInFlight), refusal: store.refusal, subject: subject()),
            "the first refusal captioned the message now on its way")

        store.apply(.opResult(refusal("that layer is still replaying", op: second)))
        guard case .refused(let againHeadline, let againReason)? = ConversationFootState(
            gate: .claudePty(.replaying), refusal: store.refusal, subject: subject())
        else { return XCTFail("the second refusal should be reported") }
        XCTAssertEqual(againHeadline, "Not sent")
        XCTAssertEqual(againReason, "that layer is still replaying")
    }

    /// The host went away mid-turn. The panel names it, says what is happening
    /// and how old the screen above it is.
    func testALostHostIsNamedWithHowOldTheScreenIs() {
        guard case .unreachable(let host, let since)? = ConversationFootState(
            gate: .claudePty(.unknown), refusal: nil, subject: subject(hostReachable: false))
        else { return XCTFail("a lost host should be reported") }

        XCTAssertEqual(host, "Studio")
        XCTAssertEqual(since, "14m")
    }

    /// An agent that stopped for good offers nothing. What happened is in the
    /// feed, where the last thing that happened belongs, and restarting is
    /// starting a new agent rather than a button here.
    func testAnEndedRunOffersNothing() {
        XCTAssertNil(ConversationFootState(
            gate: .claudePty(.exited), refusal: failure("the session has exited"),
            subject: subject(ended: ConversationSubject.Ended(code: 1))))
    }

    /// The chrome says where a conversation lives, and when the machine
    /// holding it cannot be reached it says that instead of the directory.
    func testAnUnreachableMachineIsSaidWhereThePlaceGoes() {
        XCTAssertEqual(subject().place, "~/s/amux · Studio")
        XCTAssertEqual(subject(hostReachable: false).place, "Studio · unreachable")
    }

    /// A conversation opened on the frame the tap happened has heard from
    /// nobody. It is not marked stale on the strength of that: not having been
    /// told is not the same as having been told the machine is gone.
    func testAConversationOpenedBeforeTheFleetIsNotCalledStale() {
        let subject = ConversationSubject(agent: agent, in: FleetStore(now: now))

        XCTAssertTrue(subject.hostReachable)
        XCTAssertNil(subject.ended)
        XCTAssertNil(ConversationFootState(gate: .unavailable, refusal: nil, subject: subject))
    }

    /// The exit code comes off the card the host filled in rather than being
    /// inferred from the stream having ended, and an absent code stays absent.
    func testTheExitCodeComesFromTheFleet() {
        for reported in [1, 0] {
            let subject = ConversationSubject(
                agent: agent, in: fleet(phase: .exited(exitCode: reported)))
            XCTAssertEqual(subject.ended?.code, reported)
        }
        let unsaid = ConversationSubject(agent: agent, in: fleet(phase: .exited(exitCode: nil)))
        XCTAssertNotNil(unsaid.ended)
        XCTAssertNil(unsaid.ended?.code)
        XCTAssertNil(ConversationSubject(agent: agent, in: fleet(phase: .running)).ended)
    }

    /// Leaving the inventory is correct for an exited run, but the open
    /// conversation keeps the name, place and exit code it was already
    /// showing instead of falling back to a UUID and an empty composer.
    func testAnOpenConversationRetainsIdentityAndExitAfterFleetRemoval() {
        let bundle = StoreBundle(account: AccountId("test"), clock: { self.now })
        bundle.apply(.fleet(Fleet(
            epoch: 1,
            agents: [AgentCard(
                agent: Agent(
                    id: agent, hostId: host, name: "refactor-auth", command: "claude",
                    workingDir: "~/src/amux", kind: .claude(driver: .pty),
                    createdAt: now.addingTimeInterval(-3600)),
                displayName: "refactor-auth", attention: .idle, phase: .running,
                lastActivity: now.addingTimeInterval(-840))],
            hosts: [HostState(
                entry: HostEntry(id: host, name: "Studio", online: true), epoch: 1)],
            reconciled: true)))
        let conversation = bundle.conversation(agent)
        bundle.apply(.fleet(Fleet(
            epoch: 2, agents: [],
            hosts: [HostState(
                entry: HostEntry(id: host, name: "Studio", online: true), epoch: 2)],
            reconciled: true)))
        bundle.apply(.session(SessionSnapshot(
            agent: agent, gate: .unavailable, phase: .unavailable, stream: nil,
            asks: [], facts: .unavailable, provider: ProviderFacts(),
            settingsGate: .unavailable, queue: nil, family: [],
            terminalPhase: .exited(exitCode: 7))))

        let subject = ConversationSubject(
            agent: agent, in: bundle.fleet, retaining: conversation)
        XCTAssertEqual(subject.name, "refactor-auth")
        XCTAssertEqual(subject.place, "~/s/amux · Studio")
        XCTAssertEqual(subject.ended?.code, 7)
        XCTAssertTrue(bundle.fleet.rows.isEmpty)
    }

    private func fleet(phase: AgentPhase, online: Bool = true) -> FleetStore {
        let store = FleetStore(now: now)
        store.apply(.fleet(Fleet(
            epoch: 1,
            agents: [AgentCard(
                agent: Agent(
                    id: agent, hostId: host, name: "refactor-auth", command: "claude",
                    workingDir: "~/src/amux", kind: .claude(driver: .pty),
                    createdAt: now.addingTimeInterval(-3600)),
                displayName: "refactor-auth", attention: .idle, phase: phase,
                lastActivity: now.addingTimeInterval(-840))],
            hosts: [HostState(
                entry: HostEntry(id: host, name: "Studio", online: online), epoch: 1)],
            reconciled: true)))
        return store
    }

    /// The machine's own answer, not the relay's: a conversation is stale
    /// because the machine that owns the agent is not answering.
    func testAnOfflineMachineMakesItsConversationStale() {
        let subject = ConversationSubject(agent: agent, in: fleet(phase: .running, online: false))

        XCTAssertFalse(subject.hostReachable)
        XCTAssertEqual(subject.age, "14m")
    }

    // MARK: - A machine the relay can see and will not carry to

    /// The transcript stays readable and the composer's place holds the offer
    /// rather than a fault. There is nothing wrong with this machine and
    /// nothing being waited for, so nothing here reads as an error and nothing
    /// offers to ask again.
    func testAnAwayMachinePutsTheOfferWhereTheComposerWouldBe() {
        let state = ConversationFootState(
            gate: .claudePty(.ready), refusal: nil, subject: subject(hostAway: true))

        XCTAssertEqual(state, .away(host: "Studio"))
        XCTAssertEqual(state?.headline, SubscribeCopy.headline)
        XCTAssertEqual(state?.detail, SubscribeCopy.detail(host: "Studio"))
        XCTAssertTrue(state?.detail.contains("Studio") == true)
    }

    /// A machine that is not there is never sold a route to itself. No
    /// subscription reaches a machine that is switched off, so the screen says
    /// what it says about every unreachable machine and offers to ask again.
    func testAMachineThatIsNotThereIsOfferedNothingToBuy() {
        let state = ConversationFootState(
            gate: .claudePty(.ready), refusal: nil,
            subject: subject(hostReachable: false, hostAway: true))

        XCTAssertEqual(state, .unreachable(host: "Studio", since: "14m"))
        XCTAssertFalse(state?.headline.contains(SubscribeCopy.headline) == true)
    }

    /// A run that has ended offers nothing at all, away or not: it has already
    /// said what happened, and buying a route to it would change nothing.
    func testAnEndedRunOnAnAwayMachineStillOffersNothing() {
        XCTAssertNil(ConversationFootState(
            gate: .claudePty(.ready), refusal: nil,
            subject: subject(hostAway: true, ended: .init(code: 0))))
    }
}
