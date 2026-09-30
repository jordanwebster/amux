import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a fleet row says about itself.
///
/// Three facts decide it — whether this build can open the agent's kind,
/// whether the owning machine is reachable, and the agent's attention — and
/// the order they are read in is the whole of the design. These pin that
/// order, because an offline machine that stopped outranking a request would
/// hide the machine behind a request nobody can answer.
@MainActor
final class RowStateTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)

    private func row(
        _ attention: Attention, kind: Kind = .claudeSdk, presence: Presence = .online,
        exitCause: String? = nil
    ) -> AgentRow {
        AgentRow(
            row: FleetRow(
                card: FleetCard(
                    agent: AgentKey(host: machine, agent: UUID()), name: "refactor-auth",
                    kind: kind, attention: attention, cwd: "~/src/amux", lastActivityMs: 0,
                    host: "Studio.local", hostPresence: presence, children: 0,
                    familyAttention: attention, members: 1,
                    membersNeedYou: attention == .needsYou ? 1 : 0, exitCause: exitCause,
                    workingOn: nil),
                depth: 0, expanded: false),
            unread: false)
    }

    private func host(_ presence: Presence, via: HostVia = .direct) -> HostView {
        HostView(
            hostId: machine.bytes, name: "Studio.local", local: false, trusted: true,
            candidate: false, presence: presence, away: .plain, addrs: [], via: via, current: true,
            lastDialError: nil, platform: nil, signedIn: nil, version: nil)
    }

    func testEachAttentionSpeaksInWords() {
        XCTAssertEqual(RowState(row: row(.working)), .working)
        XCTAssertEqual(RowState(row: row(.working)).word, "Working")
        XCTAssertEqual(RowState(row: row(.starting)).word, "Starting")
        XCTAssertEqual(RowState(row: row(.idle)).word, "Idle")
        XCTAssertEqual(RowState(row: row(.exited, exitCause: "finished")).word, "Finished")
        XCTAssertNil(RowState(row: row(.exited, exitCause: "finished")).elaboration)
        XCTAssertEqual(RowState(row: row(.exited, exitCause: "code 1")).word, "Exited")
        XCTAssertEqual(RowState(row: row(.exited, exitCause: "code 1")).elaboration, "code 1")
    }

    func testNeedingYouSaysSoAndIsTheOnlyMark() {
        let state = RowState(row: row(.needsYou))
        XCTAssertEqual(state, .needsYou)
        XCTAssertEqual(state.word, "Needs you")
        XCTAssertTrue(state.needsYou)
        XCTAssertEqual(state.spoken, "Needs you")
    }

    func testAKindThisBuildCannotOpenOutranksEverything() {
        let state = RowState(row: row(.needsYou, kind: .unspecified, presence: .offline))
        XCTAssertEqual(state, .unsupported)
        XCTAssertEqual(state.elaboration, "update amux to open it")
    }

    func testAnOfflineMachineOutranksARequest() {
        let state = RowState(row: row(.needsYou, presence: .offline))
        XCTAssertEqual(state, .hostOffline("Studio"))
        XCTAssertEqual(state.word, "Studio offline")
        XCTAssertTrue(state.namesTheHost)
        XCTAssertEqual(
            RowState(row: row(.working), host: host(.offline, via: .unspecified)),
            .hostOffline("Studio"))
    }

    func testAnAwayMachineIsNotLive() {
        let state = RowState(row: row(.working, presence: .away))
        XCTAssertEqual(state, .hostAway("Studio"))
        XCTAssertEqual(state.elaboration, "not live")
        XCTAssertEqual(state.spoken, "Studio is away, not live")
    }

    func testAMachineReachedAnyWayLetsTheAgentSpeak() {
        XCTAssertEqual(RowState(row: row(.working), host: host(.online)), .working)
        XCTAssertEqual(
            RowState(row: row(.working), host: host(.online, via: .relay)), .working)
    }

    func testEveryStateHasADistinctName() {
        let names = [
            RowState.needsYou, .working, .starting, .hostOffline("a"), .hostAway("a"),
            .unsupported, .exited(nil), .idle,
        ].map(\.name)
        XCTAssertEqual(Set(names).count, names.count)
    }
}
