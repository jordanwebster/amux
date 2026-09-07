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
    /// The frozen patch this turn offers to show, and what it is.
    public private(set) var changes: ReviewDocument?
    /// The artifact that patch is, so a review sent about it names it.
    public private(set) var changesArtifact: ArtifactId?
    /// What is being written to this agent: the review attached from the diff
    /// page, and whatever is said beside it. It lives here rather than in the
    /// composer, so a half-written paragraph survives a trip to the diff and
    /// back.
    public var draft = MessageDraft()
    /// Messages the person sent that the host has not echoed back yet.
    ///
    /// A phone on a train is often a second or more away from the machine it
    /// is writing to, and a message that vanishes from the field and appears
    /// nowhere reads as a message that was lost. So a send is on screen in the
    /// frame the finger is lifted in, drawn as the prompt it will become, and
    /// the host's own row replaces it when it arrives.
    public private(set) var unacknowledged: [PendingSend] = []

    /// One sent message, waiting to be replaced by the host's own row.
    public struct PendingSend: Identifiable, Equatable, Sendable {
        public let id: UUID
        public let text: String

        public init(id: UUID = UUID(), text: String) {
            self.id = id
            self.text = text
        }
    }

    /// Results for operations this conversation dispatched, newest last.
    ///
    /// A result names its operation and no agent, so the connection has to
    /// offer every one to every open conversation. A conversation keeps only
    /// the ones it asked for: anything else answers for some other agent, and
    /// drawing it here would put a host's sentence about one agent under
    /// another agent's name.
    public private(set) var results: [OpResult] = []
    /// Set once the host has confirmed this agent is gone.
    ///
    /// The confirmation and not the press: pressing Delete asks, and a screen
    /// that closed on the asking would be the phone claiming an outcome it has
    /// not been told. Whoever is showing this conversation leaves when this
    /// turns true.
    public private(set) var deleted = false
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

    /// The person has sent this text. Called with the tap, so the row is on
    /// screen before anything has left the phone.
    public func sent(_ text: String) {
        unacknowledged.append(PendingSend(text: text))
        sendTapped()
    }

    /// The transcript as a reader sees it: what the host has sent, then
    /// whatever this phone has sent and not seen come back.
    ///
    /// The two are drawn the same, because they are the same message and a
    /// row that changed appearance a second after it appeared would draw the
    /// eye to the one thing on the screen nobody needs to look at. What
    /// distinguishes them is that a pending row is named `pending-…`, so a
    /// test can say which frame it appeared in and which frame it stopped
    /// being pending in.
    public func rows() -> [TranscriptRow] {
        entries.transcriptRows() + unacknowledged.map {
            TranscriptRow(
                id: "pending-\($0.id.uuidString)", layer: .claudePty,
                kind: .prompt(text: $0.text))
        }
    }

    /// What the agent is doing, which is whatever its last row is still doing.
    ///
    /// Read off the tail rather than the whole feed: naming the open row costs
    /// nothing per frame this way, and no row before the last few can be the
    /// one still running.
    public var tailRow: TranscriptRow? {
        guard unacknowledged.isEmpty else { return nil }
        return Array(entries.suffix(Self.tailRead)).transcriptRows().last
    }

    /// How many entries back the open row can be. A folded run of reads and
    /// searches is several entries and one row, and nothing else folds.
    private static let tailRead = 8

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
            changesArtifact = update.diff
        case .opResult(let result):
            guard pendingOps.remove(result.op) != nil else { break }
            if case .agentDeleted = result.outcome { deleted = true }
            // The token stands at the caret only now, on the host's own word
            // that the bytes are stored. It is spelled by the shared library
            // from what the host answered with, so what the message carries is
            // the artifact the host has and not a description made here.
            if case .attachmentStored(let attachment) = result.outcome,
               let token = Bridge.token(for: attachment) {
                draft.insert(token)
            }
            results.append(result)
            if results.count > Self.remembered {
                results.removeFirst(results.count - Self.remembered)
            }
        case .invariant(let detail):
            invariants.append(detail)
        case .feed, .session, .diff, .fleet, .discovered, .connection, .tokenRequest, .devices:
            break
        }
    }

    /// Drops the optimistic row for any message the host has now sent back.
    ///
    /// Matched on the text, because that is all the two rows share: the phone
    /// never sees the position or identity the host will give a prompt, and
    /// guessing one would put a row in the feed at a place the host disagrees
    /// with. Only the rows that just arrived are examined, so a long feed
    /// costs nothing.
    private func reconcile(_ appended: [FeedEntry]) {
        guard !unacknowledged.isEmpty else { return }
        let arrived = Set(appended.transcriptRows().compactMap { row -> String? in
            guard case .prompt(let text) = row.kind else { return nil }
            return text
        })
        guard !arrived.isEmpty else { return }
        unacknowledged.removeAll { arrived.contains($0.text) }
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
        if update.base < firstPosition {
            // A replay from further back than anything still held. A stream
            // released while nobody was reading it is sent again from its
            // start when the conversation is reopened, and by then this may
            // have dropped an evicted prefix that the replay still carries.
            // Nothing held sits inside what is arriving, so it all goes and
            // the replay becomes the feed; keeping any of it would put rows
            // after positions that are about to be rewritten.
            entries.removeAll()
            firstPosition = update.base
        } else if update.base < end {
            entries.removeLast(Int(end - update.base))
        } else if update.base > end {
            invariants.append("feed gap between \(end) and \(update.base)")
        }
        entries.append(contentsOf: update.append)
        reconcile(update.append)
        for _ in update.append { Signposts.emit(.streamRow) }
        Signposts.emit(.transcriptCommit)
        if awaitingEcho {
            awaitingEcho = false
            Signposts.emitWhenPresented(.echoCommitted)
        }
    }
}
