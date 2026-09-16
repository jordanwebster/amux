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
    @ObservationIgnored public private(set) var entries: [FeedEntry] = []
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
    public var dictation = DictationState()
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
    /// The refusal this conversation is entitled to draw under its composer:
    /// the host's own sentence about the message that is being sent now.
    ///
    /// A remembered failure is not that. Once another message has been
    /// dispatched, the older refusal is about something the reader has already
    /// moved past, and leaving it under the box would caption a message in
    /// flight with an earlier message's reason. So a dispatch supersedes it
    /// and only an answer to the newest dispatch replaces it; `results` still
    /// remembers every answer, for a report to say what a run was told.
    public private(set) var refusal: OpFailure?
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
    /// Updates about this agent that this build could not read. Each one is
    /// drawn at the foot of the transcript: whatever it carried is missing
    /// from the screen, and a conversation that silently went stale would
    /// look current.
    public private(set) var unreadable: [UnreadableEvent] = []

    /// Absolute position of `entries.first`.
    public private(set) var firstPosition: UInt64 = 0

    /// The already-folded rows the view reads. A stream appends far more
    /// often than it rewrites history; retaining this projection keeps every
    /// arriving row from folding the entire transcript again on the main
    /// actor.
    @ObservationIgnored private var projectedRows: [TranscriptRow] = []
    /// The view observes this small token rather than the projection's array
    /// storage. Append-only streams are applied to the model immediately but
    /// publish at most once per two 60 Hz frames, so a 50-row/second source
    /// does not make SwiftUI lay out twice inside the same display interval.
    private var projectionVersion: UInt64 = 0
    @ObservationIgnored private var hasPublishedProjection = false
    @ObservationIgnored private var projectionPublish: Task<Void, Never>?

    /// Operations dispatched from this conversation that have not been
    /// answered yet. An answer claims its entry and removes it, so a second
    /// result carrying the same identifier is not claimed twice.
    private var pendingOps: Set<OpId> = []

    /// The last operation this conversation dispatched. What the foot may
    /// quote is whatever answers this one.
    private var latestDispatch: OpId?

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
        latestDispatch = op
        refusal = nil
    }

    /// The person has sent. Called the instant the tap is handled, before
    /// anything is drawn, so the echo budget covers the whole round from
    /// finger to row.
    public func sendTapped() {
        Signposts.emit(.sendTapped)
    }

    /// The person has sent this text. Called with the tap, so the row is on
    /// screen before anything has left the phone.
    ///
    /// The two marks are left here, around the one thing the echo budget is
    /// about: the tap, and the frame a person can see their own words in.
    /// That frame is this phone's alone — the row is drawn from what was
    /// typed, before anything has left the device — so the interval is the
    /// app's work and never the network's. The host's own row arriving later
    /// and replacing this one is a different event with no budget on it.
    public func sent(_ text: String) {
        sendTapped()
        // Register for the transaction this mutation is about to cause. If
        // the observer is installed after Observation has scheduled the view
        // update, its empty Core Animation transaction can miss that commit
        // boundary and report a later one instead.
        Signposts.emitWhenDrawn(.echoCommitted)
        unacknowledged.append(PendingSend(text: text))
    }

    /// The transcript as a reader sees it: what the host has sent, then
    /// whatever this phone has sent and not seen come back.
    ///
    /// The two are drawn as the same bubble, because they are the same message
    /// and a bubble that changed appearance a second after it appeared would
    /// draw the eye to the one thing on the screen nobody needs to look at; a
    /// pending one only carries a quiet "Sending" under it. A pending row is
    /// named `pending-…`, so a test can say which frame it appeared in and
    /// which frame it stopped being pending in.
    private var layer: FeedEntry.Layer {
        switch facts {
        case .claudeSdk: .claudeSdk
        case .codex: .codex
        case .claudePty, .unavailable: .claudePty
        }
    }

    public func rows() -> [TranscriptRow] {
        guard !unacknowledged.isEmpty else { return projectedRows }
        var rows = projectedRows
        rows.reserveCapacity(rows.count + unacknowledged.count)
        rows.append(contentsOf: pendingRows())
        return rows
    }

    /// Rows confirmed by the host, kept separate so drawing one optimistic
    /// prompt does not copy a thousand-row transcript just to append it.
    public func confirmedRows() -> [TranscriptRow] {
        _ = projectionVersion
        return projectedRows
    }

    /// Prompts sent from this phone and not yet echoed by the host.
    public func pendingRows() -> [TranscriptRow] {
        unacknowledged.map {
            TranscriptRow(
                id: "pending-\($0.id.uuidString)", layer: layer,
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
        return projectedRows.last
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
            if result.op == latestDispatch {
                if case .failed(let failure) = result.outcome {
                    refusal = failure
                } else {
                    refusal = nil
                }
            }
            results.append(result)
            if results.count > Self.remembered {
                results.removeFirst(results.count - Self.remembered)
            }
        case .invariant(let detail):
            invariants.append(detail)
        case .unreadable(let unread) where unread.agent == agent:
            unreadable.append(unread)
        case .feed, .session, .diff, .fleet, .discovered, .connection, .tokenRequest, .devices,
             .attention, .cloudState, .unreadable:
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
    /// Takes the pending rows the host has now sent back off the optimistic
    /// tail, and says whether any went.
    private func reconcile(_ appended: [FeedEntry]) -> Bool {
        guard !unacknowledged.isEmpty else { return false }
        let arrived = Set(appended.transcriptRows().compactMap { row -> String? in
            guard case .prompt(let text) = row.kind else { return nil }
            return text
        })
        guard !arrived.isEmpty else { return false }
        let before = unacknowledged.count
        unacknowledged.removeAll { arrived.contains($0.text) }
        return unacknowledged.count != before
    }

    private func apply(_ update: FeedUpdate) {
        let oldCount = entries.count
        let oldEnd = firstPosition + UInt64(oldCount)
        let removedPrefix = update.evicted > firstPosition
        let isPlainAppend = !removedPrefix
            && update.replace.isEmpty
            && update.base == oldEnd
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
        guard !update.append.isEmpty else {
            if removedPrefix || !update.replace.isEmpty {
                projectedRows = entries.transcriptRows()
                publishProjection()
            }
            return
        }
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
        if isPlainAppend {
            appendProjected(update.append, after: oldCount)
        } else {
            projectedRows = entries.transcriptRows()
        }
        // An append that confirms a pending prompt takes that prompt off the
        // tail at once, so its confirmed row has to be on screen in the same
        // frame. Held back for the coalescing interval, the message vanished
        // for a frame or two and then came back.
        let confirmedAPrompt = reconcile(update.append)
        publishProjection(coalescing: isPlainAppend && !confirmedAPrompt)
        for _ in update.append { Signposts.emit(.streamRow) }
        Signposts.emit(.transcriptCommit)
    }

    /// Extends the folded projection, reopening only the exploration run
    /// that crosses the append boundary. Every other prior row is immutable
    /// in a plain append and can remain exactly where it is.
    private func appendProjected(_ appended: [FeedEntry], after oldCount: Int) {
        guard !appended.isEmpty else { return }
        guard oldCount > 0,
              appended[0].exploration?.groups == true,
              entries[oldCount - 1].exploration != nil,
              !projectedRows.isEmpty
        else {
            projectedRows.append(contentsOf: appended.transcriptRows())
            return
        }

        var runStart = oldCount - 1
        while runStart > 0,
              entries[runStart].exploration?.groups == true,
              entries[runStart - 1].exploration != nil {
            runStart -= 1
        }
        projectedRows.removeLast()
        projectedRows.append(contentsOf: Array(entries[runStart...]).transcriptRows())
    }

    /// Makes the newest projection visible to Observation.
    ///
    /// Initial content and structural rewrites are synchronous. Only ordinary
    /// tail appends are gathered, with a bound short enough that a 60 Hz
    /// display can miss at most two frames while the model itself remains
    /// fully current.
    private func publishProjection(coalescing: Bool = false) {
        if !hasPublishedProjection || !coalescing {
            projectionPublish?.cancel()
            projectionPublish = nil
            hasPublishedProjection = true
            projectionVersion &+= 1
            return
        }
        guard projectionPublish == nil else { return }
        projectionPublish = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(33))
            guard !Task.isCancelled, let self else { return }
            projectionPublish = nil
            projectionVersion &+= 1
        }
    }
}
