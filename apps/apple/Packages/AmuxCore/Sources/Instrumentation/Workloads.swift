import AmuxCore
import AmuxValues
import Foundation

/// The named states a measurement is taken over.
///
/// A workload is generated from its seed rather than recorded, so the CI
/// runner and the pinned Mac measure the same bytes without shipping a
/// megabyte of fixture, and a number from last month can be compared to one
/// from today.
public enum Workload: String, Codable, Sendable, CaseIterable {
    /// 40 cached agents over 3 hosts.
    case cachedFleet40
    /// A thousand transcript rows in the pinned mixture.
    case conversation1000
    /// Twenty seconds of rows arriving at fifty a second.
    case stream50PerSecond20s
    /// The fleet arriving with no delay in front of it.
    case latency0
    /// The fleet arriving behind a hundred milliseconds of network.
    case latency100
    /// The phone reached, put away for thirty seconds and picked up again,
    /// against machines a runner is really running behind a real relay.
    case putAwayAndPickedUp

    /// How long the runner holds each delivery back, for the workloads that
    /// are about the network rather than about the data.
    public var latencyMilliseconds: Int? {
        switch self {
        case .latency0: 0
        case .latency100: 100
        default: nil
        }
    }
}

/// Generates every workload from a seed.
///
/// Nothing here is random in the sense that matters: the same seed gives the
/// same agents with the same names, ages and states, and the same thousand
/// rows in the same order, on every machine.
public enum Workloads {
    /// The pinned seed. A different seed is a different workload, and would be
    /// stated as one.
    public static let seed: UInt64 = 1

    /// A fixed morning, so an age rendered into a row is the same age forever.
    public static let now = Date(timeIntervalSince1970: 1_764_580_800)

    // MARK: - The fleet

    /// The 40-agent fleet: 6 needing you, 4 finished, 3 unknown, 5 a day old,
    /// and the rest running or idle, spread over 3 hosts.
    /// The pinned fleet: forty agents on three hosts, six asking, four
    /// finished, three starting, five quiet for a day and the rest working or
    /// idle, dealt in no particular order, as the runtime lists them.
    public static func cachedFleet(seed: UInt64 = seed) -> [FleetRow] {
        var random = Deterministic(seed: seed)
        let hosts = ["studio", "mini", "air"].enumerated().map { index, name in
            (id: uuid(seed: seed, index: index, prefix: 0x40), name: name)
        }
        var states: [(Attention, Double)] = []
        for index in 0..<6 { states.append((.needsYou, Double(2 + index))) }
        for index in 0..<4 { states.append((.exited, Double(9 + index))) }
        for index in 0..<3 { states.append((.starting, Double(20 + index))) }
        for index in 0..<5 { states.append((.idle, 1_440 + Double(index * 7))) }
        for index in 0..<22 {
            states.append((index % 2 == 0 ? .working : .idle, Double(30 + index * 3)))
        }
        states.shuffle(using: &random)
        return states.enumerated().map { index, state in
            let (attention, minutesAgo) = state
            let host = hosts[index % hosts.count]
            let name = "\(project(random.next()))-\(index + 1)"
            let agent = uuid(seed: seed, index: 1_000 + index, prefix: 0xA6)
            let card = FleetCard(
                agent: AgentKey(host: bytes(host.id), agent: bytes(agent)),
                name: name, kind: index % 3 == 0 ? .codex : .claudeSdk, attention: attention,
                cwd: "/Users/pat/source/\(name)",
                lastActivityMs: Int64(now.addingTimeInterval(-60 * minutesAgo)
                    .timeIntervalSince1970 * 1000),
                host: host.name, hostPresence: .online, children: 0, familyAttention: attention,
                exitCause: attention == .exited ? "finished" : nil,
                workingOn: "\(verb(random.next())) the \(noun(random.next()))")
            return FleetRow(card: card, depth: 0, expanded: false)
        }
    }

    private static func bytes(_ uuid: UUID) -> [UInt8] {
        withUnsafeBytes(of: uuid.uuid) { Array($0) }
    }

    private static let verbs = ["reading", "folding", "pinning", "measuring", "drawing", "pairing"]
    private static let nouns = ["transcript", "fleet", "bridge", "relay", "golden", "composer"]
    private static let projects = ["amux", "relay", "phone", "core", "design", "runner"]

    private static func verb(_ value: UInt64) -> String { verbs[Int(value % UInt64(verbs.count))] }
    private static func noun(_ value: UInt64) -> String { nouns[Int(value % UInt64(nouns.count))] }
    private static func project(_ value: UInt64) -> String {
        projects[Int(value % UInt64(projects.count))]
    }

    /// A UUID that depends only on the seed and the position, so the same
    /// workload names the same agents everywhere.
    private static func uuid(seed: UInt64, index: Int, prefix: UInt8) -> UUID {
        var random = Deterministic(seed: seed &* 0x9E37_79B9 &+ UInt64(index))
        var bytes = [UInt8](repeating: 0, count: 16)
        bytes[0] = prefix
        let first = random.next()
        let second = random.next()
        for offset in 0..<7 { bytes[1 + offset] = UInt8((first >> (8 * UInt64(offset))) & 0xFF) }
        for offset in 0..<8 { bytes[8 + offset] = UInt8((second >> (8 * UInt64(offset))) & 0xFF) }
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                           bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12],
                           bytes[13], bytes[14], bytes[15]))
    }
}

/// SplitMix64: small, fast and specified, so the sequence is the same in every
/// Swift release rather than whatever the standard library's generator does
/// this year.
public struct Deterministic: RandomNumberGenerator, Sendable {
    private var state: UInt64

    public init(seed: UInt64) { self.state = seed }

    public mutating func next() -> UInt64 {
        state = state &+ 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}
