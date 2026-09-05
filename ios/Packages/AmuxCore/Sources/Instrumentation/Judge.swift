import Foundation

/// What is measured. One name per number the app is held to, so a budget, a
/// sample, a baseline and a verdict all spell the same thing.
public enum Metric: String, Codable, Sendable, CaseIterable {
    case coldFirstFrameMs
    case reconciliationMs
    case echoFrames
    case hitchTimeRatioMsPerS
    case mainThreadCpuPercent
    case footprintMB
    case idleCommits
    case connectionsPerHost
    case backgroundConnections
    case foregroundRecoveryMs
}

/// Named `MetricUnit` rather than `Unit`: Foundation already has a `Unit`,
/// and a measurement type that has to be qualified everywhere it appears is
/// a type nobody will spell correctly.
public enum MetricUnit: String, Codable, Sendable {
    case milliseconds = "ms"
    case millisecondsPerSecond = "ms/s"
    case percent = "%"
    case megabytes = "MB"
    case count
}

/// One measurement of one metric under one workload.
///
/// `proxy` is carried on the sample rather than worked out later: whether a
/// number stands for what it claims to stand for is a property of how it was
/// taken, and a simulator's 60 Hz cannot be turned back into a phone's 120 Hz
/// by a report.
public struct MetricSample: Codable, Sendable, Equatable {
    public let metric: Metric
    public let value: Double
    public let unit: MetricUnit
    public let proxy: Bool
    public let workload: Workload

    public init(metric: Metric, value: Double, unit: MetricUnit, proxy: Bool, workload: Workload) {
        self.metric = metric
        self.value = value
        self.unit = unit
        self.proxy = proxy
        self.workload = workload
    }
}

/// One metric under one workload: the pair a budget is judged at and a
/// baseline is recorded for.
///
/// Reconciliation with no network in front of it and reconciliation behind a
/// hundred milliseconds of it are two different measurements of the same
/// metric, and the definitions hold the app to the budget at each. Pooling
/// them would let the fast one carry the slow one.
public struct Measured: Codable, Sendable, Hashable {
    public let metric: Metric
    public let workload: Workload

    public init(_ metric: Metric, _ workload: Workload) {
        self.metric = metric
        self.workload = workload
    }

    /// The pair written as one string, for a baseline file and a printed line:
    /// `reconciliationMs.latency100`.
    public var name: String { "\(metric.rawValue).\(workload.rawValue)" }

    public init?(name: String) {
        let parts = name.split(separator: ".", maxSplits: 1)
        guard parts.count == 2, let metric = Metric(rawValue: String(parts[0])),
            let workload = Workload(rawValue: String(parts[1]))
        else { return nil }
        self.init(metric, workload)
    }
}

public struct MetricResult: Codable, Sendable, Equatable {
    public let metric: Metric
    /// Which workload produced these samples. Two rows can carry the same
    /// metric, and each is judged against the one pinned budget on its own.
    public let workload: Workload
    public let budget: Double?
    public let baseline: Double?
    public let median: Double
    public let worst: Double
    public let proxy: Bool
    public let passed: Bool
    /// Why it failed, in one line, or nothing when it passed.
    public let note: String?

    public init(
        metric: Metric, workload: Workload, budget: Double?, baseline: Double?, median: Double,
        worst: Double, proxy: Bool, passed: Bool, note: String? = nil
    ) {
        self.metric = metric
        self.workload = workload
        self.budget = budget
        self.baseline = baseline
        self.median = median
        self.worst = worst
        self.proxy = proxy
        self.passed = passed
        self.note = note
    }
}

public struct PerfVerdict: Codable, Sendable, Equatable {
    public let machine: String
    public let simulator: String
    public let results: [MetricResult]
    public let passed: Bool

    public init(machine: String, simulator: String, results: [MetricResult], passed: Bool) {
        self.machine = machine
        self.simulator = simulator
        self.results = results
        self.passed = passed
    }
}

public enum PerfError: Error, Sendable, Equatable {
    case unknownMachine(String)
    case missingBaseline(Measured)
    case tooFewSamples(Measured, Int)
}

/// How many measurements of a metric under one workload a verdict is allowed
/// to rest on. Five, so the median is a median rather than a coin toss.
public let requiredSamples = 5

/// Turns samples into a verdict.
///
/// A budget is never loosened to fit a machine: a machine either has hard
/// budgets or is judged against its own recorded baseline, and the tolerance
/// is the same either way. Anything the table cannot judge — an unknown
/// machine, a metric with too few samples, a baseline the machine is required
/// to have recorded — is an error rather than a pass.
public func judge(
    samples: [MetricSample], budgets: BudgetTable, machine: String, simulator: String = ""
) throws(PerfError) -> PerfVerdict {
    guard let row = budgets.machine(machine) else { throw PerfError.unknownMachine(machine) }

    // One row per metric and workload, in the order the two enumerations are
    // written, so a verdict reads the same way every run.
    var results: [MetricResult] = []
    for metric in Metric.allCases {
        for workload in Workload.allCases {
            let measured = Measured(metric, workload)
            let taken = samples.filter { $0.metric == metric && $0.workload == workload }
            guard !taken.isEmpty else { continue }
            guard taken.count >= requiredSamples else {
                throw PerfError.tooFewSamples(measured, taken.count)
            }
            let values = taken.map(\.value).sorted()
            let middle = values[values.count / 2]
            let worst = values[values.count - 1]
            let budget = budgets.budget(metric)
            let baseline = budgets.baseline(measured)
            if row.baselineRequired && baseline == nil {
                throw PerfError.missingBaseline(measured)
            }

            var note: String?
            if row.budgetsAreHard, let limit = budget?.median, middle > limit {
                note = "median \(rounded(middle)) is over the budget of \(rounded(limit))"
            }
            if note == nil, row.budgetsAreHard, let limit = budget?.worst, worst > limit {
                note = "worst \(rounded(worst)) is over the worst-case budget of "
                    + "\(rounded(limit))"
            }
            if note == nil, let baseline {
                let tolerance = budget?.tolerance ?? 0
                let allowed = baseline * (1 + tolerance)
                if middle > allowed {
                    note = "median \(rounded(middle)) is more than "
                        + "\(Int((tolerance * 100).rounded()))% over the recorded "
                        + "\(rounded(baseline))"
                }
            }
            results.append(MetricResult(
                metric: metric,
                workload: workload,
                budget: budget?.median,
                baseline: baseline,
                median: middle,
                worst: worst,
                proxy: taken.contains { $0.proxy },
                passed: note == nil,
                note: note.map { "\(workload.rawValue): \($0)" }))
        }
    }
    return PerfVerdict(
        machine: machine,
        simulator: simulator,
        results: results,
        passed: results.allSatisfy(\.passed))
}

private func rounded(_ value: Double) -> String {
    String(format: value < 10 ? "%.2f" : "%.1f", value)
}
