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
    public private(set) var results: [OpResult] = []
    /// A batch the bridge could not place. Kept rather than hidden: a hole in
    /// the transcript is a fact the report screen has to be able to state.
    public private(set) var invariants: [String] = []

    /// Absolute position of `entries.first`.
    public private(set) var firstPosition: UInt64 = 0

    public init(agent: AgentId) {
        self.agent = agent
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
            results.append(result)
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
        guard !update.append.isEmpty else { return }
        if entries.isEmpty { firstPosition = update.base }
        let end = firstPosition + UInt64(entries.count)
        if update.base < end {
            entries.removeLast(Int(end - update.base))
        } else if update.base > end {
            invariants.append("feed gap between \(end) and \(update.base)")
        }
        entries.append(contentsOf: update.append)
    }
}
