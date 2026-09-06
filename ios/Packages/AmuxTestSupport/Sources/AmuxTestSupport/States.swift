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
