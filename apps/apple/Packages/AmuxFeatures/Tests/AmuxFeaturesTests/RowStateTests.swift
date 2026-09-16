import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a fleet row says about itself.
///
/// Four independent facts decide it — the agent's attention, whether this
/// build can read its provider, whether the owning machine has answered, and
/// whether that machine is online at all — and the order they are read in is
/// the whole of the design. These pin that order, because the order is the
/// part a later change can break silently: an offline machine that stopped
/// outranking a permission request would hide the request rather than the
/// machine, and every row involved would still look perfectly reasonable.
///
/// The words are pinned here too rather than left to a capture. A picture
/// exercises one path; this exercises all seven.
@MainActor
final class RowStateTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)
    private let identity = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000B1")!)

    private func host(online: Bool, named name: String = "Studio") -> HostEntry {
        HostEntry(id: machine, name: name, online: online)
    }

    private func row(
        attention: Attention,
        kind: AgentKind = .claude(driver: .pty),
        outcome: TurnOutcome? = nil,
        awaiting: Bool = false
    ) -> AgentRow {
        AgentRow(
            card: AgentCard(
                agent: Agent(
                    id: identity, hostId: machine, name: "refactor-auth",
                    command: "claude", workingDir: "~/src/amux", kind: kind,
                    createdAt: now),
                displayName: "refactor-auth", attention: attention, phase: .running,
                lastActivity: now, outcome: outcome, awaiting: awaiting),
            unread: false)
    }

    private func state(
        _ attention: Attention,
        kind: AgentKind = .claude(driver: .pty),
        outcome: TurnOutcome? = nil,
        awaiting: Bool = false,
        online: Bool = true
    ) -> RowState {
        RowState(
            row: row(attention: attention, kind: kind, outcome: outcome, awaiting: awaiting),
            host: host(online: online))
    }

    // MARK: - The states that speak

    func testAWorkingAgentSaysSoInWordsRatherThanDrawingAMark() {
        let state = state(.working)
        XCTAssertEqual(state, .working)
        XCTAssertEqual(state.word, "Working")
        XCTAssertNil(state.elaboration)
        XCTAssertFalse(state.needsYou, "working is not drawn in the accent")
    }

    func testAnIdleAgentKeepsItsWord() {
        XCTAssertEqual(state(.idle).word, "Idle")
    }

    /// A finished turn says what it changed, because the numbers are the
    /// readable part. An absent count is not a zero and is never drawn as one.
    func testAFinishedTurnLeadsWithItsArithmeticWhenTheProviderCountedIt() {
        let counted = state(
            .needsYou(why: .finished), outcome: TurnOutcome(files: 4, insertions: 118, deletions: 40))
        XCTAssertEqual(counted.word, "Finished")
        XCTAssertEqual(counted.elaboration, "4 files · +118 \u{2212}40")

        let uncounted = state(.needsYou(why: .finished))
        XCTAssertEqual(uncounted.word, "Finished")
        XCTAssertNil(uncounted.elaboration, "a count nobody made is not a zero")
    }

    /// The machine, and the thing that can be done about it. The row does not
    /// then repeat the machine on its trailing edge.
    func testAnOfflineMachineIsNamedInWordsAndOnlyOnce() {
        let state = state(.working, online: false)
        XCTAssertEqual(state, .hostOffline("Studio"))
        XCTAssertEqual(state.word, "Studio offline")
        XCTAssertTrue(state.namesTheHost)
        XCTAssertEqual(state.spoken, "Studio is offline")
    }

    /// What it is and what to do, rather than this app's own limitation. The
    /// provider keeps the name its host used for it.
    func testAnUnreadableProviderIsNamedAndTheReaderIsToldWhatToDo() {
        let state = state(.working, kind: .unknown("gemini"))
        XCTAssertEqual(state, .unsupported("gemini"))
        XCTAssertEqual(state.word, "gemini")
        XCTAssertEqual(state.elaboration, "update amux to open it")
        XCTAssertEqual(state.spoken, "gemini, update amux to open it")
    }

    /// The one accent left in the vocabulary, for the one state that wants you.
    func testOnlyADemandIsDrawnInTheAccent() {
        for why in [Why.permission, .question] {
            XCTAssertTrue(state(.needsYou(why: why)).needsYou)
            XCTAssertNil(state(.needsYou(why: why)).word, "the row says what is wanted instead")
        }
        for quiet in [Attention.idle, .working, .unknown] {
            XCTAssertFalse(state(quiet).needsYou, "\(quiet) draws nothing")
        }
        XCTAssertFalse(
            state(.needsYou(why: .finished)).needsYou,
            "a finished turn says so in words, not in the accent")
    }

    // MARK: - The state that deliberately says nothing

    /// An expired working inference. All the core knows is that it no longer
    /// trusts its own guess: the agent could be part-way through a long build
    /// and not writing rows, could have finished with the delivery lost, could
    /// be wedged, could be fine behind a wedged stream. Any word would be a
    /// diagnosis this app cannot support, and the age already on the row is
    /// the honest fact.
    func testAnExpiredWorkingInferenceSaysNothingAtAll() {
        let state = state(.unknown)
        XCTAssertEqual(state, .unheard)
        XCTAssertNil(state.word)
        XCTAssertNil(state.elaboration)
        XCTAssertNil(state.spoken)
        XCTAssertFalse(state.needsYou)
    }

    // MARK: - The order they are read in

    /// A provider this build cannot read outranks everything, including a
    /// demand: none of what the agent wants can be acted on from a
    /// conversation that will not open.
    func testBeingUnreadableOutranksADemandAndADarkMachine() {
        XCTAssertEqual(
            state(.needsYou(why: .permission), kind: .unknown("gemini"), online: false),
            .unsupported("gemini"))
    }

    /// The core has already made this call inside `effective_attention`, which
    /// degrades an offline host's agents to `unknown` whatever they last
    /// wanted. Reading the host directly is what also catches a remembered
    /// row, which keeps whatever attention it was cached with — so a row this
    /// phone only remembers must not raise a demand its machine cannot
    /// confirm.
    func testADarkMachineOutranksWhatTheRowRemembersWanting() {
        XCTAssertEqual(
            state(.needsYou(why: .permission), awaiting: true, online: false),
            .hostOffline("Studio"))
    }

    /// A machine that has answered lets the demand through.
    func testAMachineThatIsAnsweringLetsADemandThrough() {
        XCTAssertEqual(
            state(.needsYou(why: .permission), online: true), .needsYou(.permission))
    }

    /// Nothing has said the machine is offline, so nothing claims it is. A
    /// conversation opened before the fleet has arrived has no host to read,
    /// and marking it dark on the strength of not having heard yet would be
    /// the same lie in the other direction.
    func testAnUnknownMachineIsNotCalledOffline() {
        let unheardOf = RowState(row: row(attention: .working), host: nil)
        XCTAssertEqual(unheardOf, .working)
    }

    // MARK: - A machine the relay can see and will not carry to

    /// An agent on an away machine stays on the list and says it is not being
    /// watched. Deleting the row would claim the agent had stopped existing
    /// because somebody has not paid, and leaving it saying "Working" would be
    /// the phone asserting something no stream is telling it.
    func testAnAwayMachineLeavesTheRowListedAndNotLive() {
        let away = RowState(
            row: row(attention: .working), host: host(online: true), reach: .away)

        XCTAssertEqual(away, .hostAway("Studio"))
        XCTAssertEqual(away.word, "Studio away")
        XCTAssertEqual(away.elaboration, "not live")
        XCTAssertEqual(away.spoken, "Studio is away, not live")
        XCTAssertEqual(away.name, "host-away")
    }

    /// A machine that is not there outranks one the relay can see: the row
    /// says the machine is offline, and nothing anywhere near it offers to
    /// sell a route to a machine that is switched off.
    func testAMachineThatIsNotThereIsNeverCalledAway() {
        XCTAssertEqual(
            RowState(row: row(attention: .working), host: host(online: false), reach: .offline),
            .hostOffline("Studio"))
    }

    /// A machine on this network or across a relay this account may use is
    /// live, and the row says whatever the agent is doing.
    func testAReachableMachineLetsTheAgentSpeakForItself() {
        XCTAssertEqual(
            RowState(row: row(attention: .working), host: host(online: true),
                     reach: .onThisNetwork),
            .working)
        XCTAssertEqual(
            RowState(row: row(attention: .working), host: host(online: true),
                     reach: .throughTheRelay),
            .working)
    }

    // MARK: - The vocabulary a driver reads

    /// Kept apart from what is drawn, so a screen's wording can change without
    /// rewriting the journeys that drive it. This commit changed the words and
    /// left these alone.
    func testTheDoorsWordsAreNotTheScreensWords() {
        XCTAssertEqual(state(.working).name, "working")
        XCTAssertEqual(state(.working, online: false).name, "host-offline")
        XCTAssertEqual(state(.unknown).name, "unheard")
        XCTAssertEqual(state(.working, kind: .unknown("gemini")).name, "unsupported")
        XCTAssertEqual(state(.idle).name, "idle")
        XCTAssertEqual(state(.needsYou(why: .finished)).name, "finished")
    }
}
