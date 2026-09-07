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

    /// How picked bytes reach the machine.
    ///
    /// Separate from `dispatch` because a command is JSON and a photograph is
    /// not: the bytes go to the library through their own entry. Whoever owns
    /// the connection sets this; a bundle with no connection behind it — a
    /// fixture, a replay — leaves it alone and nothing is stored anywhere.
    @ObservationIgnored public var store: (@MainActor (PickedAttachment, Data) -> OpId?)?

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

    /// Sends what is being written to an agent, or holds it where a turn is
    /// already running.
    ///
    /// Which of the two it is comes off the layer's own gate rather than off
    /// anything the screen believes: the composer draws itself from the same
    /// gate, so a box that says "Queue a message" and a command that says
    /// `send` cannot disagree. The optimistic row is added only for a message
    /// that is actually on its way — a held one is not in the transcript yet
    /// and will not be until the turn ends.
    ///
    /// False means nothing left the phone: an empty draft, or no connection.
    /// The draft is kept in that case, because the alternative is a person
    /// watching a paragraph they wrote disappear into a failure.
    @discardableResult
    public func send(to agent: AgentId) -> Bool {
        let store = conversation(agent)
        guard !store.draft.isEmpty else { return false }
        let hold = !store.gate.accepts
        // A second hold is refused: one message is queued, not a queue. So
        // writing another while one waits replaces it, which is the gesture
        // a person is actually making — the old text is theirs to get back by
        // unqueueing, and this is the other half of the same pair.
        let held = store.queued != nil
        guard let command = hold
                ? (held ? store.draft.replaceCommand(to: agent)
                        : store.draft.holdCommand(to: agent))
                : store.draft.command(to: agent),
              let op = dispatch?(command)
        else { return false }
        // The optimistic row is folded the way a held message folds, so a
        // draft that is a command reads as the command it is rather than as
        // its arguments alone — and reads the same as the host's own echo of
        // it, which is all the two rows have to match on.
        if !hold { store.sent(store.draft.held.text) }
        store.dispatched(op)
        store.draft.clear()
        return true
    }

    /// Takes the held message back out of the queue and into the field.
    ///
    /// There is no edit mode and no discard. What was queued becomes an
    /// ordinary unsent message in front of the person who wrote it, and
    /// abandoning it is clearing the field the way every other unsent message
    /// is abandoned. The text goes into the draft before the cancellation is
    /// dispatched, so nothing is lost if the host refuses.
    ///
    /// False means nothing left the phone: no message held, one already on
    /// its way, or no connection.
    @discardableResult
    public func unqueue(_ agent: AgentId) -> Bool {
        let store = conversation(agent)
        guard let queued = store.queued, queued.changeable else { return false }
        store.draft = MessageDraft(prose: queued.text)
        store.draft.place(caret: queued.text.count)
        guard let command = MessageDraft.cancelQueue(agent),
              let op = dispatch?(command) else { return false }
        store.dispatched(op)
        return true
    }

    /// Stops the turn that is running, in the layer's own words.
    ///
    /// Interrupting is not clearing. What is in the field stays in the field:
    /// stopping an agent that is going the wrong way is usually the first half
    /// of telling it which way to go instead, and throwing the second half
    /// away would make the two one gesture.
    @discardableResult
    public func interrupt(_ agent: AgentId) -> Bool {
        let store = conversation(agent)
        guard let command = store.gate.interrupt(agent),
              let op = dispatch?(.shared(command)) else { return false }
        store.dispatched(op)
        return true
    }

    /// The model this agent's next turn runs under.
    ///
    /// Refused where the layer says it would refuse it. The sheet still opens
    /// on a session that cannot be changed — what an agent runs under is worth
    /// reading either way — so the rows are still there to press, and the gate
    /// the sheet prints its refusal from is the same one consulted here.
    @discardableResult
    public func setModel(_ model: String, of agent: AgentId) -> Bool {
        settings(AgentWrite.model(model, of: agent), of: agent)
    }

    /// How hard it thinks, as one of the levels the layer reported.
    @discardableResult
    public func setEffort(_ effort: String, of agent: AgentId) -> Bool {
        settings(AgentWrite.effort(effort, of: agent), of: agent)
    }

    /// What this agent may do without asking, named as its own provider names
    /// it. The provider decides the shape of the write, so the facts this
    /// conversation was last told are what spell it.
    @discardableResult
    public func setPermission(_ choice: String, of agent: AgentId) -> Bool {
        let store = conversation(agent)
        guard let command = ProviderPermission(store.provider.permission)
            .command(choosing: choice, agent: agent) else { return false }
        return settings(command, of: agent)
    }

    /// Every write behind the two sheets, sent only where the layer would
    /// take it.
    private func settings(_ command: BridgeCommand, of agent: AgentId) -> Bool {
        let store = conversation(agent)
        guard store.settingsGate.refusal == nil, let op = dispatch?(command) else { return false }
        store.dispatched(op)
        return true
    }

    /// Stores something picked from the system's own pickers, so the message
    /// being written can name it.
    ///
    /// The token is not put in the draft here. It stands at the caret when the
    /// host says the bytes are stored, because a token is a reference to
    /// something that exists: one written the instant the picker closed would
    /// name nothing if the store then failed, and would be sitting in a
    /// sentence somebody had gone on writing.
    ///
    /// False means nothing left the phone: no connection, or nothing picked.
    @discardableResult
    public func attach(_ picked: PickedAttachment, bytes: Data) -> Bool {
        guard !bytes.isEmpty, let op = store?(picked, bytes) else { return false }
        conversation(picked.agent).dispatched(op)
        return true
    }

    /// What this agent is called. The name is the host's to keep: it answers
    /// with the agent it renamed, and the fleet redraws from that rather than
    /// from what was typed here.
    @discardableResult
    public func rename(_ name: String, of agent: AgentId) -> Bool {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let op = dispatch?(AgentWrite.rename(trimmed, of: agent)) else { return false }
        conversation(agent).dispatched(op)
        return true
    }

    /// Deletes the agent. Nothing is closed here: the conversation stays until
    /// the host says the agent is gone, because a screen that vanished on the
    /// press would be this phone claiming something it has not been told.
    @discardableResult
    public func delete(_ agent: AgentId) -> Bool {
        guard let op = dispatch?(AgentWrite.delete(agent)) else { return false }
        conversation(agent).dispatched(op)
        return true
    }

    public func closeConversation(_ agent: AgentId) {
        reviews.removeValue(forKey: agent)
        guard conversations.removeValue(forKey: agent) != nil else { return }
        unwatch?(agent)
    }
}
