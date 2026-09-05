import Foundation
import Observation

/// The Agents home's state.
///
/// The list is placed once and then reconciled in place. A sync that confirms
/// what is already on screen must not move it: the user's thumb is already
/// travelling towards a row, and re-sorting under a sync is how a list steals
/// a tap. New agents are inserted where the ordering says they belong;
/// regrouping happens only when the screen asks for it.
@MainActor
@Observable
public final class FleetStore {
    public private(set) var rows: [AgentRow] = []
    public private(set) var sections: [FleetSection] = []
    /// The core's own word: this fleet is confirmed rather than remembered.
    public private(set) var reconciled = false
    public private(set) var epoch: UInt64 = 0
    public private(set) var hosts: [HostId: HostEntry] = [:]
    public private(set) var connection = ConnectionUpdate(state: .connecting)

    /// "3 need you · 12 agents", or the quiet form.
    public var subtitle: String {
        let waiting = rows.filter(\.needsYou).count
        guard waiting > 0 else {
            let running = rows.filter {
                if case .working = $0.attention { return true }
                return false
            }.count
            return "Nothing needs you · \(running) running"
        }
        return "\(waiting) need you · \(rows.count) agent\(rows.count == 1 ? "" : "s")"
    }

    /// The one line a quiet home shows when something is wrong, and nothing
    /// otherwise. Silence here means there is nothing to say.
    public var exceptions: String? {
        if connection.state == .disconnected {
            return connection.reason.map { "Offline · \($0)" } ?? "Offline"
        }
        let offline = hosts.values.filter { !$0.online }.map(\.name).sorted()
        switch offline.count {
        case 0: return nil
        case 1: return "\(offline[0]) offline"
        default: return "\(offline.count) hosts offline"
        }
    }

    /// Where the ordering was computed. The fold boundary stays where the user
    /// last saw it rather than sliding under them while they read.
    public private(set) var orderedAt: Date
    public var unread: UnreadWeights

    private var cards: [AgentId: AgentCard] = [:]
    private var order: [AgentId] = []
    private var placement: [AgentId: FleetSection.Kind] = [:]

    public init(now: Date = Date(), unread: UnreadWeights = UnreadWeights()) {
        self.orderedAt = now
        self.unread = unread
    }

    public func apply(_ event: Event) {
        switch event {
        case .fleet(let fleet):
            epoch = fleet.epoch
            reconciled = fleet.reconciled
            hosts = Dictionary(uniqueKeysWithValues: fleet.hosts.map { ($0.entry.id, $0.entry) })
            cards = Dictionary(fleet.agents.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
            reconcileOrder()
            rebuild()
        case .connection(let update):
            connection = update
        case .feed, .session, .opResult, .diff, .tokenRequest, .invariant:
            break
        }
    }

    /// Regroup deliberately — on a fresh appearance or a pull — never as a
    /// side effect of data arriving.
    public func refreshOrder(now: Date) {
        orderedAt = now
        order = []
        placement = [:]
        reconcileOrder()
        rebuild()
    }

    /// Record that this agent has been read, so it stops carrying unread
    /// weight the next time the list is grouped.
    public func opened(_ agent: AgentId, at time: Date = Date()) {
        unread.opened(agent, at: time)
        rebuild()
    }

    public func host(_ id: HostId) -> HostEntry? { hosts[id] }

    /// Places arrivals and forgets departures without moving what is already
    /// on screen.
    private func reconcileOrder() {
        let fresh = fleetOrder(Array(cards.values), now: orderedAt, unread: unread)
        var freshOrder: [AgentId] = []
        for section in fresh {
            for row in section.rows {
                freshOrder.append(row.id)
                if placement[row.id] == nil { placement[row.id] = section.kind }
            }
        }
        let known = Set(order)
        var placed = order.filter { cards[$0] != nil }
        for (index, id) in freshOrder.enumerated() where !known.contains(id) {
            // Insert after the nearest agent already on screen that the
            // ordering puts ahead of this one, so an arrival lands where it
            // belongs without disturbing its neighbours.
            let ahead = freshOrder[..<index].last { placed.contains($0) }
            if let ahead, let at = placed.firstIndex(of: ahead) {
                placed.insert(id, at: at + 1)
            } else {
                placed.insert(id, at: 0)
            }
        }
        order = placed
        placement = placement.filter { cards[$0.key] != nil }
    }

    private func rebuild() {
        rows = order.compactMap { id in
            guard let card = cards[id] else { return nil }
            return AgentRow(card: card, unread: unread.isUnread(card), confirmed: reconciled)
        }
        let waiting = rows.filter { placement[$0.id] == .needsYou }
        let recent = rows.filter { placement[$0.id] == .everythingElse }
        let older = rows.filter { placement[$0.id] == .older }
        var built: [FleetSection] = []
        if !waiting.isEmpty {
            built.append(FleetSection(kind: .needsYou, title: "Needs you", rows: waiting, folded: false))
        }
        built.append(FleetSection(
            kind: .everythingElse,
            title: waiting.isEmpty ? "Agents" : "Everything else",
            rows: recent,
            folded: false))
        if !older.isEmpty {
            built.append(FleetSection(kind: .older, title: "Older", rows: older, folded: true))
        }
        sections = built
    }
}
