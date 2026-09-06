import Foundation
import Observation

/// One agent's conversation.
///
/// The bridge sends transcript changes as absolute positions, so a row that
/// was rewritten upstream is rewritten here rather than appended twice, and an
/// evicted prefix leaves without renumbering what survives.
@MainActor
@Observable
public final class ConversationStore {
    public let agent: AgentId
    public private(set) var entries: [FeedEntry] = []
    public private(set) var gate: SendGate = .unavailable
    public private(set) var phase: LayerPhase = .unavailable
    public private(set) var stream: StreamPhase?
    public private(set) var asks: [Ask] = []
    public private(set) var facts: SessionFacts = .unavailable
    public private(set) var provider = ProviderFacts()
    public private(set) var settingsGate: SettingsGate = .unavailable
    public private(set) var queued: QueuedMessage?
    public private(set) var family: [FamilyMember] = []
    public private(set) var changes: DiffDocument?
    /// Results for operations this conversation dispatched, newest last.
    ///
    /// A result names its operation and no agent, so the connection has to
    /// offer every one to every open conversation. A conversation keeps only
    /// the ones it asked for: anything else answers for some other agent, and
    /// drawing it here would put a host's sentence about one agent under
    /// another agent's name.
    public private(set) var results: [OpResult] = []
    /// A batch the bridge could not place. Kept rather than hidden: a hole in
    /// the transcript is a fact the report screen has to be able to state.
    public private(set) var invariants: [String] = []

    /// Absolute position of `entries.first`.
    public private(set) var firstPosition: UInt64 = 0

    /// Set when the person has sent and the host has not yet echoed the row
    /// back. The pair of marks around it is what the optimistic-echo budget
    /// is measured between.
    private var awaitingEcho = false

    /// Operations dispatched from this conversation that have not been
    /// answered yet. An answer claims its entry and removes it, so a second
    /// result carrying the same identifier is not claimed twice.
    private var pendingOps: Set<OpId> = []

    /// How many answers a conversation remembers. Only the newest is ever
    /// drawn; the rest are kept so a report can say what a run was told.
    /// Unbounded, this would grow for as long as the app runs.
    private static let remembered = 32

    public init(agent: AgentId) {
        self.agent = agent
    }

    /// This conversation has dispatched an operation and the result carrying
    /// this identifier is its own. Called by whoever sends, with the
    /// identifier the bridge answered with.
    public func dispatched(_ op: OpId) {
        pendingOps.insert(op)
    }

    /// The person has sent. Called the instant the tap is handled, before
    /// anything is drawn, so the echo budget covers the whole round from
    /// finger to row.
    public func sendTapped() {
        awaitingEcho = true
        Signposts.emit(.sendTapped)
    }

    public func apply(_ event: Event) {
        switch event {
        case .feed(let update) where update.agent == agent:
            apply(update)
        case .session(let session) where session.agent == agent:
            gate = session.gate
            phase = session.phase
            stream = session.stream
            asks = session.asks
            facts = session.facts
            provider = session.provider
            settingsGate = session.settingsGate
            queued = session.queue
            family = session.family
        case .diff(let update) where update.agent == agent:
            changes = update.document
        case .opResult(let result):
            guard pendingOps.remove(result.op) != nil else { break }
            results.append(result)
            if results.count > Self.remembered {
                results.removeFirst(results.count - Self.remembered)
            }
        case .invariant(let detail):
            invariants.append(detail)
        case .feed, .session, .diff, .fleet, .connection, .tokenRequest:
            break
        }
    }

    private func apply(_ update: FeedUpdate) {
        if update.evicted > firstPosition {
            let gone = Int(min(update.evicted - firstPosition, UInt64(entries.count)))
            entries.removeFirst(gone)
            firstPosition = update.evicted
        }
        for replacement in update.replace {
            guard replacement.position >= firstPosition else { continue }
            let index = Int(replacement.position - firstPosition)
            guard index < entries.count else {
                invariants.append("feed replacement past the end at \(replacement.position)")
                continue
            }
            entries[index] = replacement.entry
        }
        if !update.replace.isEmpty { Signposts.emit(.transcriptCommit) }
        guard !update.append.isEmpty else { return }
        if entries.isEmpty { firstPosition = update.base }
        let end = firstPosition + UInt64(entries.count)
        if update.base < end {
            entries.removeLast(Int(end - update.base))
        } else if update.base > end {
            invariants.append("feed gap between \(end) and \(update.base)")
        }
        entries.append(contentsOf: update.append)
        for _ in update.append { Signposts.emit(.streamRow) }
        Signposts.emit(.transcriptCommit)
        if awaitingEcho {
            awaitingEcho = false
            Signposts.emitWhenPresented(.echoCommitted)
        }
    }
}
