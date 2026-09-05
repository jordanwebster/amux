import XCTest

@testable import Instrumentation

/// What a verdict must say when a machine is fast, slow, unknown or new.
final class JudgeTests: XCTestCase {
    private let machines = [
        MachineRow(name: "pinned-mac", model: "Mac14,6", budgetsAreHard: true, baselineRequired: false),
        MachineRow(name: "macos-26", model: nil, budgetsAreHard: false, baselineRequired: true),
    ]
    private let budgets: [Metric: Budget] = [
        .coldFirstFrameMs: Budget(unit: .milliseconds, median: 400, worst: 600, tolerance: 0.15),
        .footprintMB: Budget(unit: .megabytes, median: 250, worst: nil, tolerance: 0.10),
    ]

    private func samples(_ metric: Metric, _ values: [Double], unit: MetricUnit = .milliseconds) -> [MetricSample] {
        values.map {
            MetricSample(metric: metric, value: $0, unit: unit, proxy: false, workload: .cachedFleet40)
        }
    }

    func testMeetingTheBudgetPasses() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [310, 322, 330, 341, 358]),
            budgets: table, machine: "pinned-mac", simulator: "amux-golden")
        XCTAssertTrue(verdict.passed)
        let result = try XCTUnwrap(verdict.results.first)
        XCTAssertEqual(result.median, 330)
        XCTAssertEqual(result.worst, 358)
        XCTAssertNil(result.note)
    }

    func testABudgetBreachFails() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [390, 401, 460, 470, 480]),
            budgets: table, machine: "pinned-mac")
        XCTAssertFalse(verdict.passed)
        let result = try XCTUnwrap(verdict.results.first)
        XCTAssertEqual(result.median, 460)
        XCTAssertEqual(result.budget, 400)
        XCTAssertTrue(try XCTUnwrap(result.note).contains("over the budget"))
    }

    func testTheWorstSampleHasItsOwnBudget() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [300, 310, 320, 330, 900]),
            budgets: table, machine: "pinned-mac")
        XCTAssertFalse(verdict.passed)
        XCTAssertTrue(try XCTUnwrap(verdict.results.first?.note).contains("worst-case"))
    }

    func testARegressionPastTheToleranceFails() throws {
        // The budget is met, so only the recorded baseline can catch this:
        // 15% over 300 ms is 345 ms, and the median is 360 ms.
        let table = BudgetTable(machines: machines, budgets: budgets)
            .with(baselines: [.coldFirstFrameMs: 300])
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [350, 355, 360, 365, 370]),
            budgets: table, machine: "macos-26")
        XCTAssertFalse(verdict.passed)
        let result = try XCTUnwrap(verdict.results.first)
        XCTAssertEqual(result.baseline, 300)
        XCTAssertTrue(try XCTUnwrap(result.note).contains("15% over"))
    }

    func testDriftInsideTheTolerancePasses() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
            .with(baselines: [.coldFirstFrameMs: 300])
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [320, 330, 340, 342, 344]),
            budgets: table, machine: "macos-26")
        XCTAssertTrue(verdict.passed)
    }

    func testFootprintHasItsOwnTighterTolerance() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
            .with(baselines: [.footprintMB: 200])
        let verdict = try judge(
            samples: samples(.footprintMB, [215, 218, 221, 224, 226], unit: .megabytes),
            budgets: table, machine: "macos-26")
        XCTAssertFalse(verdict.passed, "10% over 200 MB is 220 MB and the median is 221 MB")
    }

    func testAMachineThatMustHaveABaselineAndHasNoneIsAnError() {
        let table = BudgetTable(machines: machines, budgets: budgets)
        XCTAssertThrowsError(
            try judge(
                samples: samples(.coldFirstFrameMs, [300, 310, 320, 330, 340]),
                budgets: table, machine: "macos-26")
        ) { error in
            XCTAssertEqual(error as? PerfError, .missingBaseline(.coldFirstFrameMs))
        }
    }

    func testAnUnknownMachineIsRefused() {
        let table = BudgetTable(machines: machines, budgets: budgets)
        XCTAssertThrowsError(
            try judge(
                samples: samples(.coldFirstFrameMs, [300, 310, 320, 330, 340]),
                budgets: table, machine: "someones-laptop")
        ) { error in
            XCTAssertEqual(error as? PerfError, .unknownMachine("someones-laptop"))
        }
    }

    func testFewerThanFiveSamplesIsAnError() {
        let table = BudgetTable(machines: machines, budgets: budgets)
        XCTAssertThrowsError(
            try judge(
                samples: samples(.coldFirstFrameMs, [300, 310, 320]),
                budgets: table, machine: "pinned-mac")
        ) { error in
            XCTAssertEqual(error as? PerfError, .tooFewSamples(.coldFirstFrameMs, 3))
        }
    }

    func testAProxySampleMakesTheResultAProxy() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
        let taken = samples(.coldFirstFrameMs, [300, 310, 320, 330, 340]).enumerated().map {
            MetricSample(
                metric: $1.metric, value: $1.value, unit: $1.unit, proxy: $0 == 0,
                workload: $1.workload)
        }
        let verdict = try judge(samples: taken, budgets: table, machine: "pinned-mac")
        XCTAssertTrue(try XCTUnwrap(verdict.results.first).proxy)
    }
}
