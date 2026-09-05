import Foundation

/// How long an agent may be quiet before it folds away.
public let fleetFoldAge: TimeInterval = 24 * 60 * 60

/// What this phone knows that the core cannot: whether you have opened an
/// agent since it last did anything.
///
/// The core reports a turn you finished reading an hour ago and one nobody has
/// looked at with the same words, and the difference is most of what an
/// ordering needs — so it is carried here rather than inferred from a state
/// the core never claimed.
public struct UnreadWeights: Sendable, Equatable {
    /// When each agent was last opened on this phone.
    public var lastOpened: [AgentId: Date]

    public init(lastOpened: [AgentId: Date] = [:]) {
        self.lastOpened = lastOpened
    }

    public func isUnread(_ card: AgentCard) -> Bool {
        guard let opened = lastOpened[card.id] else { return true }
        return opened < card.lastActivity
    }

    public mutating func opened(_ agent: AgentId, at time: Date) {
        lastOpened[agent] = time
    }
}

/// One agent as the home lists it.
public struct AgentRow: Sendable, Equatable, Identifiable {
    public var card: AgentCard
    /// Not opened since this agent last did anything.
    public var unread: Bool
    /// This row came from a reconciled fleet rather than the cold-start cache.
    public var confirmed: Bool

    public var id: AgentId { card.id }
    public var name: String { card.displayName }
    public var attention: Attention { card.attention }
    public var phase: AgentPhase { card.phase }
    public var lastActivity: Date { card.lastActivity }
    public var hostId: HostId { card.agent.hostId }
    public var workingDirectory: String { card.agent.workingDir }
    /// What the agent says it is doing, in its own words. Absent when it has
    /// not said.
    public var headline: String? { card.agent.workingOn?.text }
    public var outcome: TurnOutcome? { card.outcome }
    public var why: Why? { card.attention.why }

    public init(card: AgentCard, unread: Bool, confirmed: Bool) {
        self.card = card
        self.unread = unread
        self.confirmed = confirmed
    }

    public var needsYou: Bool {
        if case .needsYou = card.attention { return true }
        return false
    }

    /// How long this agent has been waiting on you.
    public func waiting(at now: Date) -> TimeInterval {
        now.timeIntervalSince(card.lastActivity)
    }

    /// How long ago it last did anything, in the shortest true unit: "14s",
    /// "2m", "5h", "2d". One unit only — a row is scanned, not read, and
    /// "2d 3h" is two numbers where one would do.
    public func age(at now: Date) -> String {
        let seconds = max(0, now.timeIntervalSince(card.lastActivity))
        switch seconds {
        case ..<60: return "\(Int(seconds))s"
        case ..<3600: return "\(Int(seconds / 60))m"
        case ..<86_400: return "\(Int(seconds / 3600))h"
        default: return "\(Int(seconds / 86_400))d"
        }
    }
}

public struct FleetSection: Sendable, Equatable, Identifiable {
    public enum Kind: String, Sendable, Equatable {
        /// Pinned: time alone will never float these back up.
        case needsYou
        /// One recency list. Running is not a rank.
        case everythingElse
        /// Quiet for a day. Named and reachable, never deleted from the screen.
        case older
    }

    public var kind: Kind
    public var title: String
    public var rows: [AgentRow]
    /// Shown as a single line naming what is inside it until it is opened.
    public var folded: Bool

    public var id: String { kind.rawValue }

    public init(kind: Kind, title: String, rows: [AgentRow], folded: Bool) {
        self.kind = kind
        self.title = title
        self.rows = rows
        self.folded = folded
    }
}

/// The home's ordering, as one pure function.
///
/// Two rules do the work. An agent that needs you is pinned, longest-waiting
/// first, because it cannot continue on its own and time will never raise it.
/// Everything else is one recency list, because running is not a rank — an
/// agent that stopped an hour ago can matter more than one mid-command — with
/// anything quiet for a day folded away rather than dropped. An unread agent
/// never folds: nobody has seen what it did yet.
public func fleetOrder(_ cards: [AgentCard], now: Date, unread: UnreadWeights) -> [FleetSection] {
    let rows = cards.map { AgentRow(card: $0, unread: unread.isUnread($0), confirmed: true) }

    // A tie between two identical waits still has to be an order, or the list
    // shuffles for no reason the user can see. Identity breaks it.
    let waiting = rows.filter(\.needsYou).sorted {
        ($0.lastActivity, $0.id.description) < ($1.lastActivity, $1.id.description)
    }
    let rest = rows.filter { !$0.needsYou }.sorted {
        ($1.lastActivity, $1.id.description) < ($0.lastActivity, $0.id.description)
    }
    let quiet = now.addingTimeInterval(-fleetFoldAge)
    let recent = rest.filter { $0.lastActivity > quiet || $0.unread }
    let older = rest.filter { $0.lastActivity <= quiet && !$0.unread }

    var sections: [FleetSection] = []
    if !waiting.isEmpty {
        sections.append(FleetSection(kind: .needsYou, title: "Needs you", rows: waiting, folded: false))
    }
    sections.append(FleetSection(
        kind: .everythingElse,
        title: waiting.isEmpty ? "Agents" : "Everything else",
        rows: recent,
        folded: false))
    if !older.isEmpty {
        sections.append(FleetSection(kind: .older, title: "Older", rows: older, folded: true))
    }
    return sections
}
