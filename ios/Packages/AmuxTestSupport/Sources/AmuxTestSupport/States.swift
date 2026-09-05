import AmuxCore
import Foundation

/// The event batches a fixture applies. Stores only ever change by applying
/// events, fixtures included — a fixture that reached past them could put the
/// app into a state the bridge can never produce.
public enum States {
    public static func fleet(
        _ agents: [AgentCard] = Scenario.agents,
        hosts: [HostState] = Scenario.hosts,
        reconciled: Bool = true
    ) -> Event {
        .fleet(Fleet(epoch: 1, agents: agents, hosts: hosts, reconciled: reconciled))
    }

    public static func feed(
        _ entries: [FeedEntry], agent: AgentId = Scenario.focus
    ) -> Event {
        .feed(FeedUpdate(agent: agent, base: 0, append: entries, replace: [], evicted: 0))
    }

    public static func offline(_ reason: String = "relay unavailable") -> Event {
        .connection(ConnectionUpdate(state: .disconnected, reason: reason))
    }

    /// The fleet, its conversation, and whichever session state the screen is
    /// about — applied as one batch, then grouped once.
    @MainActor
    public static func open(
        _ bundle: StoreBundle,
        agents: [AgentCard] = Scenario.agents,
        hosts: [HostState] = Scenario.hosts,
        reconciled: Bool = true,
        unread: UnreadWeights = Scenario.mostlyRead,
        entries: [FeedEntry] = [],
        agent: AgentId = Scenario.focus,
        session: SessionSnapshot? = nil,
        changes: DiffDocument? = nil,
        extra: [Event] = []
    ) {
        bundle.fleet.unread = unread
        var batch: [Event] = [fleet(agents, hosts: hosts, reconciled: reconciled)]
        if !entries.isEmpty { batch.append(feed(entries, agent: agent)) }
        if let session { batch.append(.session(session)) }
        if let changes { batch.append(.diff(DiffUpdate(agent: agent, document: changes))) }
        batch.append(contentsOf: extra)
        bundle.apply(batch)
        bundle.fleet.refreshOrder(now: Scenario.now)
    }
}
