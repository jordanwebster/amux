import AmuxCore
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
    /// One line of JSON per cold launch, holding every moment that launch
    /// marked. The samples say how long a launch took; these say where the
    /// time went inside it.
    public static var coldMarks: URL { directory.appendingPathComponent("cold-marks.jsonl") }
    public static var samples: URL { directory.appendingPathComponent("samples.json") }
    public static var verdict: URL { directory.appendingPathComponent("verdict.json") }
    public static var cadence: URL { directory.appendingPathComponent("cadence.json") }

    public static func ensure() {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }
}

/// The measurements a run takes, as groups a person can ask for one of.
///
/// A whole run is the honest one and is what CI does. Naming one group is for
/// working on it: the streaming measurements take a minute each and asking for
/// them alone skips five cold launches and ten reconciliations that were not
/// going to say anything new.
public enum PerfSection: String, Codable, Sendable, CaseIterable {
    /// The five launches the Mac drives and the app times from the process
    /// table.
    case cold
    /// The fleet arriving, with and without a hundred milliseconds in front
    /// of it.
    case reconciliation
    /// The transcript: a thousand rows with fifty a second arriving on top of
    /// them, and the same thousand rows left alone.
    case streaming
}

/// The facts only the Mac knows: which machine this is, which simulator is
/// booted, the measurement document the budgets are written in, and which
/// measurements were asked for.
public struct PerfInputs: Codable, Sendable, Equatable {
    public let machine: String
    public let simulator: String
    /// `docs/IOS_PERFORMANCE.md`, verbatim.
    public let measurements: String
    /// This machine's recorded baseline, when it has one, keyed by metric and
    /// workload written as `reconciliationMs.latency100`.
    public let baselines: [String: Double]
    /// The one group this run was asked for, or nothing for all of them.
    public let only: PerfSection?
    /// The build configuration the Mac built before it launched anything.
    /// The app cannot see the name, so it is told; whether that build was
    /// actually optimised the app answers for itself in the verdict.
    public let configuration: String

    public init(
        machine: String, simulator: String, measurements: String,
        baselines: [String: Double], only: PerfSection? = nil, configuration: String = ""
    ) {
        self.machine = machine
        self.simulator = simulator
        self.measurements = measurements
        self.baselines = baselines
        self.only = only
        self.configuration = configuration
    }

    /// Whether this run takes that group's measurements.
    public func measures(_ section: PerfSection) -> Bool {
        only == nil || only == section
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

    /// Writes what one cold launch marked, beside the sample it produced.
    ///
    /// A number on its own cannot be acted on: a launch that has got slower
    /// says nothing about whether the extra time went on loading the app or on
    /// running it. The marks say which, so the next regression can be put on
    /// the side of the line it belongs to.
    public static func appendColdMarks(_ marks: [SignpostMark]) {
        PerfFiles.ensure()
        guard let json = try? JSONEncoder().encode(marks) else { return }
        var line = json
        line.append(0x0A)
        if let handle = try? FileHandle(forWritingTo: PerfFiles.coldMarks) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        } else {
            try? line.write(to: PerfFiles.coldMarks)
        }
    }

    public static func forgetColdSamples() {
        try? FileManager.default.removeItem(at: PerfFiles.coldSamples)
        try? FileManager.default.removeItem(at: PerfFiles.coldMarks)
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
            machine: inputs.machine, simulator: inputs.simulator,
            configuration: inputs.configuration)
        try encoder.encode(verdict).write(to: PerfFiles.verdict)
        return verdict
    }
}
