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
        changes: ReviewDocument? = nil,
        extra: [Event] = []
    ) {
        bundle.fleet.unread = unread
        var batch: [Event] = [fleet(agents, hosts: hosts, reconciled: reconciled)]
        if !entries.isEmpty { batch.append(feed(entries, agent: agent)) }
        if let session { batch.append(.session(session)) }
        if let changes {
            batch.append(.diff(DiffUpdate(
                agent: agent, diff: Transcript.changesArtifact, document: changes)))
        }
        batch.append(contentsOf: extra)
        // A fixture's operation results belong to the agent it is about: the
        // conversation is told it dispatched them, exactly as the send path
        // tells it in the app.
        for case .opResult(let result) in batch {
            bundle.conversation(agent).dispatched(result.op)
        }
        bundle.apply(batch)
        bundle.fleet.refreshOrder(now: Scenario.now)
    }

    /// A machine that was answering when the phone last heard from it and is
    /// not answering now.
    ///
    /// Played as two snapshots with time between them, because that is the
    /// only way a phone ever learns when a machine went away: presence is a
    /// boolean, and nothing in it says when it changed. The clock is wound
    /// back for the second snapshot and put forward again, so the departure
    /// sits in the past of the fixed moment every screen is photographed at.
    @MainActor
    public static func lostHost(
        _ bundle: StoreBundle, _ id: HostId, minutesAgo: Double,
        agents: [AgentCard] = Scenario.agents
    ) {
        let after = Scenario.reachableHosts.map { host -> HostState in
            guard host.entry.id == id else { return host }
            var lost = host
            lost.entry.online = false
            lost.entry.lastDialError = "no route to host"
            return lost
        }
        Scenario.reading = Scenario.now.addingTimeInterval(-60 * minutesAgo)
        bundle.apply([fleet(agents, hosts: after)])
        Scenario.reading = Scenario.now
    }

    /// What the runtime says this phone is and whose keys it holds.
    @MainActor
    public static func trusted(_ bundle: StoreBundle, _ roster: DeviceRoster = Scenario.roster) {
        bundle.apply([.devices(roster)])
    }

    /// A machine on the network this phone has not paired with.
    @MainActor
    public static func offering(_ bundle: StoreBundle, _ hosts: [HostEntry] = [Scenario.unpaired]) {
        bundle.apply([.discovered(hosts)])
    }

    /// An invitation the machine has answered: authenticated, waiting on a
    /// person, nothing trusted anywhere.
    ///
    /// Played as the operation and its result rather than written into the
    /// store, because that is the only way the app ever reaches this state —
    /// a pending peer is what a machine says about itself, and a fixture that
    /// set the phase directly could show a confirmation the protocol could
    /// never produce.
    @MainActor
    public static func offered(_ bundle: StoreBundle, _ peer: PendingPeer = Scenario.offered) {
        let op = OpId(UUID())
        bundle.pairing.open()
        bundle.pairing.awaits(op)
        bundle.apply([.opResult(OpResult(op: op, outcome: .pairingPending(peer)))])
    }

    /// The conversation whose machine went away mid-turn.
    ///
    /// Studio stops answering while a turn is running: the session's stream
    /// closes as unreachable, the connection says which machine it lost, and
    /// the feed keeps every row that was already true.
    @MainActor
    public static func hostLost(_ bundle: StoreBundle) {
        var lost = Scenario.hosts
        lost[0].entry.online = false
        lost[0].entry.lastDialError = "connection reset"
        open(
            bundle, hosts: lost, entries: Transcript.pairingCopy,
            session: Sessions.claude(gate: .unknown, stream: .closed(
                reason: .object(["reason": .string("host_unreachable")]))),
            extra: [offline("Studio is not reachable")])
    }

    /// A review already part-way through: the two files nobody is reading
    /// folded away, and two remarks written where the change is.
    ///
    /// File indices are into the review's own alphabetical order, which is
    /// what every address into a review is; the patch's order is git's.
    @MainActor
    public static func reviewed(_ bundle: StoreBundle, agent: AgentId = Scenario.focus) {
        guard let review = bundle.review(agent) else { return }
        review.toggle(file: "lib.rs")
        review.toggle(file: "PROTOCOL.md")
        review.comment(
            LineRange(file: 3, from: 8, to: 10),
            """
            The catch-all swallows Code::Internal too, which isn't a pairing \
            failure. Match the three explicitly and let the rest bubble.
            """)
        review.comment(
            LineRange(file: 2, from: 5, to: 7),
            """
            Worth a test that two different failures produce byte-identical \
            output, otherwise this can regress quietly.
            """)
    }
}
