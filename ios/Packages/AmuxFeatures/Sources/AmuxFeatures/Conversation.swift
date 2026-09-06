import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened in a conversation. Like every other screen, it decides
/// nothing and navigates nowhere: it says what the person did and whoever
/// presented it takes them there.
public enum ConversationAction: Equatable, Sendable {
    /// The fleet, asked for from inside the conversation.
    case openDrawer
    /// The changes this turn made, asked for from the chip.
    case openChanges
    /// Everything a conversation can be done to rather than said to.
    case overflow
    /// Reach the machine again now, rather than waiting for the next attempt.
    case retry
    /// What the person told the agent that was waiting on them.
    case answer(AskPanel, AskDecision)
}

/// Who this conversation is with and where it runs.
///
/// The chrome names three facts and the screen owns none of them: the agent's
/// name is the fleet's, the machine is the hosts store's, and the directory is
/// the agent's. They are handed in together so a conversation opened before
/// the fleet has arrived still names itself.
public struct ConversationSubject: Equatable, Sendable {
    public let name: String
    public let host: String?
    public let directory: String
    /// Whether the machine that owns this agent is answering.
    ///
    /// A conversation opened before the fleet has arrived is not called
    /// unreachable: nothing has said it is, and marking a screen stale on the
    /// strength of not having heard yet is the same lie in the other
    /// direction.
    public let hostReachable: Bool
    /// How long ago this agent last did anything, in the shortest true unit.
    /// Absent while the fleet that knows has not arrived.
    public let age: String?
    /// Set once the agent has stopped for good.
    public let ended: Ended?
    /// Whether this agent's last turn finished and nobody has read it yet.
    ///
    /// The fleet keeps one vocabulary for everything that needs you, and a
    /// finished turn is one of them. The conversation asks the same question
    /// the home does rather than inventing a second answer from the gate.
    public let finished: Bool

    /// A run that is over.
    public struct Ended: Equatable, Sendable {
        /// Whatever code the host reported. Absent means the host never said
        /// which, which is not the same as zero and is never drawn as one.
        public let code: Int?

        public init(code: Int?) {
            self.code = code
        }
    }

    public init(
        name: String, host: String?, directory: String,
        hostReachable: Bool = true, age: String? = nil, ended: Ended? = nil,
        finished: Bool = false
    ) {
        self.name = name
        self.host = host
        self.directory = directory
        self.hostReachable = hostReachable
        self.age = age
        self.ended = ended
        self.finished = finished
    }

    /// What the chrome names an agent, gathered from the fleet that owns those
    /// facts. A conversation opened before the fleet has arrived names itself
    /// with the identity it was opened with rather than with nothing.
    @MainActor
    public init(agent: AgentId, in fleet: FleetStore) {
        guard let row = fleet.rows.first(where: { $0.id == agent }) else {
            self.init(name: agent.description, host: nil, directory: "")
            return
        }
        var ended: Ended?
        if case .exited(let code) = row.phase { ended = Ended(code: code) }
        self.init(
            name: row.name, host: fleet.host(row.hostId)?.name,
            directory: row.workingDirectory,
            hostReachable: fleet.host(row.hostId)?.online ?? true,
            age: row.age(at: fleet.orderedAt),
            ended: ended,
            finished: row.attention == .needsYou(why: .finished))
    }

    /// "Studio · ~/src/amux", or just the directory while the machine that
    /// owns this agent has not been heard from.
    ///
    /// A machine that has gone away says so here instead of naming the
    /// directory. The directory has not changed, but it is the least useful
    /// true thing on the screen at the moment the machine holding it cannot
    /// be reached, and this line is the one place a reader is already looking
    /// to find out where this conversation lives.
    public var place: String {
        guard hostReachable else {
            return [host, "unreachable"].compactMap { $0 }.joined(separator: " · ")
        }
        return [host, directory].compactMap { $0 }.joined(separator: " · ")
    }
}

/// One agent's conversation.
///
/// It has no navigation bar. A centred title with a tinted back chevron is the
/// most recognisably iOS object there is, and it made a conversation look like
/// the settings screen beside it; more to the point, a bar is a strip of screen
/// permanently spent on a name that never changes. So the feed runs to the top
/// of the display, the platform's own scroll edge effect frosts what passes
/// under the chrome, and two glass controls float over it. The left one is the
/// drawer, which is how you leave.
public struct Conversation: View {
    @Environment(\.design) private var design
    private let model: ConversationStore
    private let subject: ConversationSubject
    private let actions: @MainActor (ConversationAction) -> Void
    /// Whether the finished turn's panel has been set aside for this visit.
    ///
    /// View state, because Later is not something the host is told: nothing is
    /// sent, the changes stay where they are, and the chip in the chrome is
    /// still the way to them. Coming back to the conversation offers again.
    @State private var deferred = false

    public init(
        model: ConversationStore,
        subject: ConversationSubject,
        actions: @escaping @MainActor (ConversationAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            transcript
        }
        // A name put on a container is handed down to everything under it the
        // system does not already treat as its own, so without this the pill,
        // the chip and every row answer to "conversation" — for VoiceOver and
        // for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("conversation", value: model.agent.description)
    }

    /// The feed, under the chrome rather than beside it.
    ///
    /// The inset is what puts the pill over the scroll view instead of above
    /// it: the content keeps clear of the chrome when it is at rest and travels
    /// underneath it when it scrolls, which is the only arrangement in which
    /// frosting the top edge means anything.
    private var transcript: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if case .claudeSdk(let supported) = model.facts, !supported {
                    UnsupportedLayer(layer: "this agent's transcript")
                        .padding(.top, design.metrics.feedGap)
                } else {
                    TranscriptFeed(rows: model.entries.transcriptRows())
                }
                // The end of a run belongs in the feed rather than under it.
                // It is the last thing that happened, in sequence after the
                // last thing the agent said, and a run that ended is not a
                // state of the screen you can act on — it is a fact about the
                // transcript you scroll to the bottom of.
                if let ended = subject.ended {
                    EndOfRun(ended: ended, age: subject.age, host: subject.host)
                        .padding(.horizontal, design.metrics.gutter)
                        .padding(.top, design.metrics.feedGap)
                }
            }
            .padding(.bottom, 120)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.hidden)
        // The platform's effect, not a hand-drawn plate. Masking a glass layer
        // to make it fade stops it sampling what is behind it, so it renders
        // as a pane you can read straight through; this samples correctly.
        .scrollEdgeEffectStyle(.soft, for: .top)
        .safeAreaInset(edge: .top, spacing: 0) { chrome }
        .safeAreaInset(edge: .bottom, spacing: 0) { foot }
    }

    /// What occupies the composer's place when a message would not go.
    ///
    /// The composer is the one control on this screen that lies by staying
    /// usable, so where it will sit is where a refusal is reported. Nothing
    /// is drawn when the layer is taking messages: an empty strip along the
    /// bottom of every ordinary conversation would cost the feed a row of
    /// screen to say nothing.
    @ViewBuilder
    private var foot: some View {
        Group {
            // An unanswered ask outranks everything else down here. Whatever
            // else is true — a machine that has gone quiet, a layer catching
            // up — the agent has stopped and is waiting on one answer, and
            // that answer is the only thing worth offering.
            if let panel = model.asks.panel {
                AskPanelView(panel: panel) { actions(.answer(panel, $0)) }
            } else if let changes = model.changes, !changes.isEmpty,
                      subject.finished, !deferred {
                FinishedPanel(
                    changes: changes, review: { actions(.openChanges) },
                    later: { deferred = true })
            } else if let state = ConversationFootState(
                gate: model.gate, results: model.results, subject: subject) {
                ConversationFoot(state: state) { actions(.retry) }
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 10)
    }

    // MARK: - The chrome

    private var chrome: some View {
        HStack(alignment: .center, spacing: 8) {
            pill
            Spacer(minLength: 6)
            if let changes = model.changes, !changes.isEmpty {
                ChangesChip(changes: changes) { actions(.openChanges) }
            }
            Button { actions(.overflow) } label: {
                GlassIcon(glyph: "ellipsis")
            }
            .accessibilityLabel("More")
            .identified("conversation.overflow", label: "More")
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 6)
    }

    /// The agent, its machine and its directory, on one floating surface with
    /// the way out on its leading edge.
    ///
    /// The drawer control is inside the pill rather than beside it because the
    /// two belong together: the pill says which conversation you are in, and
    /// the control is how you go to another one.
    private var pill: some View {
        HStack(spacing: 10) {
            Button { actions(.openDrawer) } label: {
                Image(systemName: "sidebar.left")
                    .font(.system(size: 17, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Agents")
            .identified("conversation.drawer", label: "Agents")
            VStack(alignment: .leading, spacing: 0) {
                Text(subject.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                Text(subject.place)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
            }
            .padding(.trailing, 14)
        }
        .padding(.leading, 2)
        .frame(minHeight: 52)
        .frosted(Capsule())
        .accessibilityElement(children: .contain)
        .identified(
            "conversation.subject", label: "\(subject.name), \(subject.place)",
            value: subject.name)
    }
}

/// The way in to the changes a turn made.
///
/// It is a call to action rather than a menu item: on a phone, reaching the
/// changes is close to the whole reason to open a conversation once a turn
/// ends. It appears only when there is something to review, and it is drawn in
/// the diff's own green and red rather than in the accent, because those
/// colours are a convention about what changed and the accent is this app's
/// one word for "something is waiting for you". A diff is not that.
struct ChangesChip: View {
    @Environment(\.design) private var design
    let changes: ReviewDocument
    let open: @MainActor () -> Void

    /// "+118 −40". The minus is a true minus sign, not a hyphen: it sits
    /// beside a plus and has to read as its opposite.
    private var insertions: String { "+\(changes.insertions)" }
    private var deletions: String { "\u{2212}\(changes.deletions)" }

    var body: some View {
        Button(action: open) {
            HStack(spacing: 6) {
                Text(insertions).foregroundStyle(design.added.color)
                Text(deletions).foregroundStyle(design.removed.color)
            }
            .designFont(.caption, design)
            .padding(.horizontal, 13)
            // The material goes behind the label rather than over it. Glass
            // asked for on a button's own label is drawn as a filled control
            // in the label's colours, which on a light ground is a black
            // capsule; a layer behind it is the frosted plate this wants.
            .frame(minWidth: 44, minHeight: 44)
            .background { Color.clear.frosted(Capsule()) }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            "Review \(changes.insertions) added, \(changes.deletions) removed")
        .identified(
            "conversation.changes",
            label: "Review \(changes.insertions) added, \(changes.deletions) removed",
            value: "\(insertions) \(deletions)")
    }
}
