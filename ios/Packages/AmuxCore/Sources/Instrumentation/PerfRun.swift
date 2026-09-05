import Foundation

/// What the Mac tells the app before a measured run, and where the run's
/// answers are left.
///
/// The app's own container is the meeting place: the recipe writes the inputs
/// into it before launching anything, every cold launch appends its own
/// sample to it, and the suite reads the lot and leaves a verdict the recipe
/// copies back out. Nothing has to be passed through a launch argument, so a
/// cold launch is a plain launch.
public enum PerfFiles {
    public static var directory: URL {
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return documents.appendingPathComponent("perf", isDirectory: true)
    }

    public static var inputs: URL { directory.appendingPathComponent("inputs.json") }
    /// One line of JSON per cold launch, appended by the app itself.
    public static var coldSamples: URL { directory.appendingPathComponent("cold-samples.jsonl") }
    public static var samples: URL { directory.appendingPathComponent("samples.json") }
    public static var verdict: URL { directory.appendingPathComponent("verdict.json") }
    public static var cadence: URL { directory.appendingPathComponent("cadence.json") }

    public static func ensure() {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }
}

/// The facts only the Mac knows: which machine this is, which simulator is
/// booted, and the measurement document the budgets are written in.
public struct PerfInputs: Codable, Sendable, Equatable {
    public let machine: String
    public let simulator: String
    /// `docs/IOS_PERFORMANCE.md`, verbatim.
    public let measurements: String
    /// This machine's recorded baseline, when it has one, keyed by metric and
    /// workload written as `reconciliationMs.latency100`.
    public let baselines: [String: Double]

    public init(machine: String, simulator: String, measurements: String, baselines: [String: Double]) {
        self.machine = machine
        self.simulator = simulator
        self.measurements = measurements
        self.baselines = baselines
    }

    public static func read() throws -> PerfInputs {
        try JSONDecoder().decode(PerfInputs.self, from: Data(contentsOf: PerfFiles.inputs))
    }

    /// The budget table these inputs describe, baselines included.
    public func budgets() throws -> BudgetTable {
        var recorded: [Measured: Double] = [:]
        for (name, value) in baselines {
            guard let measured = Measured(name: name) else { continue }
            recorded[measured] = value
        }
        return try BudgetTable.parse(markdown: measurements).with(baselines: recorded)
    }
}

/// Collects samples across a run and leaves the verdict where the recipe
/// looks for it.
public struct PerfRun: Sendable {
    public let inputs: PerfInputs
    private var taken: [MetricSample] = []

    public init(inputs: PerfInputs) {
        self.inputs = inputs
        PerfFiles.ensure()
    }

    public mutating func record(_ sample: MetricSample) {
        taken.append(sample)
    }

    public var samples: [MetricSample] { taken }

    /// Every cold-start sample the app left behind, oldest first.
    public static func coldSamples() -> [MetricSample] {
        guard let text = try? String(contentsOf: PerfFiles.coldSamples, encoding: .utf8) else {
            return []
        }
        let decoder = JSONDecoder()
        return text.split(separator: "\n").compactMap {
            try? decoder.decode(MetricSample.self, from: Data($0.utf8))
        }
    }

    /// Appends one sample the app measured about its own launch. Called by the
    /// app, once, when the first frame carrying cached rows has been shown.
    public static func appendColdSample(_ sample: MetricSample) {
        PerfFiles.ensure()
        guard let json = try? JSONEncoder().encode(sample) else { return }
        var line = json
        line.append(0x0A)
        if let handle = try? FileHandle(forWritingTo: PerfFiles.coldSamples) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        } else {
            try? line.write(to: PerfFiles.coldSamples)
        }
    }

    public static func forgetColdSamples() {
        try? FileManager.default.removeItem(at: PerfFiles.coldSamples)
    }

    /// Judges what was measured and writes both the samples and the verdict.
    @discardableResult
    public func finish(cadence: FrameCadence?) throws -> PerfVerdict {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(taken).write(to: PerfFiles.samples)
        if let cadence { try encoder.encode(cadence).write(to: PerfFiles.cadence) }
        let verdict = try judge(
            samples: taken, budgets: inputs.budgets(),
            machine: inputs.machine, simulator: inputs.simulator)
        try encoder.encode(verdict).write(to: PerfFiles.verdict)
        return verdict
    }
}
