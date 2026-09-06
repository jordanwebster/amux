import Foundation
import Observation

/// Every store one account's connection writes to.
///
/// A batch is applied whole and in order: the events in it describe one
/// consistent moment, and applying half of it would put a fleet and a
/// conversation into different moments.
@MainActor
@Observable
public final class StoreBundle {
    public let account: AccountId
    public let fleet: FleetStore
    public let hosts: HostsStore
    public private(set) var conversations: [AgentId: ConversationStore] = [:]
    /// One review per agent, opened the first time its changes are read.
    private var reviews: [AgentId: ReviewStore] = [:]
    /// Batches applied, in the order they arrived.
    public private(set) var applied = 0

    /// Told when a conversation is opened and when it is closed.
    ///
    /// The runtime only projects a feed for an agent this client asked to
    /// watch, so opening a store here has to become a subscription there.
    /// Whoever owns the connection sets these; a bundle with no connection
    /// behind it — a fixture, a replay — leaves them alone and nothing is
    /// asked of a runtime that is not there.
    @ObservationIgnored public var watch: (@MainActor (AgentId) -> Void)?
    @ObservationIgnored public var unwatch: (@MainActor (AgentId) -> Void)?

    /// How a decision made on a screen reaches the machine.
    ///
    /// Answering an agent that is waiting is the one thing a conversation
    /// does that has to leave the phone, and the screens decide nothing and
    /// reach nothing themselves. Whoever owns the connection sets this; a
    /// bundle with no connection behind it — a fixture, a replay — leaves it
    /// alone, and a photograph of a panel answers nobody.
    @ObservationIgnored public var dispatch: (@MainActor (BridgeCommand) -> OpId?)?

    public init(account: AccountId, now: Date = Date(), unread: UnreadWeights = UnreadWeights()) {
        self.account = account
        self.fleet = FleetStore(now: now, unread: unread)
        self.hosts = HostsStore()
    }

    public func apply(_ batch: [Event]) {
        for event in batch { apply(event) }
        applied += 1
    }

    public func apply(_ event: Event) {
        fleet.apply(event)
        hosts.apply(event)
        switch event {
        case .feed(let update): conversation(update.agent).apply(event)
        case .session(let session): conversation(session.agent).apply(event)
        case .diff(let update): conversation(update.agent).apply(event)
        // Nothing here names an agent, so every open conversation is offered
        // the event and decides for itself. A result is claimed only by the
        // conversation that dispatched the operation it answers.
        case .opResult, .fleet, .connection, .tokenRequest, .invariant:
            for store in conversations.values { store.apply(event) }
        }
    }

    /// The conversation for an agent, opened on first mention. The bridge only
    /// projects feeds for agents this client subscribed to, so a store here
    /// means a subscription there.
    @discardableResult
    public func conversation(_ agent: AgentId) -> ConversationStore {
        if let existing = conversations[agent] { return existing }
        let store = ConversationStore(agent: agent)
        conversations[agent] = store
        watch?(agent)
        return store
    }

    /// The review of one agent's frozen changes, or nothing where that agent
    /// has offered none.
    ///
    /// Kept rather than rebuilt on every visit: the comments somebody has
    /// written are the review, and leaving the page to check something in the
    /// conversation must not throw them away. A new patch is a new review, so
    /// the store is replaced when the artifact changes and the comments about
    /// the old one go with it — they were written about lines that patch no
    /// longer has.
    public func review(_ agent: AgentId) -> ReviewStore? {
        guard let document = conversation(agent).changes,
              let artifact = conversation(agent).changesArtifact else { return nil }
        if let existing = reviews[agent], existing.diff == artifact { return existing }
        let store = ReviewStore(diff: artifact, document: document)
        reviews[agent] = store
        return store
    }

    /// Tells an agent what the person answered, in the layer's own words.
    ///
    /// The panel spells the command, because only the panel knows which ask
    /// this is and which layer raised it; the operation comes back to the
    /// conversation that answered so the host's reply is claimed by it rather
    /// than by whichever conversation happens to be open. False means the
    /// answer never left: no connection, or a decision the ask cannot take,
    /// which is a mistake in this app and not something a person can do.
    @discardableResult
    public func answer(_ panel: AskPanel, _ decision: AskDecision, of agent: AgentId) -> Bool {
        guard let command = panel.command(decision, agent: agent),
              let op = dispatch?(.shared(command)) else { return false }
        conversation(agent).dispatched(op)
        return true
    }

    public func closeConversation(_ agent: AgentId) {
        reviews.removeValue(forKey: agent)
        guard conversations.removeValue(forKey: agent) != nil else { return }
        unwatch?(agent)
    }
}
