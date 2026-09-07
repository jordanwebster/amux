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
    /// One of the agents this one started, asked for from the list of them.
    /// Answering a child's ask happens in the child's own conversation, so
    /// reaching it is going there rather than answering from here.
    case openChild(AgentId)
    /// Send what is in the composer, or hold it where a turn is running.
    /// What is written lives in the conversation's own draft, so nothing
    /// travels with this but the intention.
    case send
    /// Stop the turn that is running. Distinct from clearing the field, which
    /// never leaves the phone and is the composer's own business.
    case interrupt
    /// Attach something to the message, asked for from the plus.
    case attach
    /// One of the things the plus offers, chosen.
    case attaching(AttachChoice)
    /// Speak the message instead of typing it.
    case dictate
    /// A command raised by typing a slash, picked. The draft has it already;
    /// this is so a driver and a journey can see which.
    case picking(ProviderCommand)
    /// The model and effort sheet, asked for from the footer chip.
    case openSettings
    /// How this agent runs, changed: a model, an effort level or what it may
    /// do without asking. The screen says what was picked and the layer that
    /// owns the session decides whether it takes.
    case setting(SettingChange)
    /// One of the things the overflow offers, chosen.
    case overflowing(OverflowChoice)
    /// Delete this agent, confirmed after being told what that does.
    case deleteAgent
}

/// Something the conversation opens over itself.
///
/// These are states of this screen rather than screens beside it: what the
/// plus offers and what the overflow offers are both about the conversation
/// you are in, and neither takes you anywhere. It is a parameter as well as
/// state so a screen can be opened already showing one — which is how each is
/// photographed, and how a conversation reached from a notification about a
/// permission could open on it.
public enum ConversationOverlay: Equatable, Sendable {
    case plus
    /// Model and effort, from the footer chip.
    case settings
    /// What the agent may do without asking, from the row in the plus.
    case permissions
    /// Everything the agent can be done to, from the ellipsis.
    case overflow
    /// Deleting it, with the consequences named.
    case deleteAgent
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
    /// How long the agent has been on the work it announced, in the shortest
    /// true unit. This is the number the composer reports while a turn runs,
    /// and it is the fleet's arithmetic rather than a clock this screen keeps:
    /// a timer started on the phone would go on counting through a host that
    /// had stopped answering.
    public let working: String?
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
        finished: Bool = false, working: String? = nil
    ) {
        self.name = name
        self.host = host
        self.directory = directory
        self.hostReachable = hostReachable
        self.age = age
        self.ended = ended
        self.finished = finished
        self.working = working
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
            finished: row.attention == .needsYou(why: .finished),
            working: row.working(at: fleet.orderedAt))
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
    /// What the fleet calls an agent this one started. The fleet owns names
    /// and this screen does not, so whoever has the fleet hands the naming in
    /// and a conversation drawn without one falls back to the identity, which
    /// is at least true.
    private let naming: (AgentId) -> String
    private let actions: @MainActor (ConversationAction) -> Void
    /// Whether the finished turn's panel has been set aside for this visit.
    ///
    /// View state, because Later is not something the host is told: nothing is
    /// sent, the changes stay where they are, and the chip in the chrome is
    /// still the way to them. Coming back to the conversation offers again.
    @State private var deferred = false

    /// Which child said it cannot be opened, while its sentence is on show.
    ///
    /// View state, because pressing one of those chips is not going anywhere:
    /// nothing is sent, nothing is opened, and coming back to the conversation
    /// starts again with the strip as it was.
    @State private var unopenable: ChildRow.ID?

    /// What this conversation has opened over itself, if anything.
    @State private var showing: ConversationOverlay?

    public init(
        model: ConversationStore,
        subject: ConversationSubject,
        naming: @escaping (AgentId) -> String = { $0.description },
        showing: ConversationOverlay? = nil,
        actions: @escaping @MainActor (ConversationAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.naming = naming
        self.actions = actions
        _showing = State(initialValue: showing)
    }

    public var body: some View {
        ZStack {
            Ground()
            transcript
            // The two surfaces that are not about the message being written:
            // what the agent can be done to, and doing the one of those that
            // cannot be undone. They sit over the whole screen rather than in
            // the foot, because neither replaces the composer — the overflow
            // hangs from the control that opened it, and a deletion is a
            // question about the conversation as a whole.
            if showing == .overflow {
                OverflowMenu(address: address) { choice in
                    showing = choice == .delete ? .deleteAgent : nil
                    actions(.overflowing(choice))
                }
                .padding(.horizontal, design.metrics.gutter)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                .padding(.top, 108)
            }
            if showing == .deleteAgent {
                DeleteAgentCard(
                    name: subject.name,
                    cancel: { showing = nil },
                    confirm: {
                        showing = nil
                        actions(.deleteAgent)
                    })
                    .padding(.horizontal, 12)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                    .padding(.bottom, 10)
            }
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
                    TranscriptFeed(rows: model.rows())
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
        // Anything open over the conversation takes the transcript back and
        // gives a press anywhere on it somewhere to land. It goes on the feed
        // rather than on the whole screen on purpose: the pill and the
        // composer stay where they were and stay bright, because what has
        // opened is *from* them and they are still what you are working in.
        .overlay { if showing != nil { Scrim { showing = nil } } }
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
            // Being asked whether to delete the agent outranks even an ask:
            // nothing down here is worth offering while the question is
            // whether this conversation is about to stop existing, and a
            // composer left under the card would be a message you could start
            // writing to something you are deleting.
            if showing == .deleteAgent {
                EmptyView()
            // An unanswered ask outranks everything else down here. Whatever
            // else is true — a machine that has gone quiet, a layer catching
            // up — the agent has stopped and is waiting on one answer, and
            // that answer is the only thing worth offering.
            } else if let panel = model.asks.panel {
                AskPanelView(panel: panel) { actions(.answer(panel, $0)) }
            } else if let changes = model.changes, !changes.isEmpty,
                      subject.finished, !deferred {
                FinishedPanel(
                    changes: changes, review: { actions(.openChanges) },
                    later: { deferred = true })
            } else if let state = ConversationFootState(
                gate: model.gate, results: model.results, subject: subject) {
                ConversationFoot(state: state) { actions(.retry) }
            } else if let composer = ComposerState(
                gate: model.gate, tail: model.tailRow, elapsed: subject.working) {
                VStack(spacing: 8) {
                    // Raised by what is being written rather than opened, so
                    // it stacks with the cards rather than replacing them:
                    // nothing can be open over the composer while a command is
                    // being typed, because typing is what closes them.
                    if let commands = SlashCommands.offered(
                        for: model.draft, facts: model.facts, provider: model.provider) {
                        SlashRows(commands: commands) { picked in
                            model.draft.pick(picked)
                            actions(.picking(picked))
                        }
                    }
                    opened
                    ComposerBox(
                        state: composer, agent: subject.name, provider: model.provider,
                        draft: Bindable(model).draft) { action in
                            // What the plus and the chip open is this screen's
                            // own state: both are about the message being
                            // written, and nothing outside has to know one is
                            // open. Pressing the same control again closes it.
                            switch action {
                            case .attach: showing = showing == .plus ? nil : .plus
                            case .openSettings: showing = showing == .settings ? nil : .settings
                            default: break
                            }
                            actions(action)
                        }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 10)
    }

    /// What this agent answers to elsewhere: "refactor-auth/studio". It is
    /// what a person copies in order to write to it from another agent, a
    /// script or a terminal, so it is the name and the machine and nothing
    /// else.
    private var address: String {
        [subject.name, subject.host?.lowercased()].compactMap { $0 }.joined(separator: "/")
    }

    /// Whatever the composer has opened over itself.
    ///
    /// One at a time, and each replaces the last: the permissions sheet opens
    /// *alone*, from the row in the plus, and a stack of cards over a
    /// conversation would leave nothing of the conversation to write about.
    @ViewBuilder
    private var opened: some View {
        switch showing {
        case .plus:
            PlusCard(permission: ProviderPermission(model.provider.permission)) { choice in
                if choice == .permissions {
                    showing = .permissions
                } else {
                    showing = nil
                }
                actions(.attaching(choice))
            }
        case .settings:
            SettingsCard(
                provider: model.provider, refusal: model.settingsGate.refusal) { change in
                    actions(.setting(change))
                }
        case .permissions:
            PermissionsCard(
                permission: ProviderPermission(model.provider.permission),
                refusal: model.settingsGate.refusal) { change in
                    actions(.setting(change))
                }
        case .overflow, .deleteAgent, nil:
            EmptyView()
        }
    }

    // MARK: - The chrome

    private var chrome: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 8) {
                pill
                Spacer(minLength: 6)
                if let changes = model.changes, !changes.isEmpty {
                    ChangesChip(changes: changes) { actions(.openChanges) }
                }
                Button {
                    showing = showing == .overflow ? nil : .overflow
                    actions(.overflow)
                } label: {
                    GlassIcon(glyph: "ellipsis")
                }
                .accessibilityLabel("More")
                .identified("conversation.overflow", label: "More")
            }
            children
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 6)
    }

    /// What this agent started, and where each one can be answered.
    ///
    /// It rides with the chrome rather than sitting in the feed because a
    /// child is a fact about the agent and not something it said: scrolling
    /// back through a long turn must not take the one waiting child off the
    /// screen. Nothing is drawn when an agent has started nothing, which is
    /// most conversations.
    @ViewBuilder
    private var children: some View {
        let roster = model.children(named: naming)
        if !roster.isEmpty {
            ScrollView(.horizontal) {
                HStack(spacing: 8) {
                    ForEach(roster) { child in
                        ChildChip(child: child, explaining: unopenable == child.id) {
                            if let agent = child.openable {
                                unopenable = nil
                                actions(.openChild(agent))
                            } else {
                                unopenable = unopenable == child.id ? nil : child.id
                            }
                        }
                    }
                }
                .padding(.horizontal, 2)
            }
            .scrollIndicators(.hidden)
            .accessibilityElement(children: .contain)
            .identified("conversation.children", value: "\(roster.count)")
            // The sentence sits under the strip rather than inside a chip: it
            // is a whole clause and a chip is a name, and a chip that grew to
            // hold it would move every chip beside it.
            if let named = unopenable,
               let sentence = roster.first(where: { $0.id == named })?.unopenable {
                Text(sentence)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background { Color.clear.frosted(
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius, style: .continuous)) }
                    .identified("conversation.child.unopenable", value: sentence)
            }
        }
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

/// One agent this one started, as a chip you can press.
///
/// An agent with a conversation of its own is a door and is drawn as one; work
/// the provider runs inside this session is not, and pressing it says so
/// rather than doing nothing. Both are pressable because both look pressable,
/// and a control that looks alive and ignores a finger reads as a broken app.
private struct ChildChip: View {
    @Environment(\.design) private var design
    let child: ChildRow
    /// Whether this chip's own reason for going nowhere is on show under the
    /// strip.
    let explaining: Bool
    let press: @MainActor () -> Void

    var body: some View {
        Button(action: press) {
            HStack(spacing: 6) {
                if child.needs != nil {
                    NeedsYouMark(glyph: glyph, size: 16)
                } else if child.openable == nil {
                    Image(systemName: explaining ? "info.circle.fill" : "info.circle")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(design.inkFaint.color)
                }
                Text(child.name)
                    .designFont(.caption, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                if let state = child.state {
                    Text(state)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 34)
            .background { Color.clear.frosted(Capsule()) }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(spoken)
        .identified("conversation.child.\(child.id)", label: spoken, value: value)
    }

    /// The accent glyph is the fleet's, so a child waiting on a permission
    /// reads here exactly as its row reads on the home.
    private var glyph: String {
        switch child.needs {
        case .permission: "hand.raised.fill"
        case .question: "questionmark"
        case .finished: "checkmark"
        case nil: "circle"
        }
    }

    private var spoken: String {
        [child.name, said, child.openable == nil ? "no conversation" : nil]
            .compactMap { $0 }.joined(separator: ", ")
    }

    /// What a driver and VoiceOver read off the chip: what it is waiting for,
    /// or the layer's own last word about it.
    private var value: String { said ?? "" }

    private var said: String? {
        switch child.needs {
        case .permission: "needs permission"
        case .question: "has a question"
        case .finished: "finished"
        case nil: child.state
        }
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
