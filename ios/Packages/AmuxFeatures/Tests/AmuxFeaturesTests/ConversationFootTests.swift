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
        hostReachable: Bool = true, age: String? = "14m",
        ended: ConversationSubject.Ended? = nil
    ) -> ConversationSubject {
        ConversationSubject(
            name: "refactor-auth", host: "Studio", directory: "~/src/amux",
            hostReachable: hostReachable, age: age, ended: ended)
    }

    private func refusal(_ message: String) -> [OpResult] {
        let json = Data("""
            {"error":"general","message":"\(message)",\
            "auth_required":false,"subscription_required":false}
            """.utf8)
        let failure = try! AmuxJSON.decoder.decode(OpFailure.self, from: json)
        return [OpResult(op: OpId(UUID()), outcome: .failed(failure))]
    }

    /// The ordinary case, which is most conversations: the layer is taking
    /// messages and nothing is written along the bottom of the screen.
    func testAConversationThatTakesMessagesSaysNothing() {
        XCTAssertNil(ConversationFootState(
            gate: .claudePty(.ready), results: [], subject: subject()))
    }

    /// A gate that is waiting on the reader or working belongs to the ask
    /// panel and the composer. Reporting it here would put two answers to the
    /// same question on one screen.
    func testTheStatesOtherSurfacesOwnAreNotReportedHere() {
        for gate in [ClaudePtySendGate.needsYou, .working, .unavailable, .exited] {
            XCTAssertNil(
                ConversationFootState(gate: .claudePty(gate), results: [], subject: subject()),
                "\(gate) should be left to the surface that owns it")
        }
    }

    /// Replaying and a send already in flight both refuse, in both layers'
    /// vocabularies. Nothing about the two providers is flattened into one.
    func testReplayingAndASendInFlightRefuse() {
        let gates: [SendGate] = [
            .claudePty(.replaying), .claudePty(.sendInFlight),
            .codex(.replaying), .codex(.inputInFlight),
        ]
        for gate in gates {
            guard case .refused(let headline, let reason)? =
                ConversationFootState(gate: gate, results: [], subject: subject())
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
            results: refusal("the session is replaying history"),
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
        let result = refusal("that layer is replaying history")[0]
        theirs.dispatched(result.op)

        bundle.apply([.opResult(result)])

        guard case .refused(let headline, let reason)? = ConversationFootState(
            gate: .claudePty(.replaying), results: mine.results, subject: subject())
        else { return XCTFail("a replaying gate should refuse") }
        XCTAssertEqual(headline, "Cannot send")
        XCTAssertEqual(reason, "This session is replaying what it missed.")

        guard case .refused(_, let theirReason)? = ConversationFootState(
            gate: .claudePty(.replaying), results: theirs.results, subject: subject())
        else { return XCTFail("the agent that was refused should say so") }
        XCTAssertEqual(theirReason, "that layer is replaying history")
    }

    /// The host went away mid-turn. The panel names it, says what is happening
    /// and how old the screen above it is.
    func testALostHostIsNamedWithHowOldTheScreenIs() {
        guard case .unreachable(let host, let since)? = ConversationFootState(
            gate: .claudePty(.unknown), results: [], subject: subject(hostReachable: false))
        else { return XCTFail("a lost host should be reported") }

        XCTAssertEqual(host, "Studio")
        XCTAssertEqual(since, "14m")
    }

    /// An agent that stopped for good offers nothing. What happened is in the
    /// feed, where the last thing that happened belongs, and restarting is
    /// starting a new agent rather than a button here.
    func testAnEndedRunOffersNothing() {
        XCTAssertNil(ConversationFootState(
            gate: .claudePty(.exited), results: refusal("the session has exited"),
            subject: subject(ended: ConversationSubject.Ended(code: 1))))
    }

    /// The chrome says where a conversation lives, and when the machine
    /// holding it cannot be reached it says that instead of the directory.
    func testAnUnreachableMachineIsSaidWhereThePlaceGoes() {
        XCTAssertEqual(subject().place, "Studio · ~/src/amux")
        XCTAssertEqual(subject(hostReachable: false).place, "Studio · unreachable")
    }

    /// A conversation opened on the frame the tap happened has heard from
    /// nobody. It is not marked stale on the strength of that: not having been
    /// told is not the same as having been told the machine is gone.
    func testAConversationOpenedBeforeTheFleetIsNotCalledStale() {
        let subject = ConversationSubject(agent: agent, in: FleetStore(now: now))

        XCTAssertTrue(subject.hostReachable)
        XCTAssertNil(subject.ended)
        XCTAssertNil(ConversationFootState(gate: .unavailable, results: [], subject: subject))
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
}
