import Foundation

/// One machine the suite is allowed to run on.
///
/// `hard` machines are the ones a budget was written for: the pinned Mac.
/// Every other machine is slower or noisier in ways nobody agreed to, so it is
/// judged against a baseline it recorded once, with the same tolerance. That
/// is the difference between accepting a machine and loosening a budget.
public struct MachineRow: Sendable, Equatable {
    public let name: String
    /// The `hw.model` this machine reports, when it is identified that way.
    public let model: String?
    public let budgetsAreHard: Bool
    public let baselineRequired: Bool

    public init(name: String, model: String?, budgetsAreHard: Bool, baselineRequired: Bool) {
        self.name = name
        self.model = model
        self.budgetsAreHard = budgetsAreHard
        self.baselineRequired = baselineRequired
    }
}

/// A metric's pinned limits.
public struct Budget: Sendable, Equatable {
    public let unit: MetricUnit
    /// What the median must not exceed.
    public let median: Double?
    /// What the worst sample must not exceed, when the definition pins one.
    public let worst: Double?
    /// How far past a recorded baseline the median may drift, as a fraction.
    public let tolerance: Double

    public init(unit: MetricUnit, median: Double?, worst: Double?, tolerance: Double) {
        self.unit = unit
        self.median = median
        self.worst = worst
        self.tolerance = tolerance
    }
}

/// The machines and budgets, read from `docs/IOS_PERFORMANCE.md`.
///
/// The document is the source rather than a copy of one: the numbers a person
/// reads and the numbers the suite enforces cannot disagree if there is only
/// one of them.
public struct BudgetTable: Sendable, Equatable {
    public let machines: [MachineRow]
    public let budgets: [Metric: Budget]
    /// What this machine measured when its baseline was recorded, if it has
    /// one. Loaded from `ios/Perf/baselines/<machine>.json`.
    public let baselines: [Metric: Double]

    public init(machines: [MachineRow], budgets: [Metric: Budget], baselines: [Metric: Double] = [:]) {
        self.machines = machines
        self.budgets = budgets
        self.baselines = baselines
    }

    public func machine(_ name: String) -> MachineRow? {
        machines.first { $0.name == name }
    }

    /// The machine that reports this `hw.model`, if the table names one.
    public func machine(model: String) -> MachineRow? {
        machines.first { $0.model == model }
    }

    public func budget(_ metric: Metric) -> Budget? { budgets[metric] }
    public func baseline(_ metric: Metric) -> Double? { baselines[metric] }

    public func with(baselines: [Metric: Double]) -> BudgetTable {
        BudgetTable(machines: machines, budgets: budgets, baselines: baselines)
    }
}

public enum BudgetTableError: Error, Sendable, Equatable {
    case sectionMissing(String)
    case rowUnreadable(String)
}

extension BudgetTable {
    /// Reads the `Machines` and `Budgets` tables out of the measurement
    /// document. Everything else in it is prose for a person; these two tables
    /// are the part the suite obeys.
    public static func parse(markdown: String) throws(BudgetTableError) -> BudgetTable {
        var machines: [MachineRow] = []
        for cells in try rows(of: "Machines", in: markdown) {
            guard cells.count >= 4 else { throw BudgetTableError.rowUnreadable(cells.joined(separator: " | ")) }
            let model = bare(cells[1])
            machines.append(MachineRow(
                name: bare(cells[0]),
                model: model == "—" ? nil : model,
                budgetsAreHard: cells[2].contains("hard"),
                baselineRequired: cells[3].contains("required")))
        }
        var budgets: [Metric: Budget] = [:]
        for cells in try rows(of: "Budgets", in: markdown) {
            guard cells.count >= 5, let metric = Metric(rawValue: bare(cells[0])),
                  let unit = MetricUnit(rawValue: bare(cells[1])) else {
                throw BudgetTableError.rowUnreadable(cells.joined(separator: " | "))
            }
            guard let tolerance = percentage(cells[4]) else {
                throw BudgetTableError.rowUnreadable(cells.joined(separator: " | "))
            }
            budgets[metric] = Budget(
                unit: unit,
                median: Double(bare(cells[2])),
                worst: Double(bare(cells[3])),
                tolerance: tolerance)
        }
        guard !machines.isEmpty else { throw BudgetTableError.sectionMissing("Machines") }
        guard !budgets.isEmpty else { throw BudgetTableError.sectionMissing("Budgets") }
        return BudgetTable(machines: machines, budgets: budgets)
    }

    /// The body rows of the markdown table under the named heading.
    private static func rows(
        of section: String, in markdown: String
    ) throws(BudgetTableError) -> [[String]] {
        let lines = markdown.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        guard let start = lines.firstIndex(where: { $0.trimmed == "## \(section)" }) else {
            throw BudgetTableError.sectionMissing(section)
        }
        var body: [[String]] = []
        var seenHeader = false
        for line in lines[(start + 1)...] {
            let text = line.trimmed
            if text.hasPrefix("## ") { break }
            guard text.hasPrefix("|") else { continue }
            let cells = text.split(separator: "|", omittingEmptySubsequences: false)
                .dropFirst().dropLast().map { $0.trimmed }
            // The header row and the dashes under it describe the table
            // rather than state a machine or a budget.
            if !seenHeader { seenHeader = true; continue }
            if cells.allSatisfy({ $0.allSatisfy { $0 == "-" || $0 == ":" } }) { continue }
            body.append(cells)
        }
        guard !body.isEmpty else { throw BudgetTableError.sectionMissing(section) }
        return body
    }

    private static func bare(_ cell: String) -> String {
        cell.replacingOccurrences(of: "`", with: "").trimmed
    }

    private static func percentage(_ cell: String) -> Double? {
        let text = bare(cell).replacingOccurrences(of: "%", with: "")
        guard let value = Double(text) else { return nil }
        return value / 100
    }
}

extension String {
    fileprivate var trimmed: String { trimmingCharacters(in: .whitespaces) }
}

extension Substring {
    fileprivate var trimmed: String { String(self).trimmingCharacters(in: .whitespaces) }
}
