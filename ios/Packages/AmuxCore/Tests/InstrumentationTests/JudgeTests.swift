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

    private func samples(
        _ metric: Metric, _ values: [Double], unit: MetricUnit = .milliseconds,
        workload: Workload = .cachedFleet40
    ) -> [MetricSample] {
        values.map {
            MetricSample(metric: metric, value: $0, unit: unit, proxy: false, workload: workload)
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
            .with(baselines: [Measured(.coldFirstFrameMs, .cachedFleet40): 300])
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
            .with(baselines: [Measured(.coldFirstFrameMs, .cachedFleet40): 300])
        let verdict = try judge(
            samples: samples(.coldFirstFrameMs, [320, 330, 340, 342, 344]),
            budgets: table, machine: "macos-26")
        XCTAssertTrue(verdict.passed)
    }

    func testFootprintHasItsOwnTighterTolerance() throws {
        let table = BudgetTable(machines: machines, budgets: budgets)
            .with(baselines: [Measured(.footprintMB, .cachedFleet40): 200])
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
            XCTAssertEqual(
                error as? PerfError,
                .missingBaseline(Measured(.coldFirstFrameMs, .cachedFleet40)))
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
            XCTAssertEqual(
                error as? PerfError,
                .tooFewSamples(Measured(.coldFirstFrameMs, .cachedFleet40), 3))
        }
    }

    /// Reconciliation is measured twice — with no network in front of it and
    /// behind a hundred milliseconds of one — and the definitions hold the app
    /// to the same budget under each. Pooled into one median, five fast
    /// samples would carry five slow ones straight past it.
    func testEachWorkloadIsJudgedAgainstTheBudgetOnItsOwn() throws {
        let table = BudgetTable(
            machines: machines,
            budgets: [.reconciliationMs: Budget(
                unit: .milliseconds, median: 1000, worst: nil, tolerance: 0.15)])
        let verdict = try judge(
            samples: samples(.reconciliationMs, [5, 6, 7, 8, 9], workload: .latency0)
                + samples(
                    .reconciliationMs, [1100, 1200, 1300, 1400, 1500], workload: .latency100),
            budgets: table, machine: "pinned-mac")

        XCTAssertFalse(verdict.passed)
        XCTAssertEqual(verdict.results.count, 2)
        let fast = try XCTUnwrap(verdict.results.first { $0.workload == .latency0 })
        XCTAssertTrue(fast.passed)
        XCTAssertEqual(fast.median, 7)
        XCTAssertNil(fast.note)
        let slow = try XCTUnwrap(verdict.results.first { $0.workload == .latency100 })
        XCTAssertFalse(slow.passed)
        XCTAssertEqual(slow.median, 1300)
        XCTAssertEqual(slow.budget, 1000)
        // The failure names the workload it is about, so a line in a verdict
        // says which of the two reconciliations went over.
        XCTAssertTrue(try XCTUnwrap(slow.note).contains("latency100"))
        XCTAssertEqual(verdict.results.filter { !$0.passed }.map(\.workload), [.latency100])
    }

    /// A baseline is recorded for a metric under a workload. One keyed by the
    /// metric alone would hold the slow reconciliation to the fast one's
    /// recorded number, and every run would fail for the wrong reason.
    func testBaselinesAreRecordedPerWorkload() throws {
        let table = BudgetTable(machines: machines, budgets: [:])
            .with(baselines: [
                Measured(.reconciliationMs, .latency0): 6,
                Measured(.reconciliationMs, .latency100): 1200,
            ])
        let verdict = try judge(
            samples: samples(.reconciliationMs, [5, 6, 6, 6, 7], workload: .latency0)
                + samples(
                    .reconciliationMs, [1150, 1180, 1200, 1210, 1220], workload: .latency100),
            budgets: table, machine: "macos-26")
        XCTAssertTrue(verdict.passed)
        XCTAssertEqual(
            verdict.results.map(\.baseline), [6, 1200],
            "each row is compared with what its own workload recorded")
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
