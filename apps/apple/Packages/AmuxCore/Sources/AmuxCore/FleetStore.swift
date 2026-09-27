import AmuxValues
import Foundation
import Observation

/// How long a family has to be quiet before it folds into "Older".
public let fleetFoldAge: TimeInterval = 24 * 60 * 60

/// One agent as the home screen lists it: the runtime's card, where it sits
/// in its family, and whether this phone has seen its latest activity.
public struct AgentRow: Sendable, Equatable, Identifiable {
    public var row: FleetRow
    public var unread: Bool

    public init(row: FleetRow, unread: Bool) {
        self.row = row
        self.unread = unread
    }

    public var card: FleetCard { row.card }
    public var id: AgentKey { card.agent }
    public var name: String { card.name.isEmpty ? "agent" : card.name }
    public var attention: Attention { card.attention }
    public var familyAttention: Attention { card.familyAttention }
    /// Zero for a family's head.
    public var depth: Int { Int(row.depth) }
    public var children: Int { Int(card.children) }
    public var expanded: Bool { row.expanded }
    public var hostId: HostId? { card.agent.hostId }
    public var hostName: String { card.host }
    public var hostPresence: Presence { card.hostPresence }
    public var workingDirectory: String { card.cwd }
    /// What the agent says it is working on.
    public var headline: String? { card.workingOn.flatMap { $0.isEmpty ? nil : $0 } }
    public var lastActivity: Date { Date(milliseconds: card.lastActivityMs) }
    /// A kind this build knows how to open.
    public var readable: Bool { card.kind != .unspecified }
    public var needsYou: Bool { attention == .needsYou }
    /// Somebody in this agent's family is asking for the person.
    public var familyNeedsYou: Bool { familyAttention == .needsYou }

    public func age(at now: Date) -> String {
        Elapsed.spelled(max(0, now.timeIntervalSince(lastActivity)))
    }
}

extension Date {
    public init(milliseconds: Int64) {
        self.init(timeIntervalSince1970: TimeInterval(milliseconds) / 1000)
    }
}

/// A duration as a row has room for it.
public enum Elapsed {
    public static func spelled(_ seconds: TimeInterval) -> String {
        switch seconds {
        case ..<60: "\(Int(seconds))s"
        case ..<3600: "\(Int(seconds / 60))m"
        case ..<86_400: "\(Int(seconds / 3600))h"
        default: "\(Int(seconds / 86_400))d"
        }
    }
}

public struct FleetSection: Sendable, Equatable, Identifiable {
    public enum Kind: String, Sendable, Equatable {
        case agents
        case older
    }

    public var kind: Kind
    public var title: String
    public var rows: [AgentRow]
    public var folded: Bool
    public var id: String { kind.rawValue }

    public init(kind: Kind, title: String, rows: [AgentRow], folded: Bool) {
        self.kind = kind
        self.title = title
        self.rows = rows
        self.folded = folded
    }
}

/// The fleet as the home screen draws it.
///
/// The runtime ranks it — loudest family first, then the most recently
/// active — and this keeps that ranking still while somebody is looking at
/// it: a family that gets louder stays where it is until the list is pulled
/// or shown again, because a row that jumps as a thumb comes down on it opens
/// the wrong chat. A family that arrives lands where the ranking puts it,
/// among the families already on screen.
@MainActor
@Observable
public final class FleetStore {
    /// Every row on screen, families expanded where asked.
    public private(set) var rows: [AgentRow] = []
    public private(set) var sections: [FleetSection] = []
    /// Trusted hosts by id, this phone included.
    public private(set) var hosts: [HostId: HostView] = [:]
    /// The family heads whose members are listed under them.
    public private(set) var expanded: Set<[UInt8]> = []
    /// When the order was last taken from the runtime; ages are read
    /// against it so a list at rest does not tick.
    public private(set) var orderedAt: Date
    /// The relay link, as the account screens report it.
    public private(set) var relay: RelayLink = .off

    @ObservationIgnored private var ranked: [FleetRow] = []
    @ObservationIgnored private var order: [AgentKey] = []
    @ObservationIgnored private var seen: [AgentKey: Date] = [:]
    @ObservationIgnored private let launched: Date
    @ObservationIgnored private var markedFirstFrame = false

    public init(now: Date = Date()) {
        orderedAt = now
        launched = now
    }

    /// What the runtime lists now.
    public func show(_ rows: [FleetRow], hosts views: [HostView]) {
        ranked = rows
        hosts = Dictionary(
            views.filter(\.trusted).compactMap { view in view.id.map { ($0, view) } },
            uniquingKeysWith: { _, last in last })
        place()
        rebuild()
        if !markedFirstFrame && !self.rows.isEmpty {
            markedFirstFrame = true
            Signposts.emitWhenPresented(.firstCachedFrame)
        }
    }

    public func relay(_ link: RelayLink) {
        relay = link
    }

    /// Takes the runtime's ranking as it is now.
    public func refreshOrder(now: Date) {
        orderedAt = now
        order = []
        place()
        rebuild()
    }

    /// Somebody opened this agent's chat.
    public func opened(_ agent: AgentKey, at time: Date = Date()) {
        seen[agent] = time
        rebuild()
    }

    /// Lists or folds away a family's members; the owner reads the rows
    /// again with the new set.
    public func toggle(_ head: AgentKey) {
        if expanded.contains(head.agent) {
            expanded.remove(head.agent)
        } else {
            expanded.insert(head.agent)
        }
    }

    public func host(_ id: HostId?) -> HostView? {
        id.flatMap { hosts[$0] }
    }

    public func reach(ofHost id: HostId?) -> HostReach? {
        host(id)?.reach
    }

    public func row(_ agent: AgentKey) -> AgentRow? {
        rows.first { $0.id == agent }
    }

    public func name(of agent: AgentKey) -> String {
        row(agent)?.name ?? agent.description
    }

    /// Hosts other than this phone.
    public var machines: [HostView] {
        hosts.values.filter { !$0.local }
    }

    public var subtitle: String {
        let waiting = rows.filter(\.needsYou).count
        guard waiting > 0 else {
            let running = rows.filter { $0.attention == .working }.count
            return "Nothing needs you · \(running) running"
        }
        return "\(waiting) need you · \(rows.count) agent\(rows.count == 1 ? "" : "s")"
    }

    /// One line for what is wrong with reaching the fleet, if anything.
    public var exceptions: String? {
        switch relay {
        case .signInAgain: return "Sign in again to reach your hosts through the relay"
        case .updateRequired: return "Update amux to reach your hosts through the relay"
        case .off, .connecting, .connected, .retrying, .failed: break
        }
        let offline = machines.filter { $0.reach == .offline }.map(\.name).sorted()
        switch offline.count {
        case 0: return nil
        case 1: return "\(offline[0]) offline"
        default: return "\(offline.count) hosts offline"
        }
    }

    /// A host the relay would reach if this account paid for it.
    public var awayHost: String? {
        machines.filter { $0.reach == .away }.map(\.name).sorted().first
    }

    public var unreachableHost: String? {
        machines.filter { $0.reach == .offline }.map(\.name).sorted().first
    }

    private func isUnread(_ row: FleetRow) -> Bool {
        let activity = Date(milliseconds: row.card.lastActivityMs)
        return activity > (seen[row.card.agent] ?? launched)
    }

    /// Keeps the families on screen where they are and puts new ones where
    /// the ranking has them.
    private func place() {
        let heads = ranked.filter { $0.depth == 0 }.map(\.card.agent)
        let current = Set(heads)
        var placed = order.filter { current.contains($0) }
        let known = Set(placed)
        for (index, head) in heads.enumerated() where !known.contains(head) {
            let ahead = heads[..<index].last { placed.contains($0) }
            if let ahead, let at = placed.firstIndex(of: ahead) {
                placed.insert(head, at: at + 1)
            } else {
                placed.insert(head, at: 0)
            }
        }
        order = placed
    }

    private func rebuild() {
        // The runtime lists each family's members after their head.
        var families: [AgentKey: [FleetRow]] = [:]
        var head: AgentKey?
        for row in ranked {
            if row.depth == 0 { head = row.card.agent }
            if let head { families[head, default: []].append(row) }
        }
        let quiet = orderedAt.addingTimeInterval(-fleetFoldAge)
        var recent: [AgentRow] = []
        var older: [AgentRow] = []
        for key in order {
            guard let family = families[key], let first = family.first else { continue }
            let rows = family.map { AgentRow(row: $0, unread: isUnread($0)) }
            let resting = Date(milliseconds: first.card.lastActivityMs) <= quiet
                && first.card.familyAttention != .needsYou
                && !rows.contains(where: \.unread)
            if resting { older += rows } else { recent += rows }
        }
        rows = recent + older
        var built = [FleetSection(kind: .agents, title: "Agents", rows: recent, folded: false)]
        if !older.isEmpty {
            built.append(FleetSection(kind: .older, title: "Older", rows: older, folded: true))
        }
        sections = built
    }
}
