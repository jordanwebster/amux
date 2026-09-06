import AmuxCore
import XCTest

@testable import Instrumentation

/// A workload is only a workload if it is the same everywhere. These tests are
/// about that, not about the numbers measured over it.
final class WorkloadTests: XCTestCase {
    func testTheFleetIsThePinnedComposition() {
        let fleet = Workloads.cachedFleet()
        XCTAssertEqual(fleet.agents.count, 40)
        XCTAssertEqual(fleet.hosts.count, 3)
        XCTAssertEqual(fleet.agents.filter { $0.attention.why == .permission }.count, 3)
        XCTAssertEqual(fleet.agents.filter { $0.attention.why == .question }.count, 3)
        XCTAssertEqual(fleet.agents.filter { $0.attention.why == .finished }.count, 4)
        XCTAssertEqual(fleet.agents.filter { $0.attention == .unknown }.count, 3)
        XCTAssertEqual(
            fleet.agents.filter { $0.lastActivity <= Workloads.now.addingTimeInterval(-86_400) }.count,
            5)
        XCTAssertFalse(fleet.reconciled, "a cached fleet has not been confirmed by a host")
        XCTAssertEqual(Set(fleet.agents.map(\.id)).count, 40, "every agent is its own")
    }

    func testTheSameSeedGivesTheSameFleet() {
        XCTAssertEqual(Workloads.cachedFleet(seed: 1), Workloads.cachedFleet(seed: 1))
        XCTAssertNotEqual(Workloads.cachedFleet(seed: 1), Workloads.cachedFleet(seed: 2))
    }

    func testTheFleetIsNotDealtInStateOrder() {
        // A list already sorted by state would make the home screen's ordering
        // work look free.
        let attention = Workloads.cachedFleet().agents.map(\.attention)
        let waiting = attention.enumerated().filter { $0.element.why != nil }.map(\.offset)
        XCTAssertGreaterThan(try! XCTUnwrap(waiting.last), 9, "the agents needing you are all first")
    }

    func testTheConversationIsThePinnedMixture() {
        let rows = Workloads.conversation(agent: AgentId(UUID()))
        XCTAssertEqual(rows.count, 1_000)
        XCTAssertEqual(rows.filter { $0.entryKind == "message" }.count, 550)
        XCTAssertEqual(rows.filter { $0.entryKind == "tool" }.count, 400)
        XCTAssertEqual(rows.filter { $0.entryKind == "rule" }.count, 50)
        let folded = rows.filter { $0.row["grouped"]?.boolValue == true }
        XCTAssertEqual(folded.count, 100)
        let long = rows.filter {
            ($0.row["outcome"]?["facts"]?["head"]?.stringValue ?? "").split(separator: "\n").count > 200
        }
        XCTAssertEqual(long.count, 50)
        XCTAssertEqual(Set(rows.map(\.rowId)).count, 1_000, "every row has its own identity")
    }

    func testTheSameSeedGivesTheSameConversation() {
        let agent = AgentId(UUID())
        XCTAssertEqual(
            Workloads.conversation(agent: agent, rows: 50, seed: 1),
            Workloads.conversation(agent: agent, rows: 50, seed: 1))
        XCTAssertNotEqual(
            Workloads.conversation(agent: agent, rows: 50, seed: 1),
            Workloads.conversation(agent: agent, rows: 50, seed: 2))
    }

    func testTheStreamArrivesFiftyRowsASecondForTwentySeconds() {
        let batches = Workloads.stream(agent: AgentId(UUID()))
        XCTAssertEqual(batches.count, 20)
        XCTAssertTrue(batches.allSatisfy { $0.count == 50 })
    }

    func testTheStreamsRowsContinueTheTranscriptTheyLandOn() {
        // A feed never hands out an identity twice, and a list whose rows
        // share identities diffs undefined: the arriving rows would land in
        // arbitrary places and the streaming numbers would be about nothing.
        let agent = AgentId(UUID())
        let transcript = Workloads.conversation(agent: agent)
        let arriving = Workloads.stream(agent: agent).flatMap { $0 }
        XCTAssertEqual(
            Set(transcript.map(\.rowId)).intersection(arriving.map(\.rowId)), [],
            "an arriving row reuses an identity the transcript already holds")
        XCTAssertEqual(
            Set((transcript + arriving).map(\.rowId)).count, 2_000,
            "every row of the streamed transcript is its own")
    }

    func testTheLatencyWorkloadsCarryTheirDelay() {
        XCTAssertEqual(Workload.latency0.latencyMilliseconds, 0)
        XCTAssertEqual(Workload.latency100.latencyMilliseconds, 100)
        XCTAssertNil(Workload.conversation1000.latencyMilliseconds)
    }

    func testAWorkloadSurvivesTheBridgesOwnJson() throws {
        // The suite delivers workloads as the runtime's own JSON, so a
        // workload that cannot be encoded and decoded is not a workload.
        let events = [
            Event.fleet(Workloads.cachedFleet()),
            Workloads.append(
                Workloads.conversation(agent: AgentId(UUID()), rows: 20),
                to: AgentId(UUID()), at: 0),
        ]
        let json = try AmuxJSON.encoder.encode(events)
        XCTAssertEqual(try AmuxJSON.decoder.decode([Event].self, from: json), events)
    }
}
