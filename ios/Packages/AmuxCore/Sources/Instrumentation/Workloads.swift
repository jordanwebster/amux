import AmuxCore
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
    public static func cachedFleet(seed: UInt64 = seed, reconciled: Bool = false) -> Fleet {
        var random = Deterministic(seed: seed)
        let hosts = (0..<3).map { index -> HostState in
            HostState(
                entry: HostEntry(
                    id: HostId(uuid(seed: seed, index: index, prefix: 0x40)),
                    name: ["studio", "mini", "air"][index],
                    online: true,
                    version: "0.4.0"),
                epoch: 1)
        }
        // The composition is the pinned one; the order they are dealt in is
        // shuffled by the seed so the list is not sorted by state to begin
        // with, which is what makes the ordering work measurable.
        var states: [(Attention, AgentPhase, Double)] = []
        for index in 0..<6 {
            let why: Why = index % 2 == 0 ? .permission : .question
            states.append((.needsYou(why: why), .running, Double(2 + index)))
        }
        for index in 0..<4 { states.append((.needsYou(why: .finished), .exited(exitCode: 0), Double(9 + index))) }
        for index in 0..<3 { states.append((.unknown, .running, Double(20 + index))) }
        for index in 0..<5 { states.append((.idle, .running, 1_440 + Double(index * 7))) }
        for index in 0..<22 {
            states.append((index % 2 == 0 ? .working : .idle, .running, Double(30 + index * 3)))
        }
        states.shuffle(using: &random)

        let agents = states.enumerated().map { index, state -> AgentCard in
            let (attention, phase, minutesAgo) = state
            let host = hosts[index % hosts.count].entry.id
            let name = "\(project(random.next()))-\(index + 1)"
            return AgentCard(
                agent: Agent(
                    id: AgentId(uuid(seed: seed, index: 1_000 + index, prefix: 0xA6)),
                    hostId: host,
                    name: name,
                    command: index % 3 == 0 ? "codex" : "claude",
                    workingDir: "/Users/pat/source/\(name)",
                    kind: index % 3 == 0 ? .codex : .claude(driver: .pty),
                    createdAt: now.addingTimeInterval(-86_400 * 3),
                    workingOn: WorkingOn(
                        text: "\(verb(random.next())) the \(noun(random.next()))",
                        updatedAt: now.addingTimeInterval(-60 * minutesAgo))),
                displayName: name,
                attention: attention,
                phase: phase,
                lastActivity: now.addingTimeInterval(-60 * minutesAgo))
        }
        return Fleet(epoch: 1, agents: agents, hosts: hosts, reconciled: reconciled)
    }

    // MARK: - The conversation

    /// A thousand rows in the pinned mixture: 55% prose with markdown, 20%
    /// tool rows, 10% folded reads, 5% command output over 200 lines, 5%
    /// edits, 5% rules and rows this build does not know.
    ///
    /// `from` is the identity of the first row. A feed never hands out an
    /// identity twice, so rows generated to arrive on top of an existing
    /// transcript continue its numbering rather than starting again: repeated
    /// identities make the list's diffing undefined, and a measurement taken
    /// over an undefined list is a measurement of nothing.
    public static func conversation(
        agent: AgentId, rows count: Int = 1_000, seed: UInt64 = seed, from first: Int = 1
    ) -> [FeedEntry] {
        var random = Deterministic(seed: seed)
        var kinds: [Kind] = []
        kinds += Array(repeating: .prose, count: count * 55 / 100)
        kinds += Array(repeating: .tool, count: count * 20 / 100)
        kinds += Array(repeating: .foldedRead, count: count * 10 / 100)
        kinds += Array(repeating: .longOutput, count: count * 5 / 100)
        kinds += Array(repeating: .edit, count: count * 5 / 100)
        kinds += Array(repeating: .unknown, count: count - kinds.count)
        kinds.shuffle(using: &random)
        return kinds.enumerated().map { index, kind in
            entry(kind, id: first + index, random: &random)
        }
    }

    /// The stream: fresh rows handed out in the batches a fifty rows a second
    /// arrival would deliver them in, numbered to land on top of the
    /// `onTopOf`-row transcript they arrive into.
    public static func stream(
        agent: AgentId, rowsPerSecond: Int = 50, seconds: Int = 20, seed: UInt64 = seed,
        onTopOf existing: Int = 1_000
    ) -> [[FeedEntry]] {
        let rows = conversation(
            agent: agent, rows: rowsPerSecond * seconds, seed: seed &+ 1, from: existing + 1)
        return stride(from: 0, to: rows.count, by: rowsPerSecond).map {
            Array(rows[$0..<min($0 + rowsPerSecond, rows.count)])
        }
    }

    /// A feed update that appends these rows at this position.
    public static func append(_ rows: [FeedEntry], to agent: AgentId, at base: UInt64) -> Event {
        .feed(FeedUpdate(agent: agent, base: base, append: rows, replace: [], evicted: 0))
    }

    private enum Kind { case prose, tool, foldedRead, longOutput, edit, unknown }

    private static func entry(_ kind: Kind, id: Int, random: inout Deterministic) -> FeedEntry {
        switch kind {
        case .prose:
            let text = "**\(verb(random.next()).capitalized)** the `\(noun(random.next()))` "
                + "so the \(noun(random.next())) can be read.\n\n- \(noun(random.next()))\n"
                + "- \(noun(random.next()))"
            return row(id, .object([
                "entry": .string("message"),
                "message_id": .string("msg_\(id)"),
                "segments": .array([.string(text)]),
                "content": .array([.object(["segment": .string("prose"), "value": .string(text)])]),
                "finality": .object(["finality": .string("final"), "stop_reason": .string("end_turn")]),
            ]))
        case .tool:
            return tool(id, name: "Grep",
                invocation: .object(["tool": .string("query"), "text": .string(noun(random.next()))]),
                head: "\(random.next() % 40 + 1) matches", grouped: false)
        case .foldedRead:
            return tool(id, name: "Read",
                invocation: .object([
                    "tool": .string("read"),
                    "file_path": .string("/Users/pat/source/\(noun(random.next())).swift"),
                ]),
                head: "140 lines", grouped: true)
        case .longOutput:
            let output = (0..<220).map { "  \(noun(random.next())) \($0)" }.joined(separator: "\n")
            return tool(id, name: "Bash",
                invocation: .object([
                    "tool": .string("bash"),
                    "command": .string("swift build 2>&1 | tail -220"),
                    "description": .null,
                ]),
                head: output, grouped: false, truncated: true)
        case .edit:
            let path = "/Users/pat/source/\(noun(random.next())).swift"
            return tool(id, name: "Edit",
                invocation: .object([
                    "tool": .string("edit"), "file_path": .string(path),
                    "replace_all": .bool(false),
                ]),
                facts: .object([
                    "facts": .string("edit"),
                    "file_path": .string(path),
                    "added": .int(Int(random.next() % 30) + 1),
                    "removed": .int(Int(random.next() % 10)),
                ]))
        case .unknown:
            // A row kind this build has no drawing for. The list must place
            // it without knowing what it is, and measuring without one would
            // measure a transcript the app will never actually receive.
            return row(id, .object([
                "entry": .string("rule"),
                "rule": .string("a row this build does not know"),
            ]))
        }
    }

    private static func tool(
        _ id: Int, name: String, invocation: JSONValue, head: String = "done",
        grouped: Bool = false, truncated: Bool = false, facts: JSONValue? = nil
    ) -> FeedEntry {
        row(id, .object([
            "entry": .string("tool"),
            "name": .string(name),
            "grouped": .bool(grouped),
            "invocation": invocation,
            "outcome": .object([
                "outcome": .string("success"),
                "facts": facts ?? .object([
                    "facts": .string("output"),
                    "head": .string(head),
                    "truncated": .bool(truncated),
                ]),
            ]),
        ]))
    }

    private static func row(_ id: Int, _ body: JSONValue) -> FeedEntry {
        var fields: [String: JSONValue] = ["id": .int(id), "seq": .int(id)]
        if case .object(let object) = body {
            for (key, value) in object { fields[key] = value }
        }
        fields["kind"] = .object(["entry": body["entry"] ?? .string("rule")])
        return FeedEntry(layer: .claudePty, row: .object(fields))
    }

    // MARK: - Words and identity

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
