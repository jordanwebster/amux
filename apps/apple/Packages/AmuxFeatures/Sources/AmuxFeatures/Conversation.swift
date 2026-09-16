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
    /// Buy the relay tunnel, asked for from the machine it would reach. Not a
    /// retry: this machine will go on not answering until the account can open
    /// a tunnel to it, and nothing on this screen can do that.
    case subscribe
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
    case dictationSettings
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
    /// A new name for this agent, confirmed in the rename card. The host keeps
    /// the name, so what is drawn afterwards is what the host answers with and
    /// not what was typed.
    case renamed(String)
    /// Take the message that was waiting back into the field. Not a discard:
    /// what was queued becomes an ordinary unsent message, and abandoning it
    /// is clearing the field like any other.
    case unqueue
}

/// Something the conversation opens over itself.
///
/// These are states of this screen rather than screens beside it: what the
/// plus offers and what the overflow offers are both about the conversation
/// you are in, and neither takes you anywhere. It is a parameter as well as
/// state so a screen can be opened already showing one — which is how each is
/// photographed, how a conversation reached from a notification about a
/// permission could open on it, and how a report of a conversation with one
/// open comes back with it open.
///
/// Each is spelled, because a recording of a screen has to name what was open
/// over it in words that survive being written to a file and read back by a
/// later build.
public enum ConversationOverlay: String, Equatable, Sendable {
    case plus
    /// Model and effort, from the footer chip.
    case settings
    /// What the agent may do without asking, from the row in the plus.
    case permissions
    /// Everything the agent can be done to, from the ellipsis.
    case overflow
    /// Deleting it, with the consequences named.
    case deleteAgent
    /// Giving it another name, from the row in the overflow.
    case rename
    /// The task list, grown out of the strip above the composer. It is here
    /// with the rest because only one thing is open at a time, and because
    /// this is how a capture asks for the strip already open.
    case tasks
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
    /// Whether the relay can see the machine that owns this agent and will not
    /// carry anything to it on this account.
    ///
    /// Apart from `hostReachable` because they are opposite kinds of fact. A
    /// machine that is not answering may answer in a minute and the screen
    /// offers to ask again; a machine the relay can see will go on not
    /// answering until somebody subscribes, and asking again would never
    /// change it.
    public let hostAway: Bool
    public let readable: Bool
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
        hostReachable: Bool = true, hostAway: Bool = false, age: String? = nil,
        ended: Ended? = nil,
        finished: Bool = false, working: String? = nil, readable: Bool = true
    ) {
        self.name = name
        self.readable = readable
        self.host = host
        self.directory = directory
        self.hostReachable = hostReachable
        self.hostAway = hostAway
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
            hostAway: fleet.reach(ofHost: row.hostId) == .away,
            age: row.age(at: fleet.orderedAt),
            ended: ended,
            finished: row.attention == .needsYou(why: .finished),
            working: row.working(at: fleet.orderedAt), readable: row.readable)
    }

    /// "~/s/amux · Studio", or just the directory while the machine that
    /// owns this agent has not been heard from. Written short by
    /// ``PlaceNames``; the place sheet has both in full.
    ///
    /// A machine that has gone away says so here instead of naming the
    /// directory. The directory has not changed, but it is the least useful
    /// true thing on the screen at the moment the machine holding it cannot
    /// be reached, and this line is the one place a reader is already looking
    /// to find out where this conversation lives.
    public var place: String {
        let machine = host.map(PlaceNames.host)
        guard hostReachable else {
            return [machine, "unreachable"].compactMap { $0 }.joined(separator: " · ")
        }
        // The same substitution for the same reason: a directory on a machine
        // nothing will reach is the least useful true thing on the screen, and
        // this line is where a reader is already looking to find out why.
        if hostAway { return [machine, "away"].compactMap { $0 }.joined(separator: " · ") }
        return PlaceNames.place(host: host, directory: directory)
    }

    /// Where this agent can be written to from elsewhere: "refactor-auth/studio".
    /// It is what a person copies in order to write to it from another agent,
    /// a script or a terminal, so it is the name and the machine and nothing
    /// else.
    public var address: String {
        [name, host?.lowercased()].compactMap { $0 }.joined(separator: "/")
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
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.design) private var design
    private let model: ConversationStore
    private let subject: ConversationSubject
    /// Where a recording left the reader in this transcript, or nothing for
    /// the ordinary case of opening at the latest entry.
    private let resting: TranscriptResting?
    /// Told what this conversation has opened over itself, whenever that
    /// changes. Nothing in the shipping app listens: it exists so a build with
    /// the reporting tools in it can record a card somebody had open, which is
    /// in no message and would otherwise be lost with the screen.
    private let opening: (@MainActor (ConversationOverlay?) -> Void)?
    /// Told where the reader has come to rest in the transcript, for the same
    /// reason and by the same builds.
    private let reading: (@MainActor (TranscriptResting) -> Void)?
    /// Agent identities are stable routing keys; the fleet owns the names a
    /// person recognises in the expanded Started section.
    private let naming: (AgentId) -> String
    private let actions: @MainActor (ConversationAction) -> Void
    /// What this conversation has opened over itself, if anything.
    @State private var showing: ConversationOverlay?
    /// How tall the page is, and how tall what is standing at the bottom of it
    /// wants to be. Both are measured rather than assumed because the answer
    /// is the reader's: at the largest accessibility size an unanswered ask is
    /// taller than the display, and a bottom inset that asks for more room
    /// than there is does not overflow — SwiftUI squeezes every inset on the
    /// view instead, so the pill at the top loses two thirds of its height and
    /// draws its name straight through the glass, and every line on the card
    /// below collapses to one truncated line. Measuring is what lets the card
    /// scroll inside what there is instead.
    @State private var pageHeight: CGFloat = 0
    @State private var footHeight: CGFloat = 0
    /// Whether the sheet with the machine, directory and address in full is
    /// up. A system sheet rather than one of the cards above: it is read and
    /// put away, and nothing on the conversation waits on it.
    @State private var placeOpen = false


    public init(
        model: ConversationStore,
        subject: ConversationSubject,
        showing: ConversationOverlay? = nil,
        resting: TranscriptResting? = nil,
        opening: (@MainActor (ConversationOverlay?) -> Void)? = nil,
        reading: (@MainActor (TranscriptResting) -> Void)? = nil,
        naming: @escaping (AgentId) -> String = { $0.description },
        actions: @escaping @MainActor (ConversationAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.resting = resting
        self.opening = opening
        self.reading = reading
        self.naming = naming
        self.actions = actions
        _showing = State(initialValue: showing)
    }

    public var body: some View {
        ZStack {
            Ground()
            transcript
            // A conversation overlay pushes the page back edge to edge. The
            // chrome and bottom controls are safe-area insets on this stack,
            // so they remain in front of the scrim as floating surfaces.
            if dimsPage {
                Scrim { showing = nil }
            }
            // The overflow hangs from the control that opened it. Bottom
            // cards live with the composer in the safe-area inset below.
            if showing == .overflow {
                OverflowMenu(address: address) { choice in
                    switch choice {
                    case .rename: showing = .rename
                    case .delete: showing = .deleteAgent
                    // Copying happens outside this screen and leaves nothing
                    // open behind it: the menu did what it said it would.
                    case .copyAddress: showing = nil
                    }
                    actions(.overflowing(choice))
                }
                .padding(.horizontal, design.metrics.gutter)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                // The outer safe-area inset has already reserved the floating
                // chrome, so this is the reference's eight-point attachment
                // below the controls rather than a duplicated chrome height.
                .padding(.top, 8)
                // Out of the control that opened it. The ellipsis does not
                // move while this arrives, which is what makes it read as
                // hanging from the button rather than replacing it.
                .transition(.growing(from: .topTrailing))
            }
        }
        .moving(value: showing)
        .safeAreaInset(edge: .top, spacing: 0) { chrome }
        .safeAreaInset(edge: .bottom, spacing: 0) { foot }
        // Measure the stable outer page rather than the transcript viewport.
        // The bottom inset reduces that viewport; using the reduced height to
        // cap the inset makes each side resize the other at accessibility
        // text sizes and can leave SwiftUI cycling between two layouts.
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { pageHeight = $0 }
        // A name put on a container is handed down to everything under it the
        // system does not already treat as its own, so without this the pill,
        // the chip and every row answer to "conversation" — for VoiceOver and
        // for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("conversation", value: model.agent.description)
        // Said here rather than at each control that opens something, because
        // several of them close one card by opening another and two of them —
        // rename and delete — are reached from inside the overflow and never
        // pass through an action at all. What is open is one piece of state,
        // so what is open is what is reported.
        .onChange(of: showing) { _, now in opening?(now) }
        .sheet(isPresented: $placeOpen) {
            PlaceSheet(subject: subject)
                .presentationDetents([.height(PlaceSheet.height)])
                .presentationDragIndicator(.visible)
        }
    }

    /// The feed, under the chrome rather than beside it.
    ///
    /// The inset is what puts the pill over the scroll view instead of above
    /// it: the content keeps clear of the chrome when it is at rest and travels
    /// underneath it when it scrolls, which is the only arrangement in which
    /// frosting the top edge means anything.
    private var transcript: some View {
        ConversationTranscript(
            model: model, subject: subject, resting: resting, reading: reading)
        // The platform's effect, not a hand-drawn plate. Masking a glass layer
        // to make it fade stops it sampling what is behind it, so it renders
        // as a pane you can read straight through; this samples correctly.
        // The system safe area belongs to the floating chrome, not to the
        // feed behind it. Extending the scroll view to the physical top lets
        // its soft edge effect fade continuously behind the status region
        // instead of starting at the chrome's lower boundary.
        .ignoresSafeArea(edges: .top)
        .scrollEdgeEffectStyle(.soft, for: .top)
    }

    /// The most of the page the foot may take. The rest belongs to the pill
    /// and to whatever the agent last said, and a conversation whose whole
    /// screen is the answer buttons is not a conversation.
    private var footCap: CGFloat {
        pageHeight > 0 ? pageHeight * 0.84 : .infinity
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
        // At every ordinary text size what stands at the bottom is drawn
        // exactly as it is written, and the branch below is not taken.
        //
        // At an accessibility size it can be taller than the display — an
        // unanswered ask is a headline, a command, a reason and three buttons,
        // all set at the reader's size — and a bottom inset that asks for more
        // room than there is does not overflow. SwiftUI squeezes every inset
        // on the view instead: the pill at the top loses two thirds of its
        // height and draws its name straight through the glass, and every line
        // on the card collapses to one truncated line. So the card is given
        // what there is and scrolls inside it, which is the one arrangement in
        // which nothing on it is cut off.
        if typeSize.isAccessibilitySize {
            ScrollView {
                standing
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: {
                        footHeight = $0
                    }
            }
            .scrollBounceBehavior(.basedOnSize)
            .scrollDisabled(footHeight <= footCap)
            .frame(height: footHeight > 0 ? min(footHeight, footCap) : nil)
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
        } else {
            standing
                .padding(.horizontal, 12)
                .padding(.bottom, 10)
        }
    }

    @ViewBuilder
    private var standing: some View {
        ConversationStanding(
            model: model, subject: subject, showing: $showing,
            naming: naming, actions: actions)
    }

    /// Goes somewhere else, keyboard first.
    ///
    /// The composer is often being written in when a person reaches for the
    /// patch, a child or the fleet, and the keyboard it raised does not belong
    /// to any of those. Left standing it outlives this screen and covers the
    /// next one — including this one on the way back, which is rebuilt while
    /// the keys are already there and so is laid out as though the bottom of
    /// the display were free. Putting it down here rather than in a lifecycle
    /// callback ties it to the press, which is the one moment that is certain.
    private func leaving(_ action: ConversationAction) {
        Keyboard.putDown()
        actions(action)
    }

    private var address: String { subject.address }

    /// Surfaces opened over the conversation push its page back. Growing the
    /// facts strip does not: it is part of the conversation's bottom content.
    private var dimsPage: Bool {
        switch showing {
        case .overflow, .plus, .settings, .permissions, .deleteAgent, .rename:
            true
        case .tasks, nil:
            false
        }
    }

    // MARK: - The chrome

    private var chrome: some View {
        HStack(alignment: .center, spacing: 8) {
            pill
            Spacer(minLength: 6)
            if let changes = model.changes, !changes.isEmpty {
                ChangesChip(changes: changes) { leaving(.openChanges) }
            }
            Button {
                showing = showing == .overflow ? nil : .overflow
                actions(.overflow)
            } label: {
                GlassIcon(glyph: "ellipsis", size: 36)
                    .thumbTarget(x: 4, y: 4)
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("More")
            .identified("conversation.overflow", label: "More")
            .reclaimingThumbTarget(x: 4, y: 4)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, typeSize.isAccessibilitySize ? 6 : 2)
        // Glass over a sliver of transcript says "this floats". Glass over a
        // third of the display says nothing and leaves half-read words behind
        // every letter of the name, so at an accessibility size the chrome
        // stands on the ground instead and the feed begins under it.
        .background {
            if typeSize.isAccessibilitySize {
                design.ground.color.ignoresSafeArea(edges: .top)
            }
        }
        .dynamicTypeSize(...DynamicTypeSize.accessibility1)
    }

    /// The agent, its machine and its directory, on one floating surface with
    /// the way out on its leading edge.
    ///
    /// The drawer control is inside the pill rather than beside it because the
    /// two belong together: the pill says which conversation you are in, and
    /// the control is how you go to another one.
    private var pill: some View {
        Group {
            if typeSize.isAccessibilitySize {
                accessiblePill
            } else {
                HStack(spacing: 8) {
                    Button { leaving(.openDrawer) } label: {
                        Image(systemName: "sidebar.left")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(design.inkMuted.color)
                            .thumbTarget(x: 15, y: 15)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Agents")
                    .identified("conversation.drawer", label: "Agents")
                    .reclaimingThumbTarget(x: 15, y: 15)
                    placeButton(grow: 8) { subjectLabel }
                }
                .padding(.horizontal, 13)
                .padding(.vertical, 8)
                .frosted(Capsule(), as: .control)
            }
        }
        .accessibilityElement(children: .contain)
        .identified(
            "conversation.subject", label: "\(subject.name), \(subject.place)",
            value: subject.name)
    }

    private var accessiblePill: some View {
        HStack(spacing: 10) {
            Button { leaving(.openDrawer) } label: {
                Image(systemName: "sidebar.left")
                    .font(.system(size: 17, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("Agents")
            .identified("conversation.drawer", label: "Agents")
            placeButton(grow: 0) { subjectLabel }.padding(.trailing, 14)
        }
        .padding(.leading, 2)
        .frame(minHeight: 52)
        .frosted(Capsule(), wash: 1, as: .control)
    }

    private var subjectLabel: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(subject.name)
                .designFont(.identifier, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
            Text(subject.place)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    /// The name and the short place, pressed for the long ones.
    /// `grow` reaches the pill's own edges, so the whole height of the pill
    /// beside the drawer control answers the press without the pill growing.
    private func placeButton<Label: View>(
        grow: CGFloat, @ViewBuilder label: () -> Label
    ) -> some View {
        Button {
            Keyboard.putDown()
            placeOpen = true
        } label: {
            label().thumbTarget(y: grow)
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel("\(subject.name), \(subject.place)")
        .accessibilityHint("Shows the host, directory and address in full")
        .identified("conversation.place", label: "\(subject.name), \(subject.place)")
        .reclaimingThumbTarget(y: grow)
    }
}

/// The observable boundary for everything standing above the home indicator.
///
/// Draft edits, session gates and task facts belong here. Keeping them out of
/// the conversation root means clearing a sent draft does not rebuild the
/// chrome or re-evaluate the thousand-row transcript beside this control.
private struct ConversationStanding: View {
    let model: ConversationStore
    let subject: ConversationSubject
    @Binding var showing: ConversationOverlay?
    let naming: (AgentId) -> String
    let actions: @MainActor (ConversationAction) -> Void

    @ViewBuilder
    var body: some View {
        Group {
            // Being asked whether to delete the agent outranks even an ask:
            // nothing down here is worth offering while the question is
            // whether this conversation is about to stop existing, and a
            // composer left under the card would be a message you could start
            // writing to something you are deleting. Renaming is the same:
            // one field at a time.
            if showing == .deleteAgent {
                DeleteAgentCard(
                    name: subject.name,
                    cancel: { showing = nil },
                    confirm: {
                        showing = nil
                        actions(.deleteAgent)
                    })
            } else if showing == .rename {
                RenameCard(
                    current: subject.name,
                    cancel: { showing = nil },
                    confirm: { name in
                        showing = nil
                        actions(.renamed(name))
                    })
            } else if let panel = model.asks.panel {
                AskPanelView(panel: panel) { actions(.answer(panel, $0)) }
            } else if let state = ConversationFootState(
                gate: model.gate, refusal: model.refusal, subject: subject) {
                ConversationFoot(
                    state: state, retry: { actions(.retry) },
                    subscribe: { actions(.subscribe) })
            } else if let composer = ComposerState(
                gate: model.gate, tail: activityTail, elapsed: subject.working) {
                ConversationComposerStanding(
                    model: model, subject: subject, state: composer,
                    showing: $showing, naming: naming, actions: actions)
            }
        }
        .moving(value: showing)
    }

    /// Only a running turn reads the transcript tail. A ready composer does
    /// not derive anything from it, so arriving rows do not invalidate the
    /// controls during the common streaming case.
    private var activityTail: TranscriptRow? {
        switch model.gate {
        case .claudePty(.working), .claudeSdk(.working), .codex(.activeTurn):
            model.tailRow
        default:
            nil
        }
    }

}

/// Draft-sized invalidations stop here. The surrounding footer chooses which
/// state exists; this view handles the state that changes with every edit.
private struct ConversationComposerStanding: View {
    let model: ConversationStore
    let subject: ConversationSubject
    let state: ComposerState
    @Binding var showing: ConversationOverlay?
    let naming: (AgentId) -> String
    let actions: @MainActor (ConversationAction) -> Void

    var body: some View {
        VStack(spacing: 8) {
            strip
            ConversationDraftCommands(model: model, actions: actions)
            opened
            ConversationComposerBox(
                model: model, state: state, agent: subject.name,
                showing: $showing, actions: actions)
        }
    }

    @ViewBuilder
    private var strip: some View {
        let facts = ConversationFacts(model)
        let children = model.children(named: naming)
        if !facts.isEmpty {
            FactsStrip(
                facts: facts, children: children, open: showing == .tasks,
                grow: { showing = showing == .tasks ? nil : .tasks },
                openChild: { child in
                    if let agent = child.openable { leaving(.openChild(agent)) }
                },
                unqueue: {
                    showing = nil
                    actions(.unqueue)
                })
        }
    }

    @ViewBuilder
    private var opened: some View {
        switch showing {
        case .plus:
            PlusCard(permission: ProviderPermission(model.provider.permission)) { choice in
                showing = choice == .permissions ? .permissions : nil
                actions(.attaching(choice))
            }
            .transition(Self.growingFromTheFooter)
        case .settings:
            SettingsCard(
                provider: model.provider, refusal: model.settingsGate.refusal) { change in
                    actions(.setting(change))
                }
                .transition(Self.growingFromTheFooter)
        case .permissions:
            PermissionsCard(
                permission: ProviderPermission(model.provider.permission),
                refusal: model.settingsGate.refusal) { change in
                    actions(.setting(change))
                }
                .transition(Self.growingFromTheFooter)
        case .overflow, .deleteAgent, .rename, .tasks, nil:
            EmptyView()
        }
    }

    /// Out of the footer's leading corner, where the plus and the model chip
    /// both are. The card still takes its row above the composer — it must
    /// not cover the field it is about to be used with — but it grows from
    /// the control rather than appearing at arm's length from it.
    /// Built from a modifier whose resting state wraps nothing, rather than
    /// from `.scale`. A scale transition leaves its transform on the view it
    /// settled, and a card sits on screen far longer than the quarter second
    /// it takes to arrive: on the permissions card, whose rows wrap, that was
    /// enough to round its height differently and shift everything under it.
    private static let growingFromTheFooter: AnyTransition = .growing(from: .bottomLeading)

    private func leaving(_ action: ConversationAction) {
        Keyboard.putDown()
        actions(action)
    }
}

private struct ConversationDraftCommands: View {
    let model: ConversationStore
    let actions: @MainActor (ConversationAction) -> Void

    @ViewBuilder
    var body: some View {
        if let commands = SlashCommands.offered(
            for: model.draft, facts: model.facts, provider: model.provider) {
            SlashRows(commands: commands) { picked in
                model.draft.pick(picked)
                actions(.picking(picked))
            }
        }
    }
}

private struct ConversationComposerBox: View {
    let model: ConversationStore
    let state: ComposerState
    let agent: String
    @Binding var showing: ConversationOverlay?
    let actions: @MainActor (ConversationAction) -> Void

    var body: some View {
        ComposerBox(
            state: state, agent: agent, provider: model.provider,
            draft: Bindable(model).draft, dictation: model.dictation) { action in
                // A card opened from the footer grows above the box, and with
                // the keyboard still up from the last message there is no
                // conversation left to see or to press to put it away. The
                // keyboard comes back with the next tap in the field.
                switch action {
                case .attach:
                    if showing != .plus { Keyboard.putDown() }
                    showing = showing == .plus ? nil : .plus
                case .openSettings:
                    if showing != .settings { Keyboard.putDown() }
                    showing = showing == .settings ? nil : .settings
                default: break
                }
                actions(action)
            }
    }
}

/// The observable transcript boundary.
///
/// A row arriving changes this view, not the conversation that owns it. The
/// surrounding chrome and composer have their own state and should not be
/// rebuilt fifty times a second just because the lazy feed gained a child.
private struct ConversationTranscript: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.design) private var design
    let model: ConversationStore
    let subject: ConversationSubject
    let resting: TranscriptResting?
    let reading: (@MainActor (TranscriptResting) -> Void)?

    var body: some View {
        let rows = withUnreadable(model.confirmedRows())
        TranscriptContainer(resting: resting, moved: reading, tail: rows.last?.id) {
            if !subject.readable {
                UnsupportedLayer(layer: "this agent’s transcript")
                    .padding(.top, design.metrics.feedGap)
            } else if typeSize.isAccessibilitySize, model.asks.panel != nil {
                // The ask repeats the command and reason it needs. At large
                // type, a clipped fragment of the feed behind the fixed pill
                // provides no context and makes both surfaces harder to read.
                EmptyView()
            } else {
                TranscriptFeed(rows: rows)
            }
            // A finished run is the last event in the feed, not a screen
            // state placed under it.
            if let ended = subject.ended {
                EndOfRun(ended: ended, age: subject.age, host: subject.host)
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.top, design.metrics.feedGap)
            }
        }
        // A local send occupies the same bottom edge its confirmed row will
        // inherit, without changing the lazy history merely to show one
        // optimistic bubble. When the host echoes it, the inset disappears
        // as the identical row arrives at the anchored tail.
        .overlay(alignment: .bottom) {
            if subject.readable,
               !(typeSize.isAccessibilitySize && model.asks.panel != nil) {
                PendingTranscriptFeed(model: model)
            }
        }
    }
}

extension ConversationTranscript {
    /// The feed, with a row at its foot for each update about this agent that
    /// could not be read. What such an update carried is missing from the
    /// rows above, and saying so where the reader is looking is the difference
    /// between a conversation that admits a gap and one that looks current.
    fileprivate func withUnreadable(_ rows: [TranscriptRow]) -> [TranscriptRow] {
        guard !model.unreadable.isEmpty else { return rows }
        return rows + model.unreadable.enumerated().map { index, unread in
            TranscriptRow(
                id: "unreadable-update-\(index)", layer: rows.last?.layer ?? .claudePty,
                kind: .unreadable(label: "\(unread.kind.lowercased()) update"))
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
            .background { Color.clear.frosted(Capsule(), as: .control) }
            .contentShape(Capsule())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(
            "Review \(changes.insertions) added, \(changes.deletions) removed")
        .identified(
            "conversation.changes",
            label: "Review \(changes.insertions) added, \(changes.deletions) removed",
            value: "\(insertions) \(deletions)")
    }
}

/// The scale half of a surface's arrival, applied only while it is arriving.
///
/// `.scale` would be the obvious way to write this, and it leaves its
/// transform on the view it settled — which for anything that stays open is
/// almost all of the time it is on screen. Here the resting state wraps
/// nothing at all.
struct Growing: ViewModifier {
    let growing: Bool
    let anchor: UnitPoint

    @ViewBuilder
    func body(content: Content) -> some View {
        if growing {
            content.scaleEffect(0.94, anchor: anchor)
        } else {
            content
        }
    }
}

extension AnyTransition {
    /// Out of the control that opened it.
    static func growing(from anchor: UnitPoint) -> AnyTransition {
        .modifier(
            active: Growing(growing: true, anchor: anchor),
            identity: Growing(growing: false, anchor: anchor)
        ).combined(with: .opacity)
    }
}
