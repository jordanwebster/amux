import AmuxCore
import XCTest

@testable import Instrumentation

/// A workload is only a workload if it is the same everywhere. These tests are
/// about that, not about the numbers measured over it.
final class WorkloadTests: XCTestCase {
    func testTheFleetIsThePinnedComposition() {
        let fleet = Workloads.cachedFleet()
        XCTAssertEqual(fleet.count, 40)
        XCTAssertEqual(fleet.filter { $0.card.attention == .needsYou }.count, 6)
        XCTAssertEqual(fleet.filter { $0.card.attention == .exited }.count, 4)
        let dayAgo = Int64(Workloads.now.addingTimeInterval(-86_400).timeIntervalSince1970 * 1000)
        XCTAssertEqual(fleet.filter { $0.card.lastActivityMs <= dayAgo }.count, 5)
        XCTAssertEqual(Set(fleet.map(\.card.host)), ["studio", "mini", "air"])
    }

    func testTheSameSeedGivesTheSameFleet() {
        XCTAssertEqual(Workloads.cachedFleet(seed: 1), Workloads.cachedFleet(seed: 1))
        XCTAssertNotEqual(Workloads.cachedFleet(seed: 1), Workloads.cachedFleet(seed: 2))
    }

    func testTheFleetIsNotDealtInStateOrder() {
        let attention = Workloads.cachedFleet().map(\.card.attention)
        XCTAssertNotEqual(attention, attention.sorted { $0.rawValue < $1.rawValue })
    }

    func testTheLatencyWorkloadsCarryTheirDelay() {
        XCTAssertEqual(Workload.latency0.latencyMilliseconds, 0)
        XCTAssertEqual(Workload.latency100.latencyMilliseconds, 100)
        XCTAssertNil(Workload.cachedFleet40.latencyMilliseconds)
    }

    func testAWorkloadSurvivesTheBridgesOwnJson() throws {
        let fleet = Workloads.cachedFleet()
        let decoded = try JSONDecoder().decode(
            [FleetRow].self, from: try JSONEncoder().encode(fleet))
        XCTAssertEqual(decoded, fleet)
    }
}
