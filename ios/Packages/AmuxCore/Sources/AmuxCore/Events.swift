import Foundation

// The shared Rust bridge projects the reducer's read surface as JSON events.
// These types are that surface in Swift, named as the core names it. Nothing
// here decides anything: a rule the core already owns — a send gate, an
// attention state, a fold — is read here, never recomputed.

/// One projected event. The bridge delivers them in arrays, in order.
public enum Event: Sendable, Equatable, Codable {
    case fleet(Fleet)
    case feed(FeedUpdate)
    case session(SessionSnapshot)
    case opResult(OpResult)
    case diff(DiffUpdate)
    case connection(ConnectionUpdate)
    case tokenRequest(requestId: UInt64)
    case invariant(detail: String)

    private enum Key: String, CodingKey {
        case fleet = "Fleet"
        case feed = "Feed"
        case session = "Session"
        case opResult = "OpResult"
        case diff = "Diff"
        case connection = "Connection"
        case tokenRequest = "TokenRequest"
        case invariant = "Invariant"
    }

    private struct RequestId: Codable, Sendable, Equatable {
        var request_id: UInt64
    }

    private struct Detail: Codable, Sendable, Equatable {
        var detail: String
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        guard let key = container.allKeys.first, container.allKeys.count == 1 else {
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath,
                debugDescription: "an event is one tagged variant, got \(container.allKeys)"))
        }
        switch key {
        case .fleet: self = .fleet(try container.decode(Fleet.self, forKey: key))
        case .feed: self = .feed(try container.decode(FeedUpdate.self, forKey: key))
        case .session: self = .session(try container.decode(SessionSnapshot.self, forKey: key))
        case .opResult: self = .opResult(try container.decode(OpResult.self, forKey: key))
        case .diff: self = .diff(try container.decode(DiffUpdate.self, forKey: key))
        case .connection: self = .connection(try container.decode(ConnectionUpdate.self, forKey: key))
        case .tokenRequest:
            self = .tokenRequest(requestId: try container.decode(RequestId.self, forKey: key).request_id)
        case .invariant:
            self = .invariant(detail: try container.decode(Detail.self, forKey: key).detail)
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .fleet(let value): try container.encode(value, forKey: .fleet)
        case .feed(let value): try container.encode(value, forKey: .feed)
        case .session(let value): try container.encode(value, forKey: .session)
        case .opResult(let value): try container.encode(value, forKey: .opResult)
        case .diff(let value): try container.encode(value, forKey: .diff)
        case .connection(let value): try container.encode(value, forKey: .connection)
        case .tokenRequest(let id):
            try container.encode(RequestId(request_id: id), forKey: .tokenRequest)
        case .invariant(let detail):
            try container.encode(Detail(detail: detail), forKey: .invariant)
        }
    }
}

// MARK: - Fleet

public struct Fleet: Codable, Sendable, Equatable {
    public var epoch: UInt64
    public var agents: [AgentCard]
    public var hosts: [HostState]
    /// The core's own word for "this list is confirmed", not a spinner: it is
    /// true only once the model has synchronized on a connected relay.
    public var reconciled: Bool

    public init(epoch: UInt64, agents: [AgentCard], hosts: [HostState], reconciled: Bool) {
        self.epoch = epoch
        self.agents = agents
        self.hosts = hosts
        self.reconciled = reconciled
    }
}

public struct AgentCard: Codable, Sendable, Equatable, Identifiable {
    public var agent: Agent
    public var displayName: String
    public var attention: Attention
    public var phase: AgentPhase
    public var lastActivity: Date
    /// What this agent's last finished turn changed, when the provider
    /// counted it. Absent means the counts are not known, never that nothing
    /// changed, so a row states the outcome without arithmetic rather than
    /// claiming a zero.
    public var outcome: TurnOutcome?
    /// Remembered from the last run and not yet confirmed by the machine that
    /// owns this agent.
    ///
    /// Hosts answer one at a time, so a card is confirmed when its own machine
    /// has been heard from rather than when the whole fleet has. That is what
    /// lets a row stop shimmering on its own instead of the list waiting for
    /// the slowest machine on the account.
    public var awaiting: Bool

    public var id: AgentId { agent.id }

    private enum CodingKeys: String, CodingKey {
        case agent
        case displayName = "display_name"
        case attention
        case phase
        case lastActivity = "last_activity"
        case outcome
        case awaiting
    }

    /// Written out rather than synthesised because the bridge leaves
    /// `awaiting` out of a card nobody is waiting on, and a synthesised
    /// decoder would refuse the card rather than read the absence as false.
    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        agent = try fields.decode(Agent.self, forKey: .agent)
        displayName = try fields.decode(String.self, forKey: .displayName)
        attention = try fields.decode(Attention.self, forKey: .attention)
        phase = try fields.decode(AgentPhase.self, forKey: .phase)
        lastActivity = try fields.decode(Date.self, forKey: .lastActivity)
        outcome = try fields.decodeIfPresent(TurnOutcome.self, forKey: .outcome)
        awaiting = try fields.decodeIfPresent(Bool.self, forKey: .awaiting) ?? false
    }

    public init(
        agent: Agent, displayName: String, attention: Attention, phase: AgentPhase,
        lastActivity: Date, outcome: TurnOutcome? = nil, awaiting: Bool = false
    ) {
        self.agent = agent
        self.displayName = displayName
        self.attention = attention
        self.phase = phase
        self.lastActivity = lastActivity
        self.outcome = outcome
        self.awaiting = awaiting
    }
}

/// What one finished turn changed, as the provider counted it.
///
/// A finished turn is stated in words on the row rather than drawn as a tick,
/// and the words are only worth reading if they carry the numbers.
public struct TurnOutcome: Codable, Sendable, Equatable {
    public var files: Int
    public var insertions: Int
    public var deletions: Int
    /// Anything else the provider said about the turn in one short phrase,
    /// such as "3 tests added".
    public var note: String?

    public init(files: Int, insertions: Int, deletions: Int, note: String? = nil) {
        self.files = files
        self.insertions = insertions
        self.deletions = deletions
        self.note = note
    }

    /// "4 files · +118 −40 · 3 tests added". The minus is a true minus sign,
    /// not a hyphen: it sits beside a plus and has to read as its opposite.
    public var arithmetic: String {
        var parts = ["\(files) file\(files == 1 ? "" : "s")", "+\(insertions) \u{2212}\(deletions)"]
        if let note { parts.append(note) }
        return parts.joined(separator: " · ")
    }
}

public struct Agent: Codable, Sendable, Equatable, Identifiable {
    public var id: AgentId
    public var hostId: HostId
    public var name: String?
    public var command: String
    public var workingDir: String
    public var kind: AgentKind
    public var readonly: Bool
    public var args: [String]
    public var createdAt: Date
    public var parent: AgentParent?
    public var workingOn: WorkingOn?

    private enum CodingKeys: String, CodingKey {
        case id
        case hostId = "host_id"
        case name
        case command
        case workingDir = "working_dir"
        case kind
        case readonly
        case args
        case createdAt = "created_at"
        case parent
        case workingOn = "working_on"
    }

    public init(
        id: AgentId, hostId: HostId, name: String?, command: String, workingDir: String,
        kind: AgentKind, readonly: Bool = false, args: [String] = [], createdAt: Date,
        parent: AgentParent? = nil, workingOn: WorkingOn? = nil
    ) {
        self.id = id
        self.hostId = hostId
        self.name = name
        self.command = command
        self.workingDir = workingDir
        self.kind = kind
        self.readonly = readonly
        self.args = args
        self.createdAt = createdAt
        self.parent = parent
        self.workingOn = workingOn
    }
}

public struct AgentParent: Codable, Sendable, Equatable {
    public var agentId: AgentId
    public var hostId: HostId

    private enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case hostId = "host_id"
    }

    public init(agentId: AgentId, hostId: HostId) {
        self.agentId = agentId
        self.hostId = hostId
    }
}

public struct WorkingOn: Codable, Sendable, Equatable {
    public var text: String
    public var updatedAt: Date

    private enum CodingKeys: String, CodingKey {
        case text
        case updatedAt = "updated_at"
    }

    public init(text: String, updatedAt: Date) {
        self.text = text
        self.updatedAt = updatedAt
    }
}

/// The agent and the driver hosting it. A Claude agent's driver decides which
/// chat layer the core folds, so the phone must not flatten the two.
public enum AgentKind: Sendable, Equatable, Codable {
    case claude(driver: ClaudeDriver)
    case codex
    case testAgent

    private enum Key: String, CodingKey { case kind, driver }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "claude": self = .claude(driver: try container.decode(ClaudeDriver.self, forKey: .driver))
        case "codex": self = .codex
        case "test_agent": self = .testAgent
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown agent kind \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .claude(let driver):
            try container.encode("claude", forKey: .kind)
            try container.encode(driver, forKey: .driver)
        case .codex: try container.encode("codex", forKey: .kind)
        case .testAgent: try container.encode("test_agent", forKey: .kind)
        }
    }
}

public enum ClaudeDriver: String, Codable, Sendable, Equatable {
    case pty
    case sdk
}

/// "Does this agent need you", as the core derives it. `unknown` means the
/// host is unreachable and the state genuinely is not known; it is never a
/// loading state and must never be drawn as idle.
public enum Attention: Sendable, Equatable, Codable {
    case unknown
    case idle
    case working
    case needsYou(why: Why)

    private enum Key: String, CodingKey { case attention, why }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .attention) {
        case "unknown": self = .unknown
        case "idle": self = .idle
        case "working": self = .working
        case "needs_you": self = .needsYou(why: try container.decode(Why.self, forKey: .why))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown attention \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .unknown: try container.encode("unknown", forKey: .attention)
        case .idle: try container.encode("idle", forKey: .attention)
        case .working: try container.encode("working", forKey: .attention)
        case .needsYou(let why):
            try container.encode("needs_you", forKey: .attention)
            try container.encode(why, forKey: .why)
        }
    }

    public var why: Why? {
        guard case .needsYou(let why) = self else { return nil }
        return why
    }
}

public enum Why: String, Codable, Sendable, Equatable {
    case permission
    case question
    case finished
}

public enum AgentPhase: Sendable, Equatable, Codable {
    case running
    case exited(exitCode: Int?)

    private enum Key: String, CodingKey { case phase, exit_code }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .phase) {
        case "running": self = .running
        case "exited": self = .exited(exitCode: try container.decodeIfPresent(Int.self, forKey: .exit_code))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown phase \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .running: try container.encode("running", forKey: .phase)
        case .exited(let code):
            try container.encode("exited", forKey: .phase)
            try container.encode(code, forKey: .exit_code)
        }
    }
}

public struct HostState: Codable, Sendable, Equatable, Identifiable {
    public var entry: HostEntry
    public var epoch: UInt64

    public var id: HostId { entry.id }

    public init(entry: HostEntry, epoch: UInt64) {
        self.entry = entry
        self.epoch = epoch
    }
}

public struct HostEntry: Codable, Sendable, Equatable, Identifiable {
    public var id: HostId
    public var name: String
    /// Presence as routing derived it. Nothing probes, so "never tried" is
    /// offline with no dial error rather than a state of its own.
    public var online: Bool
    public var version: String?
    public var capabilities: JSONValue?
    public var trustStatus: HostTrustStatus
    public var lastDialError: String?

    private enum CodingKeys: String, CodingKey {
        case id, name, online, version, capabilities
        case trustStatus = "trust_status"
        case lastDialError = "last_dial_error"
    }

    public init(
        id: HostId, name: String, online: Bool, version: String? = nil,
        capabilities: JSONValue? = nil, trustStatus: HostTrustStatus = .trusted,
        lastDialError: String? = nil
    ) {
        self.id = id
        self.name = name
        self.online = online
        self.version = version
        self.capabilities = capabilities
        self.trustStatus = trustStatus
        self.lastDialError = lastDialError
    }
}

public enum HostTrustStatus: String, Codable, Sendable, Equatable {
    case trusted
    case untrustedButOnline = "untrusted_but_online"
}

// MARK: - Feed

public struct FeedUpdate: Codable, Sendable, Equatable {
    public var agent: AgentId
    /// Absolute position of the first appended row, independent of the rows'
    /// own native identifiers.
    public var base: UInt64
    public var append: [FeedEntry]
    public var replace: [FeedReplacement]
    /// Every position below this is gone; drop it before applying the ranges.
    public var evicted: UInt64

    public init(agent: AgentId, base: UInt64, append: [FeedEntry], replace: [FeedReplacement], evicted: UInt64) {
        self.agent = agent
        self.base = base
        self.append = append
        self.replace = replace
        self.evicted = evicted
    }
}

/// A replaced row and the absolute position it replaces. The bridge writes it
/// as a two-element array.
public struct FeedReplacement: Codable, Sendable, Equatable {
    public var position: UInt64
    public var entry: FeedEntry

    public init(position: UInt64, entry: FeedEntry) {
        self.position = position
        self.entry = entry
    }

    public init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        self.position = try container.decode(UInt64.self)
        self.entry = try container.decode(FeedEntry.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.unkeyedContainer()
        try container.encode(position)
        try container.encode(entry)
    }
}

/// One transcript row, tagged with the layer whose vocabulary it speaks.
///
/// The row bodies differ per layer and no two are interchangeable, so the
/// layer travels with the row. Everything past the identity a fold needs is
/// carried verbatim until the transcript that renders it is built.
public struct FeedEntry: Codable, Sendable, Equatable, Identifiable {
    public var layer: Layer
    public var row: JSONValue

    public enum Layer: String, Codable, Sendable, Equatable {
        case claudePty = "claude_pty"
        case claudeSdk = "claude_sdk"
        case codex
    }

    private enum CodingKeys: String, CodingKey { case layer, row }

    public init(layer: Layer, row: JSONValue) {
        self.layer = layer
        self.row = row
    }

    /// The row's native identifier within its window.
    public var rowId: UInt64 { UInt64(row["id"]?.intValue ?? 0) }
    /// The stream sequence the row was folded from.
    public var seq: UInt64 { UInt64(row["seq"]?.intValue ?? 0) }
    /// The row-kind word this layer used, such as `message` or `tool`.
    public var entryKind: String { row["kind"]?["entry"]?.stringValue ?? "" }

    public var id: String { "\(layer.rawValue):\(rowId)" }
}

// MARK: - Session

public struct SessionSnapshot: Codable, Sendable, Equatable {
    public var agent: AgentId
    public var gate: SendGate
    public var phase: LayerPhase
    public var stream: StreamPhase?
    public var asks: [Ask]
    public var facts: SessionFacts
    public var provider: ProviderFacts
    public var settingsGate: SettingsGate
    public var queue: QueuedMessage?
    public var family: [FamilyMember]

    private enum CodingKeys: String, CodingKey {
        case agent, gate, phase, stream, asks, facts, provider
        case settingsGate = "settings_gate"
        case queue, family
    }

    public init(
        agent: AgentId, gate: SendGate, phase: LayerPhase, stream: StreamPhase?,
        asks: [Ask], facts: SessionFacts, provider: ProviderFacts,
        settingsGate: SettingsGate, queue: QueuedMessage?, family: [FamilyMember]
    ) {
        self.agent = agent
        self.gate = gate
        self.phase = phase
        self.stream = stream
        self.asks = asks
        self.facts = facts
        self.provider = provider
        self.settingsGate = settingsGate
        self.queue = queue
        self.family = family
    }
}

/// Whether this layer will accept a prompt now, and why not when it will not.
/// The two layers' refusals are different vocabularies, kept apart on purpose.
public enum SendGate: Sendable, Equatable, Codable {
    case claudePty(ClaudePtySendGate)
    case codex(CodexSendGate)
    case unavailable

    private enum Key: String, CodingKey { case layer, value }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(try container.decode(ClaudePtySendGate.self, forKey: .value))
        case "codex": self = .codex(try container.decode(CodexSendGate.self, forKey: .value))
        case "unavailable": self = .unavailable
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown gate layer \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .claudePty(let gate):
            try container.encode("claude_pty", forKey: .layer)
            try container.encode(gate, forKey: .value)
        case .codex(let gate):
            try container.encode("codex", forKey: .layer)
            try container.encode(gate, forKey: .value)
        case .unavailable:
            try container.encode("unavailable", forKey: .layer)
        }
    }

    public var accepts: Bool {
        switch self {
        case .claudePty(let gate): gate == .ready
        case .codex(let gate): gate == .ready
        case .unavailable: false
        }
    }
}

public enum ClaudePtySendGate: String, Codable, Sendable, Equatable {
    case ready
    case unavailable
    case exited
    case readOnly = "read_only"
    case replaying
    case working
    case needsYou = "needs_you"
    case unknown
    case sendInFlight = "send_in_flight"
}

public enum ClaudeSdkSendGate: String, Codable, Sendable, Equatable {
    case ready
    case unavailable
    case exited
    case readOnly = "read_only"
    case replaying
    case working
    case needsYou = "needs_you"
    case unknown
    case inputInFlight = "input_in_flight"
}

public enum CodexSendGate: String, Codable, Sendable, Equatable {
    case ready
    case unavailable
    case exited
    case closed
    case replaying
    case activeTurn = "active_turn"
    case needsYou = "needs_you"
    case observerReadOnly = "observer_read_only"
    case readOnly = "read_only"
    case unknown
    case inputInFlight = "input_in_flight"
}

/// The chat phase this layer reports. Its shape is the layer's own.
public enum LayerPhase: Sendable, Equatable, Codable {
    case claudePty(JSONValue)
    case codex(JSONValue)
    case unavailable

    private enum Key: String, CodingKey { case layer, value }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(try container.decode(JSONValue.self, forKey: .value))
        case "codex": self = .codex(try container.decode(JSONValue.self, forKey: .value))
        case "unavailable": self = .unavailable
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown phase layer \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .claudePty(let value):
            try container.encode("claude_pty", forKey: .layer)
            try container.encode(value, forKey: .value)
        case .codex(let value):
            try container.encode("codex", forKey: .layer)
            try container.encode(value, forKey: .value)
        case .unavailable:
            try container.encode("unavailable", forKey: .layer)
        }
    }

    /// The phase word both layers spell the same way: `idle`, `running`, and
    /// whatever else that layer adds.
    public var phase: String? {
        switch self {
        case .claudePty(let value), .codex(let value): value["phase"]?.stringValue
        case .unavailable: nil
        }
    }
}

public enum StreamPhase: Sendable, Equatable, Codable {
    case opening
    case replaying
    case live
    case closed(reason: JSONValue)

    private enum Key: String, CodingKey {
        case streamPhase = "stream_phase"
        case reason
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .streamPhase) {
        case "opening": self = .opening
        case "replaying": self = .replaying
        case "live": self = .live
        case "closed": self = .closed(reason: try container.decode(JSONValue.self, forKey: .reason))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown stream phase \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .opening: try container.encode("opening", forKey: .streamPhase)
        case .replaying: try container.encode("replaying", forKey: .streamPhase)
        case .live: try container.encode("live", forKey: .streamPhase)
        case .closed(let reason):
            try container.encode("closed", forKey: .streamPhase)
            try container.encode(reason, forKey: .reason)
        }
    }
}

/// One outstanding question, in its layer's vocabulary.
public struct Ask: Codable, Sendable, Equatable {
    public var layer: Layer
    public var body: JSONValue

    public enum Layer: String, Codable, Sendable, Equatable {
        case claudePty = "claude_pty"
        case codex
    }

    private enum Key: String, CodingKey { case layer, value }

    public init(layer: Layer, body: JSONValue) {
        self.layer = layer
        self.body = body
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        self.layer = try container.decode(Layer.self, forKey: .layer)
        self.body = try container.decode(JSONValue.self, forKey: .value)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(layer, forKey: .layer)
        try container.encode(body, forKey: .value)
    }
}

/// Session facts belonging to one layer, kept whole under the layer that owns
/// them. `claudeSdk` says outright that this build cannot read that layer.
public enum SessionFacts: Sendable, Equatable, Codable {
    case claudePty(JSONValue)
    case codex(JSONValue)
    case claudeSdk(supported: Bool)
    case unavailable

    private enum Key: String, CodingKey { case layer, supported }

    public init(from decoder: any Decoder) throws {
        let tagged = try decoder.container(keyedBy: Key.self)
        let body = try JSONValue(from: decoder)
        switch try tagged.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(body)
        case "codex": self = .codex(body)
        case "claude_sdk": self = .claudeSdk(supported: try tagged.decode(Bool.self, forKey: .supported))
        case "unavailable": self = .unavailable
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown facts layer \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .claudePty(let body), .codex(let body):
            try body.encode(to: encoder)
        case .claudeSdk(let supported):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode("claude_sdk", forKey: .layer)
            try container.encode(supported, forKey: .supported)
        case .unavailable:
            var container = encoder.container(keyedBy: Key.self)
            try container.encode("unavailable", forKey: .layer)
        }
    }
}

public struct ProviderFacts: Codable, Sendable, Equatable {
    public var model: String?
    public var effort: String?
    public var models: [ModelInfo]
    public var efforts: [String]
    public var commands: [ProviderCommand]
    public var permission: JSONValue
    public var todos: TaskList?

    public init(
        model: String? = nil, effort: String? = nil, models: [ModelInfo] = [],
        efforts: [String] = [], commands: [ProviderCommand] = [],
        permission: JSONValue = .object(["provider": .string("unavailable")]),
        todos: TaskList? = nil
    ) {
        self.model = model
        self.effort = effort
        self.models = models
        self.efforts = efforts
        self.commands = commands
        self.permission = permission
        self.todos = todos
    }
}

public struct ModelInfo: Codable, Sendable, Equatable {
    public var id: String
    public var name: String
    public var efforts: [String]
    public var defaultEffort: String?

    private enum CodingKeys: String, CodingKey {
        case id, name, efforts
        case defaultEffort = "default_effort"
    }

    public init(id: String, name: String, efforts: [String] = [], defaultEffort: String? = nil) {
        self.id = id
        self.name = name
        self.efforts = efforts
        self.defaultEffort = defaultEffort
    }
}

public struct ProviderCommand: Codable, Sendable, Equatable {
    public var name: String
    public var source: JSONValue
    public var terminalOnly: Bool

    private enum CodingKeys: String, CodingKey {
        case name, source
        case terminalOnly = "terminal_only"
    }

    public init(name: String, source: JSONValue, terminalOnly: Bool = false) {
        self.name = name
        self.source = source
        self.terminalOnly = terminalOnly
    }
}

/// The provider's own task list, folded by the core rather than counted here.
public struct TaskList: Codable, Sendable, Equatable {
    public var done: Int
    public var total: Int
    public var current: String?
    public var items: [TaskItem]

    public init(done: Int, total: Int, current: String?, items: [TaskItem]) {
        self.done = done
        self.total = total
        self.current = current
        self.items = items
    }
}

/// One task and its state; the core writes the pair as a two-element array.
public struct TaskItem: Codable, Sendable, Equatable {
    public var text: String
    public var state: TodoState

    public init(text: String, state: TodoState) {
        self.text = text
        self.state = state
    }

    public init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        self.text = try container.decode(String.self)
        self.state = try container.decode(TodoState.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.unkeyedContainer()
        try container.encode(text)
        try container.encode(state)
    }
}

public enum TodoState: String, Codable, Sendable, Equatable {
    case pending
    case inProgress = "in_progress"
    case completed
}

public enum SettingsGate: Sendable, Equatable, Codable {
    case ready
    case ptySettingsUnavailable
    case unavailable
    case codex(reason: CodexSendGate)
    case claudeSdk(reason: ClaudeSdkSendGate)

    private enum Key: String, CodingKey { case gate, reason }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .gate) {
        case "ready": self = .ready
        case "pty_settings_unavailable": self = .ptySettingsUnavailable
        case "unavailable": self = .unavailable
        case "codex": self = .codex(reason: try container.decode(CodexSendGate.self, forKey: .reason))
        case "claude_sdk":
            self = .claudeSdk(reason: try container.decode(ClaudeSdkSendGate.self, forKey: .reason))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown settings gate \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .ready: try container.encode("ready", forKey: .gate)
        case .ptySettingsUnavailable: try container.encode("pty_settings_unavailable", forKey: .gate)
        case .unavailable: try container.encode("unavailable", forKey: .gate)
        case .codex(let reason):
            try container.encode("codex", forKey: .gate)
            try container.encode(reason, forKey: .reason)
        case .claudeSdk(let reason):
            try container.encode("claude_sdk", forKey: .gate)
            try container.encode(reason, forKey: .reason)
        }
    }
}

public struct QueuedMessage: Codable, Sendable, Equatable {
    public var draft: JSONValue
    public var heldAt: Date
    public var delivery: JSONValue

    private enum CodingKeys: String, CodingKey {
        case draft
        case heldAt = "held_at"
        case delivery
    }

    public init(draft: JSONValue, heldAt: Date, delivery: JSONValue) {
        self.draft = draft
        self.heldAt = heldAt
        self.delivery = delivery
    }
}

public struct FamilyMember: Codable, Sendable, Equatable, Identifiable {
    public var agent: AgentId
    public var depth: Int
    public var needs: Why?

    public var id: AgentId { agent }

    public init(agent: AgentId, depth: Int, needs: Why?) {
        self.agent = agent
        self.depth = depth
        self.needs = needs
    }
}

// MARK: - Operations, diffs and the link

public struct OpResult: Codable, Sendable, Equatable {
    public var op: OpId
    public var outcome: OpOutcome

    public init(op: OpId, outcome: OpOutcome) {
        self.op = op
        self.outcome = outcome
    }
}

/// How a dispatched operation ended. Outcomes the app acts on are named; the
/// rest keep their tag and body so nothing is silently swallowed.
public enum OpOutcome: Sendable, Equatable, Codable {
    case inputSent
    case agentCreated(Agent)
    case agentRenamed(Agent)
    case agentDeleted
    case queueRemoved
    case subscribed(agent: AgentId)
    case unsubscribed(agent: AgentId)
    case failed(OpFailure)
    case other(outcome: String, body: JSONValue)

    private enum Key: String, CodingKey { case outcome, agent, error }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let outcome = try container.decode(String.self, forKey: .outcome)
        switch outcome {
        case "input_sent": self = .inputSent
        case "agent_created": self = .agentCreated(try container.decode(Agent.self, forKey: .agent))
        case "agent_renamed": self = .agentRenamed(try container.decode(Agent.self, forKey: .agent))
        case "agent_deleted": self = .agentDeleted
        case "queue_removed": self = .queueRemoved
        case "subscribed": self = .subscribed(agent: try container.decode(AgentId.self, forKey: .agent))
        case "unsubscribed": self = .unsubscribed(agent: try container.decode(AgentId.self, forKey: .agent))
        case "error": self = .failed(try container.decode(OpFailure.self, forKey: .error))
        default: self = .other(outcome: outcome, body: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .other(_, let body):
            try body.encode(to: encoder)
        default:
            var container = encoder.container(keyedBy: Key.self)
            switch self {
            case .inputSent: try container.encode("input_sent", forKey: .outcome)
            case .agentDeleted: try container.encode("agent_deleted", forKey: .outcome)
            case .queueRemoved: try container.encode("queue_removed", forKey: .outcome)
            case .agentCreated(let agent):
                try container.encode("agent_created", forKey: .outcome)
                try container.encode(agent, forKey: .agent)
            case .agentRenamed(let agent):
                try container.encode("agent_renamed", forKey: .outcome)
                try container.encode(agent, forKey: .agent)
            case .subscribed(let agent):
                try container.encode("subscribed", forKey: .outcome)
                try container.encode(agent, forKey: .agent)
            case .unsubscribed(let agent):
                try container.encode("unsubscribed", forKey: .outcome)
                try container.encode(agent, forKey: .agent)
            case .failed(let failure):
                try container.encode("error", forKey: .outcome)
                try container.encode(failure, forKey: .error)
            case .other: break
            }
        }
    }
}

/// A failed operation as the status line states it. Authentication and payment
/// failures are the two the app answers with a banner rather than a message.
public struct OpFailure: Sendable, Equatable, Codable {
    public var error: String
    public var message: String?
    public var authRequired: Bool
    public var subscriptionRequired: Bool
    public var body: JSONValue

    private enum Key: String, CodingKey {
        case error, message
        case authRequired = "auth_required"
        case subscriptionRequired = "subscription_required"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        self.error = try container.decode(String.self, forKey: .error)
        self.message = try container.decodeIfPresent(String.self, forKey: .message)
        self.authRequired = try container.decodeIfPresent(Bool.self, forKey: .authRequired) ?? false
        self.subscriptionRequired =
            try container.decodeIfPresent(Bool.self, forKey: .subscriptionRequired) ?? false
        self.body = try JSONValue(from: decoder)
    }

    public func encode(to encoder: any Encoder) throws {
        try body.encode(to: encoder)
    }
}

public struct DiffUpdate: Codable, Sendable, Equatable {
    public var agent: AgentId
    public var document: DiffDocument

    public init(agent: AgentId, document: DiffDocument) {
        self.agent = agent
        self.document = document
    }
}

public struct DiffDocument: Codable, Sendable, Equatable {
    public var numbering: DiffNumbering
    public var hunks: [DiffHunk]
    /// The source was a bounded head rather than the whole patch.
    public var truncated: Bool

    public init(numbering: DiffNumbering, hunks: [DiffHunk], truncated: Bool) {
        self.numbering = numbering
        self.hunks = hunks
        self.truncated = truncated
    }

    /// How many rows this patch adds and how many it takes away.
    ///
    /// Counted from the patch itself rather than taken from the agent card's
    /// last-turn totals: what the chip offers to open is this document, and a
    /// number that disagreed with the page it opens would be worse than no
    /// number. A truncated document is still counted honestly — it says what
    /// is in it, and the page it opens says it was cut short.
    public var insertions: Int { lines(startingWith: "+") }
    public var deletions: Int { lines(startingWith: "-") }

    /// Whether there is anything here to review at all. A document that
    /// arrived with no hunks, or with nothing but context, is not a change.
    public var isEmpty: Bool { insertions == 0 && deletions == 0 }

    private func lines(startingWith mark: Character) -> Int {
        hunks.reduce(0) { total, hunk in
            total + hunk.lines.count { $0.first == mark }
        }
    }
}

public enum DiffNumbering: String, Codable, Sendable, Equatable {
    case absolute
    case none
}

public struct DiffHunk: Codable, Sendable, Equatable {
    public var oldStart: UInt32
    public var newStart: UInt32
    public var header: String?
    /// Rows keep their leading ` `, `-` or `+`.
    public var lines: [String]

    private enum CodingKeys: String, CodingKey {
        case oldStart = "old_start"
        case newStart = "new_start"
        case header, lines
    }

    public init(oldStart: UInt32, newStart: UInt32, header: String?, lines: [String]) {
        self.oldStart = oldStart
        self.newStart = newStart
        self.header = header
        self.lines = lines
    }
}

public struct ConnectionUpdate: Codable, Sendable, Equatable {
    public var state: ConnectionState
    public var reason: String?

    public init(state: ConnectionState, reason: String? = nil) {
        self.state = state
        self.reason = reason
    }
}

public enum ConnectionState: String, Codable, Sendable, Equatable {
    case connecting
    case connected
    case disconnected
}
