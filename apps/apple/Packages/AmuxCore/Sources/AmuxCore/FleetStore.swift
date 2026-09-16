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
    /// How many confirmed fleets have arrived since the app started.
    ///
    /// `reconciled` above is sticky: once a fleet has been confirmed it stays
    /// confirmed, through an outage and through the app being put away, because
    /// what it describes is the rows on screen and they are still confirmed
    /// rows. So it cannot answer whether the app reconciled *again* after being
    /// picked up — it was already true before it was put down. This moves once
    /// per confirmation, which is the question a lifecycle audit is asking.
    public private(set) var reconciliations = 0
    public private(set) var epoch: UInt64 = 0
    public private(set) var hosts: [HostId: HostEntry] = [:]
    public private(set) var connection = ConnectionUpdate(state: .connecting)
    /// Whether anybody is signed in and what the relay will carry for them, as
    /// the link itself reports it.
    ///
    /// The home reads it for one question only: whether an account is worth
    /// offering. Read from the link rather than asked of the account service,
    /// because what decides whether a machine can be used is the credential
    /// the relay holds.
    public private(set) var cloud: CloudState = .signedOut

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
            return connection.reason?.sentence ?? "Offline"
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

    /// Marked once: the first frame a person can read the fleet from is the
    /// number the cold-start budget is about, and it happens once per launch.
    private var markedFirstFrame = false
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
            // A fleet nobody has confirmed that names no agents is a runtime
            // that has not reached anything yet, not a phone whose agents are
            // gone. It happens on every launch that cannot get to the relay,
            // and taking it at its word would empty the screen a moment after
            // the cache had filled it. What such a runtime does know is which
            // machines this device is paired with, so those are taken and the
            // remembered rows are kept until something answers for them.
            let unreached = !fleet.reconciled && fleet.agents.isEmpty && !cards.isEmpty
            let agents = unreached ? Array(cards.values) : fleet.agents
            // Check the existing indexed content before allocating its
            // replacements. Confirmation of an unchanged cached fleet is the
            // common reconnect path, and dictionary construction was most of
            // its fixed cost.
            let visibleContentChanged = fleet.hosts.count != hosts.count
                || agents.count != cards.count
                || fleet.hosts.contains { hosts[$0.id] != $0.entry }
                || agents.contains { cards[$0.id] != $0 }
            epoch = fleet.epoch
            let wasReconciled = reconciled
            reconciled = fleet.reconciled
            if fleet.reconciled { reconciliations += 1 }
            // A host commonly confirms exactly the fleet that was restored
            // from cache. The confirmation changes provenance, not anything
            // drawn in the list, so do not rebuild every row and notify every
            // observer as though the list changed.
            if visibleContentChanged {
                hosts = Dictionary(
                    uniqueKeysWithValues: fleet.hosts.map { ($0.entry.id, $0.entry) })
                if !unreached {
                    cards = Dictionary(
                        fleet.agents.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
                }
                reconcileOrder()
                rebuild()
            }
            if !wasReconciled && reconciled { Signposts.emit(.reconciled) }
            if !markedFirstFrame && !rows.isEmpty {
                markedFirstFrame = true
                Signposts.emitWhenPresented(.firstCachedFrame)
            }
        case .connection(let update):
            let wasConnected = connection.state == .connected
            connection = update
            if !wasConnected && update.state == .connected { Signposts.emit(.streamConnected) }
        case .cloudState(let state):
            cloud = state
        case .feed, .session, .opResult, .diff, .discovered, .tokenRequest, .invariant, .devices,
             .attention, .unreadable:
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

    /// Takes in an agent the machine has just started.
    ///
    /// The machine has already answered — the agent exists, on the machine
    /// that named it — so this is not an optimistic row: it is a fact this
    /// phone was told directly rather than through the next inventory. Putting
    /// it in now is what lets the conversation it opens name where it runs,
    /// instead of showing a blank line until the fleet catches up. The next
    /// inventory replaces it with the machine's own account of it.
    public func created(_ agent: Agent) {
        guard cards[agent.id] == nil else { return }
        cards[agent.id] = AgentCard(
            agent: agent, displayName: agent.name ?? agent.command,
            attention: .idle, phase: .running, lastActivity: agent.createdAt)
        reconcileOrder()
        rebuild()
    }

    public func host(_ id: HostId) -> HostEntry? { hosts[id] }

    /// Where a machine is, under the rule the Hosts tab groups by.
    public func reach(of host: HostEntry) -> HostReach { host.reach(tier: cloud.tier) }

    /// Where the machine an agent runs on is, or nothing where the fleet has
    /// not named that machine yet.
    public func reach(ofHost id: HostId?) -> HostReach? {
        guard let id, let host = hosts[id] else { return nil }
        return reach(of: host)
    }

    /// The machine the relay can see and this account may not tunnel to, or
    /// nothing where there is none.
    ///
    /// Named rather than counted: the one line a home is allowed above the
    /// list says which machine, and a count would leave a reader looking for
    /// it. Alphabetical where there are several, so two runs say the same
    /// thing.
    public var awayHost: String? {
        hosts.values.filter { reach(of: $0) == .away }.map(\.name).sorted().first
    }

    /// The machine no route reaches, on the same terms.
    public var unreachableHost: String? {
        hosts.values.filter { reach(of: $0) == .offline }.map(\.name).sorted().first
    }

    /// What this agent is called, for a screen that has an identity and needs
    /// a name.
    ///
    /// The fleet owns names, so every screen that shows one asks here. An
    /// agent the fleet has not heard of yet is named with the identity it was
    /// reached by, which is at least true — a blank where a name belongs reads
    /// as something that failed to load.
    public func name(of agent: AgentId) -> String {
        rows.first { $0.id == agent }?.name ?? agent.description
    }

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
            return AgentRow(card: card, unread: unread.isUnread(card))
        }
        let recent = rows.filter { placement[$0.id] == .agents }
        let older = rows.filter { placement[$0.id] == .older }
        var built = [FleetSection(kind: .agents, title: "Agents", rows: recent, folded: false)]
        if !older.isEmpty {
            built.append(FleetSection(kind: .older, title: "Older", rows: older, folded: true))
        }
        sections = built
    }
}
