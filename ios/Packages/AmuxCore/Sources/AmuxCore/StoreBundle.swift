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

    public func closeConversation(_ agent: AgentId) {
        guard conversations.removeValue(forKey: agent) != nil else { return }
        unwatch?(agent)
    }
}
