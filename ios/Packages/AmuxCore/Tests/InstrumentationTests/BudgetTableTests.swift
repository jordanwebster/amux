import XCTest

@testable import Instrumentation

/// The measurement document is the budget table. These tests read the
/// committed document rather than a copy of it, so a budget edited in prose
/// without a number behind it fails here.
final class BudgetTableTests: XCTestCase {
    private func document() throws -> String {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "IOS_PERFORMANCE", withExtension: "md"))
        return try String(contentsOf: url, encoding: .utf8)
    }

    func testTheDocumentNamesThePinnedMacAndTheRunner() throws {
        let table = try BudgetTable.parse(markdown: document())
        let pinned = try XCTUnwrap(table.machine("pinned-mac"))
        XCTAssertEqual(pinned.model, "Mac14,6")
        XCTAssertTrue(pinned.budgetsAreHard)
        XCTAssertFalse(pinned.baselineRequired)
        let runner = try XCTUnwrap(table.machine("macos-26"))
        XCTAssertNil(runner.model)
        XCTAssertFalse(runner.budgetsAreHard)
        XCTAssertTrue(runner.baselineRequired)
        XCTAssertNil(table.machine("someones-laptop"))
    }

    func testAMachineIsFoundByTheModelItReports() throws {
        let table = try BudgetTable.parse(markdown: document())
        XCTAssertEqual(table.machine(model: "Mac14,6")?.name, "pinned-mac")
        XCTAssertNil(table.machine(model: "Mac16,1"))
    }

    func testEveryMetricHasABudget() throws {
        let table = try BudgetTable.parse(markdown: document())
        for metric in Metric.allCases {
            XCTAssertNotNil(table.budget(metric), "\(metric.rawValue) has no budget row")
        }
    }

    func testThePinnedNumbersAreTheOnesInTheDefinitions() throws {
        let table = try BudgetTable.parse(markdown: document())
        let cold = try XCTUnwrap(table.budget(.coldFirstFrameMs))
        XCTAssertEqual(cold.median, 400)
        XCTAssertEqual(cold.worst, 600)
        XCTAssertEqual(cold.tolerance, 0.15, accuracy: 1e-9)
        XCTAssertEqual(try XCTUnwrap(table.budget(.reconciliationMs)).median, 1000)
        XCTAssertEqual(try XCTUnwrap(table.budget(.hitchTimeRatioMsPerS)).median, 5)
        XCTAssertEqual(try XCTUnwrap(table.budget(.mainThreadCpuPercent)).median, 60)
        let footprint = try XCTUnwrap(table.budget(.footprintMB))
        XCTAssertEqual(footprint.median, 250)
        XCTAssertEqual(footprint.tolerance, 0.10, accuracy: 1e-9)
        XCTAssertEqual(try XCTUnwrap(table.budget(.idleCommits)).median, 0)
    }

    func testTheDefinitionsAndTheChecklistAreInTheDocument() throws {
        let text = try document()
        for line in [
            "| Cold first frame | Kernel process start to the first presented frame",
            "| Reconciliation | `streamConnected` to the last row's shimmer ending",
            "| Streaming scroll | Hitch time ratio ≤ 5 ms per second",
            "| Samples and tolerance | 5 samples per metric",
            "## The physical-phone checklist",
        ] {
            XCTAssertTrue(text.contains(line), "the document has lost: \(line)")
        }
        XCTAssertFalse(text.contains("- [x]"), "the physical-phone checklist is ticked")
    }

    func testADocumentWithoutTheTablesIsRefused() {
        XCTAssertThrowsError(try BudgetTable.parse(markdown: "# nothing here")) { error in
            XCTAssertEqual(error as? BudgetTableError, .sectionMissing("Machines"))
        }
    }
}
