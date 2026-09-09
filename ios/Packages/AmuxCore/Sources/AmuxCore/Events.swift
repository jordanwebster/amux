import Foundation

// The shared Rust bridge projects the reducer's read surface as JSON events.
// These types are that surface in Swift, named as the core names it. Nothing
// here decides anything: a rule the core already owns — a send gate, an
// attention state, a fold — is read here, never recomputed.

/// One projected event. The bridge delivers them in arrays, in order.
public enum Event: Sendable, Equatable, Codable {
    case fleet(Fleet)
    /// Machines on this account's relay that this phone has not paired with.
    /// Never part of the fleet: an untrusted machine is an offer, not a host.
    case discovered([HostEntry])
    case feed(FeedUpdate)
    case session(SessionSnapshot)
    case opResult(OpResult)
    case diff(DiffUpdate)
    case connection(ConnectionUpdate)
    /// The bridge wants a fresh relay token for one signed-in account. The
    /// account is named because a phone signed in twice has two of them.
    case tokenRequest(requestId: UInt64, account: String)
    /// How many agents are waiting on an account that is not on screen, read
    /// from that account's own live subscription while the app is in front of
    /// somebody. The only thing an unselected account reports.
    case attention(account: String, waiting: Int)
    case invariant(detail: String)
    /// This phone's own identity and every machine it trusts. Apart from the
    /// fleet because it answers a different question: the fleet says what is
    /// answering, this says whose key has been granted access — including a
    /// machine that is away and would be trusted again the moment it returned.
    case devices(DeviceRoster)

    private enum Key: String, CodingKey {
        case fleet = "Fleet"
        case discovered = "Discovered"
        case feed = "Feed"
        case session = "Session"
        case opResult = "OpResult"
        case diff = "Diff"
        case connection = "Connection"
        case tokenRequest = "TokenRequest"
        case attention = "Attention"
        case invariant = "Invariant"
        case devices = "Devices"
    }

    private struct RequestId: Codable, Sendable, Equatable {
        var request_id: UInt64
        var account: String
    }

    private struct Waiting: Codable, Sendable, Equatable {
        var account: String
        var waiting: Int
    }

    private struct Discovery: Codable, Sendable, Equatable {
        var hosts: [HostEntry]
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
        case .discovered:
            self = .discovered(try container.decode(Discovery.self, forKey: key).hosts)
        case .feed: self = .feed(try container.decode(FeedUpdate.self, forKey: key))
        case .session: self = .session(try container.decode(SessionSnapshot.self, forKey: key))
        case .opResult: self = .opResult(try container.decode(OpResult.self, forKey: key))
        case .diff: self = .diff(try container.decode(DiffUpdate.self, forKey: key))
        case .connection: self = .connection(try container.decode(ConnectionUpdate.self, forKey: key))
        case .tokenRequest:
            let request = try container.decode(RequestId.self, forKey: key)
            self = .tokenRequest(requestId: request.request_id, account: request.account)
        case .attention:
            let waiting = try container.decode(Waiting.self, forKey: key)
            self = .attention(account: waiting.account, waiting: waiting.waiting)
        case .invariant:
            self = .invariant(detail: try container.decode(Detail.self, forKey: key).detail)
        case .devices: self = .devices(try container.decode(DeviceRoster.self, forKey: key))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .fleet(let value): try container.encode(value, forKey: .fleet)
        case .discovered(let hosts):
            try container.encode(Discovery(hosts: hosts), forKey: .discovered)
        case .feed(let value): try container.encode(value, forKey: .feed)
        case .session(let value): try container.encode(value, forKey: .session)
        case .opResult(let value): try container.encode(value, forKey: .opResult)
        case .diff(let value): try container.encode(value, forKey: .diff)
        case .connection(let value): try container.encode(value, forKey: .connection)
        case .tokenRequest(let id, let account):
            try container.encode(RequestId(request_id: id, account: account), forKey: .tokenRequest)
        case .attention(let account, let waiting):
            try container.encode(Waiting(account: account, waiting: waiting), forKey: .attention)
        case .invariant(let detail):
            try container.encode(Detail(detail: detail), forKey: .invariant)
        case .devices(let roster): try container.encode(roster, forKey: .devices)
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
    /// A provider this build has no case for, kept under the name the host
    /// used for it.
    ///
    /// Refusing to decode it would throw away the whole fleet the moment one
    /// machine on the account runs a newer amux, which turns one agent nobody
    /// can read into a phone that shows nothing at all. So it arrives, it is
    /// listed, and `AgentRow.readable` is what stops it being offered to open.
    case unknown(String)

    private enum Key: String, CodingKey { case kind, driver }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "claude": self = .claude(driver: try container.decode(ClaudeDriver.self, forKey: .driver))
        case "codex": self = .codex
        case "test_agent": self = .testAgent
        case let other: self = .unknown(other)
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
        case .unknown(let name): try container.encode(name, forKey: .kind)
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
    /// What kind of machine it is, in its own words: the operating system its
    /// daemon announced in the handshake. Absent for a host nothing has been
    /// adjacent to, and for one running a build from before hosts said so.
    public var platform: String?

    private enum CodingKeys: String, CodingKey {
        case id, name, online, version, capabilities, platform
        case trustStatus = "trust_status"
        case lastDialError = "last_dial_error"
    }

    public init(
        id: HostId, name: String, online: Bool, version: String? = nil,
        capabilities: JSONValue? = nil, trustStatus: HostTrustStatus = .trusted,
        lastDialError: String? = nil, platform: String? = nil
    ) {
        self.id = id
        self.name = name
        self.online = online
        self.version = version
        self.capabilities = capabilities
        self.trustStatus = trustStatus
        self.lastDialError = lastDialError
        self.platform = platform
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
    case claudeSdk(ClaudeSdkSendGate)
    case codex(CodexSendGate)
    case unavailable

    private enum Key: String, CodingKey { case layer, value }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(try container.decode(ClaudePtySendGate.self, forKey: .value))
        case "claude_sdk": self = .claudeSdk(try container.decode(ClaudeSdkSendGate.self, forKey: .value))
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
        case .claudeSdk(let gate):
            try container.encode("claude_sdk", forKey: .layer)
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
        case .claudeSdk(let gate): gate == .ready
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
    case claudeSdk(JSONValue)
    case codex(JSONValue)
    case unavailable

    private enum Key: String, CodingKey { case layer, value }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(try container.decode(JSONValue.self, forKey: .value))
        case "claude_sdk": self = .claudeSdk(try container.decode(JSONValue.self, forKey: .value))
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
        case .claudeSdk(let value):
            try container.encode("claude_sdk", forKey: .layer)
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
        case .claudePty(let value), .claudeSdk(let value), .codex(let value): value["phase"]?.stringValue
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
        case claudeSdk = "claude_sdk"
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
/// them. SDK facts are kept separate from terminal session facts.
public enum SessionFacts: Sendable, Equatable, Codable {
    case claudePty(JSONValue)
    case codex(JSONValue)
    case claudeSdk(JSONValue)
    case unavailable

    private enum Key: String, CodingKey { case layer }

    public init(from decoder: any Decoder) throws {
        let tagged = try decoder.container(keyedBy: Key.self)
        let body = try JSONValue(from: decoder)
        switch try tagged.decode(String.self, forKey: .layer) {
        case "claude_pty": self = .claudePty(body)
        case "codex": self = .codex(body)
        case "claude_sdk": self = .claudeSdk(body)
        case "unavailable": self = .unavailable
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown facts layer \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .claudePty(let body), .claudeSdk(let body), .codex(let body):
            try body.encode(to: encoder)
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

    /// Why a change would be refused, in the core's own words, or nothing
    /// where it would be taken. Spelled here to match `provider::SettingsGate`
    /// in the shared crate: the phone must not tell a person something
    /// different about a refusal than the terminal does.
    public var refusal: String? {
        switch self {
        case .ready: nil
        case .ptySettingsUnavailable:
            "model, effort and preset changes are unavailable for Claude PTY sessions"
        case .unavailable: "provider settings unavailable for this agent"
        // The layer's own gate word, said rather than translated: what a
        // session refuses for is its vocabulary and this build does not carry
        // a second set of sentences for it.
        case .codex(let reason): "settings unavailable: codex reports \(spelled(reason.rawValue))"
        case .claudeSdk(let reason):
            "settings unavailable: claude reports \(spelled(reason.rawValue))"
        }
    }

    private func spelled(_ gate: String) -> String {
        gate.replacingOccurrences(of: "_", with: " ")
    }
}

/// The one message this agent is holding until its turn ends.
///
/// One per agent, and the core holds it rather than the phone: a phone that
/// goes to sleep, loses its network or is put away must not take the message
/// with it, and only the machine the agent runs on can be sure the turn ended
/// exactly once.
public struct QueuedMessage: Codable, Sendable, Equatable {
    public var draft: HeldDraft
    public var heldAt: Date
    public var delivery: QueueDelivery

    private enum CodingKeys: String, CodingKey {
        case draft
        case heldAt = "held_at"
        case delivery
    }

    public init(draft: HeldDraft, heldAt: Date, delivery: QueueDelivery) {
        self.draft = draft
        self.heldAt = heldAt
        self.delivery = delivery
    }

    /// What the message says, which is the only thing a strip has room for
    /// and the whole of what goes back into the field when it is unqueued.
    public var text: String { draft.text }

    /// Whether it can still be changed. A message already on its way cannot:
    /// the core refuses to replace or cancel one, and offering the gesture
    /// anyway would be a control whose only outcome is a refusal.
    public var changeable: Bool {
        if case .sending = delivery { return false }
        return true
    }
}

/// A held message as the core spells it: the segments it is made of and the
/// artifacts that travel with it.
///
/// The same shape as a draft on its way out, because it is the same draft —
/// held rather than sent. Mirrored here rather than kept as loose JSON so a
/// change to the shared vocabulary fails in the schema test instead of at a
/// strip that quietly stops saying anything.
public struct HeldDraft: Codable, Sendable, Equatable {
    public var segments: [HeldSegment]
    public var attachments: [DraftAttachment]

    public init(segments: [HeldSegment], attachments: [DraftAttachment] = []) {
        self.segments = segments
        self.attachments = attachments
    }

    /// The message read back as one string, folded the way the shared library
    /// folds it: a command token reads as the command it is, and everything
    /// else is what was written.
    public var text: String {
        segments.map { segment in
            switch segment {
            case .text(let text): text
            case .command(let name): "/\(name)"
            }
        }.joined()
    }
}

/// One piece of a held message.
public enum HeldSegment: Codable, Sendable, Equatable {
    case text(String)
    /// The command the message is, which travels as its own segment rather
    /// than as typed characters.
    case command(name: String)

    private enum Key: String, CodingKey { case segment, text, name }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .segment) {
        case "text": self = .text(try container.decode(String.self, forKey: .text))
        case "command_token": self = .command(name: try container.decode(String.self, forKey: .name))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath,
                debugDescription: "unknown draft segment \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .text(let text):
            try container.encode("text", forKey: .segment)
            try container.encode(text, forKey: .text)
        case .command(let name):
            try container.encode("command_token", forKey: .segment)
            try container.encode(name, forKey: .name)
        }
    }
}

/// Where a held message has got to.
public enum QueueDelivery: Codable, Sendable, Equatable {
    /// Waiting for the turn to end.
    case held
    /// The turn ended and it is on its way.
    case sending(op: OpId)
    /// It went and was refused. The core keeps it rather than dropping it, so
    /// what was written is not lost to a failure.
    case failed(OpFailure)

    private enum Key: String, CodingKey { case state, op, error }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .state) {
        case "held": self = .held
        case "sending": self = .sending(op: try container.decode(OpId.self, forKey: .op))
        case "failed": self = .failed(try container.decode(OpFailure.self, forKey: .error))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath,
                debugDescription: "unknown queue delivery \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .held: try container.encode("held", forKey: .state)
        case .sending(let op):
            try container.encode("sending", forKey: .state)
            try container.encode(op, forKey: .op)
        case .failed(let error):
            try container.encode("failed", forKey: .state)
            try container.encode(error, forKey: .error)
        }
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
    /// A picked file is stored on the agent's host. What comes back is what a
    /// token names; the bytes stay where they were sent.
    case attachmentStored(DraftAttachment)
    case queueRemoved
    case subscribed(agent: AgentId)
    case unsubscribed(agent: AgentId)
    /// A pairing secret authenticated. The machine has said who it is and
    /// nothing has been trusted; `pending` is what the answer names.
    case pairingPending(PendingPeer)
    /// Trust written, on this phone and on the machine.
    case paired(host: HostId, name: String)
    /// Abandoned by the person. Nothing was written anywhere.
    case pairingAbandoned
    /// The secret did not authenticate. Mistyped, already used, expired and
    /// never issued all arrive here, in the same shape and with nothing else
    /// said, because telling them apart is what guessing codes would need.
    case pairingRefused
    /// The attempt an answer names is not one the runtime is holding: it was
    /// answered already, or the app has been restarted since.
    case pairingLost
    /// The runtime is reading this account now. It arrives after the switch
    /// on screen, and everything projected before it answered for the account
    /// that was on screen until then.
    case selected(account: String)
    /// The connection has been asked to dial now. Whether that shortened
    /// anything is the connection's to decide; whether the relay answers
    /// arrives as a connection state rather than as an answer to this.
    case retryRequested
    /// Trust withdrawn from a machine: this phone stopped holding its key and
    /// closed every link it had to it.
    case revoked(host: HostId, name: String)
    /// The machine named is not one this phone trusts, so there was nothing to
    /// withdraw — a second tap, or a screen that had gone stale.
    case revokeRefused
    /// What a machine has to offer as a working directory: what it was used in
    /// recently, the repositories under its roots, and the roots themselves.
    /// The three are kept apart because a directory somebody worked in
    /// yesterday is a different kind of suggestion from one that merely exists.
    case repositories(host: HostId, recent: [Project], repositories: [Project], roots: [String])
    /// The machine could not be asked, or would not answer. It carries no
    /// detail: a screen that cannot list directories offers a typed path, and
    /// which call failed does not change that.
    case repositoriesUnavailable(host: HostId)
    case failed(OpFailure)
    case other(outcome: String, body: JSONValue)

    private enum Key: String, CodingKey {
        case outcome, agent, error, attachment, host, name, account
        case recent, repositories, roots
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let outcome = try container.decode(String.self, forKey: .outcome)
        switch outcome {
        case "input_sent": self = .inputSent
        case "agent_created": self = .agentCreated(try container.decode(Agent.self, forKey: .agent))
        case "agent_renamed": self = .agentRenamed(try container.decode(Agent.self, forKey: .agent))
        case "agent_deleted": self = .agentDeleted
        case "attachment_stored":
            self = .attachmentStored(try container.decode(DraftAttachment.self, forKey: .attachment))
        case "queue_removed": self = .queueRemoved
        case "subscribed": self = .subscribed(agent: try container.decode(AgentId.self, forKey: .agent))
        case "unsubscribed": self = .unsubscribed(agent: try container.decode(AgentId.self, forKey: .agent))
        case "pairing_pending": self = .pairingPending(try PendingPeer(from: decoder))
        case "paired":
            self = .paired(
                host: try container.decode(HostId.self, forKey: .host),
                name: try container.decode(String.self, forKey: .name))
        case "pairing_abandoned": self = .pairingAbandoned
        case "pairing_refused": self = .pairingRefused
        case "pairing_lost": self = .pairingLost
        case "selected":
            self = .selected(account: try container.decode(String.self, forKey: .account))
        case "retry_requested": self = .retryRequested
        case "revoked":
            self = .revoked(
                host: try container.decode(HostId.self, forKey: .host),
                name: try container.decode(String.self, forKey: .name))
        case "revoke_refused": self = .revokeRefused
        case "repositories":
            self = .repositories(
                host: try container.decode(HostId.self, forKey: .host),
                recent: try container.decode([Project].self, forKey: .recent),
                repositories: try container.decode([Project].self, forKey: .repositories),
                roots: try container.decode([String].self, forKey: .roots))
        case "repositories_unavailable":
            self = .repositoriesUnavailable(host: try container.decode(HostId.self, forKey: .host))
        case "error": self = .failed(try container.decode(OpFailure.self, forKey: .error))
        default: self = .other(outcome: outcome, body: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .other(_, let body):
            try body.encode(to: encoder)
        case .pairingPending(let pending):
            try pending.encode(to: encoder)
            var container = encoder.container(keyedBy: Key.self)
            try container.encode("pairing_pending", forKey: .outcome)
        default:
            var container = encoder.container(keyedBy: Key.self)
            switch self {
            case .inputSent: try container.encode("input_sent", forKey: .outcome)
            case .agentDeleted: try container.encode("agent_deleted", forKey: .outcome)
            case .attachmentStored(let attachment):
                try container.encode("attachment_stored", forKey: .outcome)
                try container.encode(attachment, forKey: .attachment)
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
            case .paired(let host, let name):
                try container.encode("paired", forKey: .outcome)
                try container.encode(host, forKey: .host)
                try container.encode(name, forKey: .name)
            case .pairingAbandoned: try container.encode("pairing_abandoned", forKey: .outcome)
            case .pairingRefused: try container.encode("pairing_refused", forKey: .outcome)
            case .pairingLost: try container.encode("pairing_lost", forKey: .outcome)
            case .selected(let account):
                try container.encode("selected", forKey: .outcome)
                try container.encode(account, forKey: .account)
            case .retryRequested: try container.encode("retry_requested", forKey: .outcome)
            case .revoked(let host, let name):
                try container.encode("revoked", forKey: .outcome)
                try container.encode(host, forKey: .host)
                try container.encode(name, forKey: .name)
            case .revokeRefused: try container.encode("revoke_refused", forKey: .outcome)
            case .repositories(let host, let recent, let repositories, let roots):
                try container.encode("repositories", forKey: .outcome)
                try container.encode(host, forKey: .host)
                try container.encode(recent, forKey: .recent)
                try container.encode(repositories, forKey: .repositories)
                try container.encode(roots, forKey: .roots)
            case .repositoriesUnavailable(let host):
                try container.encode("repositories_unavailable", forKey: .outcome)
                try container.encode(host, forKey: .host)
            case .pairingPending: break
            case .failed(let failure):
                try container.encode("error", forKey: .outcome)
                try container.encode(failure, forKey: .error)
            case .other: break
            }
        }
    }
}

/// One directory a new agent could be started in, as the machine that holds it
/// describes it.
public struct Project: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var name: String
    /// When an agent last ran here, or nothing where none has. It is what makes
    /// a directory recent rather than merely present.
    public var lastUsed: Date?

    public var id: String { path }

    private enum CodingKeys: String, CodingKey {
        case path, name
        case lastUsed = "last_used"
    }

    public init(path: String, name: String, lastUsed: Date? = nil) {
        self.path = path
        self.name = name
        self.lastUsed = lastUsed
    }
}

/// What this phone is and which machines it trusts.
///
/// The list is the trust store, not the inventory: a machine that is away is
/// still in it, because the key it was granted is still good and the moment it
/// answers again it will be let in. That is exactly why the list exists — it
/// is what somebody reads before withdrawing one.
public struct DeviceRoster: Codable, Sendable, Equatable {
    public var identity: DeviceIdentity
    public var devices: [PairedDevice]

    public init(identity: DeviceIdentity, devices: [PairedDevice]) {
        self.identity = identity
        self.devices = devices
    }
}

/// This phone, as the machines it pairs with see it.
public struct DeviceIdentity: Codable, Sendable, Equatable {
    public var host: HostId
    public var name: String
    /// The fingerprint of this phone's key, in the spelling the machine on the
    /// other side of a pairing shows for its own.
    public var fingerprint: String

    public init(host: HostId, name: String, fingerprint: String) {
        self.host = host
        self.name = name
        self.fingerprint = fingerprint
    }
}

/// One machine this phone holds a key for.
public struct PairedDevice: Codable, Sendable, Equatable, Identifiable {
    public var host: HostId
    public var name: String
    public var fingerprint: String
    public var pairedAt: Date

    public var id: HostId { host }

    private enum CodingKeys: String, CodingKey {
        case host, name, fingerprint
        case pairedAt = "paired_at"
    }

    public init(host: HostId, name: String, fingerprint: String, pairedAt: Date) {
        self.host = host
        self.name = name
        self.fingerprint = fingerprint
        self.pairedAt = pairedAt
    }
}

/// A machine that has authenticated a pairing secret and is waiting to be
/// trusted or turned away.
///
/// It is what a person looks at before deciding: the name the machine calls
/// itself and the fingerprint of the key this phone would be trusting. The
/// capability that would actually commit the trust is not here — it stays in
/// the runtime, and `pending` is only the handle an answer names — so nothing
/// that reads or logs this value can pair with anybody.
public struct PendingPeer: Codable, Sendable, Equatable, Identifiable {
    public var pending: String
    public var host: HostId
    public var name: String
    public var fingerprint: String
    /// When the machine stops holding this attempt open.
    public var expiresAt: Date

    public var id: String { pending }

    private enum CodingKeys: String, CodingKey {
        case pending, host, name, fingerprint
        case expiresAt = "expires_at"
    }

    public init(
        pending: String, host: HostId, name: String, fingerprint: String, expiresAt: Date
    ) {
        self.pending = pending
        self.host = host
        self.name = name
        self.fingerprint = fingerprint
        self.expiresAt = expiresAt
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

/// A patch frozen for review, and what it was taken against.
public struct DiffUpdate: Codable, Sendable, Equatable {
    public var agent: AgentId
    /// The artifact this patch is. A review sent later names it, so the person
    /// who receives the review can open the same diff that was read.
    public var diff: ArtifactId
    public var document: ReviewDocument

    public init(agent: AgentId, diff: ArtifactId, document: ReviewDocument) {
        self.agent = agent
        self.diff = diff
        self.document = document
    }
}

/// A multi-file patch, already split, numbered and identified.
///
/// The phone does no diff parsing of its own. Splitting a patch into files,
/// numbering its rows on both sides and checking the arithmetic against what
/// the host said changed all happen once, in the shared core, and arrive here
/// finished — because a page that guessed a row number would put a comment on
/// the wrong line.
public struct ReviewDocument: Codable, Sendable, Equatable {
    public var files: [ReviewFile]
    /// The repository state this was computed from.
    public var identity: BaseIdentity

    public init(files: [ReviewFile], identity: BaseIdentity) {
        self.files = files
        self.identity = identity
    }

    /// How many rows this patch adds and how many it takes away.
    ///
    /// Counted from the patch itself rather than taken from the agent card's
    /// last-turn totals: what the chip offers to open is this document, and a
    /// number that disagreed with the page it opens would be worse than no
    /// number.
    public var insertions: Int { files.reduce(0) { $0 + Int($1.added) } }
    public var deletions: Int { files.reduce(0) { $0 + Int($1.removed) } }

    /// Whether there is anything here to review at all.
    public var isEmpty: Bool { insertions == 0 && deletions == 0 }
}

/// One file's share of a patch: its magnitudes, its numbered rows, and where
/// each hunk begins in them.
public struct ReviewFile: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var added: UInt32
    public var removed: UInt32
    public var rows: [DiffRow]
    /// The index of each hunk's first real row. A reader scrubbing through a
    /// file lands on these rather than on the blank between two hunks.
    public var hunkStarts: [Int]

    private enum CodingKeys: String, CodingKey {
        case path, added, removed, rows
        case hunkStarts = "hunk_starts"
    }

    public init(
        path: String, added: UInt32, removed: UInt32, rows: [DiffRow], hunkStarts: [Int]
    ) {
        self.path = path
        self.added = added
        self.removed = removed
        self.rows = rows
        self.hunkStarts = hunkStarts
    }

    /// A patch names a file once, so its path is its identity.
    public var id: String { path }
}

/// One row with independent old-file and new-file positions.
public struct DiffRow: Codable, Sendable, Equatable {
    public var old: UInt32?
    public var new: UInt32?
    public var kind: Kind
    /// The row as the patch wrote it, leading ` `, `-` or `+` included.
    public var text: String

    public enum Kind: String, Codable, Sendable, Equatable {
        /// The break between two hunks. It carries no text of its own: what it
        /// says — that the next line is not the one after the last — the row
        /// numbers on either side of it already say.
        case boundary
        /// A line inside a hunk that is not content, such as the note that a
        /// file has no newline at its end. It is a fact about the patch.
        case note
        case context
        case added
        case removed
    }

    public init(old: UInt32?, new: UInt32?, kind: Kind, text: String) {
        self.old = old
        self.new = new
        self.kind = kind
        self.text = text
    }
}

/// The repository state a patch was computed from, kept whole so a review sent
/// later says exactly what it was written against.
public struct BaseIdentity: Codable, Sendable, Equatable {
    public var base: DiffBase
    public var head: String
    public var mergeBase: String?
    /// The blob each reviewed file was at, as pairs of path and object name.
    public var blobs: [[String]]

    private enum CodingKeys: String, CodingKey {
        case base, head, blobs
        case mergeBase = "merge_base"
    }

    public init(base: DiffBase, head: String, mergeBase: String?, blobs: [[String]]) {
        self.base = base
        self.head = head
        self.mergeBase = mergeBase
        self.blobs = blobs
    }
}

/// What a diff was taken against.
public enum DiffBase: Sendable, Equatable, Codable {
    case workingTree
    case branch(String)

    private enum Key: String, CodingKey { case kind, base }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "working_tree": self = .workingTree
        case "branch": self = .branch(try container.decode(String.self, forKey: .base))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown diff base \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .workingTree:
            try container.encode("working_tree", forKey: .kind)
        case .branch(let base):
            try container.encode("branch", forKey: .kind)
            try container.encode(base, forKey: .base)
        }
    }

    /// How a review header spells this base.
    public var spelling: String {
        switch self {
        case .workingTree: ""
        case .branch(let base): "branch:\(base)"
        }
    }
}

public struct ConnectionUpdate: Codable, Sendable, Equatable {
    public var state: ConnectionState
    public var reason: OfflineReason?

    public init(state: ConnectionState, reason: OfflineReason? = nil) {
        self.state = state
        self.reason = reason
    }
}

/// Why the phone is offline, in the kinds the core distinguishes.
///
/// The core sends a kind, never a message: a transport error is a sentence
/// written for whoever is reading a log, and putting it above the fleet tells
/// a person nothing they can act on. The words are written here, once, where
/// the rest of the screen's copy lives.
public enum OfflineReason: String, Codable, Sendable, Equatable {
    /// Nothing answered the dial: no network, no route, or refused.
    case unreachable
    /// The relay answered and would not take this device.
    case rejected
    /// Dialled, and nothing came back in time.
    case timedOut = "timed_out"
    /// A connection that was up has ended; another attempt is coming.
    case ended
    /// The client itself stopped, so nothing is trying.
    case stopped
    /// The app is not in front of anybody, so it holds no connection.
    case suspended

    /// The one line a home shows above its rows, in full.
    public var sentence: String {
        switch self {
        case .unreachable: "Offline · can't reach amux — check your connection"
        case .rejected: "Offline · sign in again to reconnect"
        case .timedOut: "Offline · amux isn't answering — trying again"
        case .ended: "Offline · reconnecting"
        case .stopped: "Offline · amux stopped — reopen the app"
        case .suspended: "Offline · reconnecting"
        }
    }
}

public enum ConnectionState: String, Codable, Sendable, Equatable {
    case connecting
    case connected
    case disconnected
}
