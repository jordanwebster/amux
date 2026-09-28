import AmuxCore
import AmuxDesign
import SwiftUI

/// Who a chat is with and where it runs, from the fleet: the header paints
/// from these before the chat's own rows arrive.
public struct ChatSubject: Equatable, Sendable {
    public let name: String
    public let host: String
    public let directory: String
    public let presence: Presence
    /// Why the host is out of reach, when it is.
    public let away: Away?

    public init(name: String, host: String, directory: String, presence: Presence, away: Away?) {
        self.name = name
        self.host = host
        self.directory = directory
        self.presence = presence
        self.away = away
    }

    public var reachable: Bool { presence == .online }

    /// "~/s/amux · Studio", or why the host cannot be reached.
    public var place: String {
        if !reachable {
            switch away {
            case .revoked?: return String(localized: "\(host) · no longer trusts this phone")
            case .signedOut?: return String(localized: "\(host) · this phone is signed out")
            default: return String(localized: "\(host) · away")
            }
        }
        return PlaceNames.place(host: host.isEmpty ? nil : host, directory: directory)
    }

    /// "refactor-auth/studio": what another agent, a script or a terminal
    /// writes to reach this one.
    public var address: String {
        [name, host.isEmpty ? nil : host.lowercased()].compactMap { $0 }.joined(separator: "/")
    }
}

/// What a chat asks of whoever presented it: anything that leaves the
/// screen, reaches the clipboard, a picker, or the agent's lifecycle.
public enum ChatAction: Equatable, Sendable {
    case back
    case attach(AttachChoice)
    case rename(String)
    /// End the agent's process. Resume starts it again.
    case stopAgent
    case delete
    case copyAddress
    /// A parent or child in this chat's family.
    case open(AgentKey)
    /// The agent's uncommitted changes, from the header's changes chip.
    case review
    /// Start or stop dictating into the draft.
    case dictate
    /// Dictation was refused: open the app's page in Settings.
    case dictationSettings
}

/// What the chat has open over itself; one at a time.
public enum ChatOverlay: String, Equatable, Sendable {
    case rename
    case delete
    /// The model, effort and permission mode.
    case settings
}

/// One agent's chat.
///
/// The rows run full height under a floating header that names the agent
/// and where it runs. Exactly one thing stands at the bottom: a card being
/// asked about the agent itself, the head ask docked where the composer
/// was, a card saying why the agent cannot take a message, or the composer
/// with the queue and the facts strip above it. The draft outlives all of
/// them.
public struct ChatScreen: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let model: ChatModel
    let subject: ChatSubject
    let family: FamilyHeader?
    let actions: (ChatAction) -> Void
    @State private var showing: ChatOverlay?
    @State private var placeOpen = false
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var atNewest = true
    @State private var userScrolling = false
    /// Bumped to put the keyboard down.
    @State private var putDown = 0

    public init(
        model: ChatModel, subject: ChatSubject, family: FamilyHeader? = nil,
        showing: ChatOverlay? = nil, actions: @escaping (ChatAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.family = family
        self.actions = actions
        _showing = State(initialValue: showing)
    }

    public var body: some View {
        ZStack(alignment: .bottom) {
            Ground()
            feed
            if model.newActivity {
                Button { model.jumpToNewest() } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.down")
                        Text("New activity")
                    }
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frosted(Capsule(), as: .glass)
                }
                .buttonStyle(.amuxControl)
                .padding(.bottom, 10)
                .identified("chat.newActivity", label: String(localized: "New activity"), value: "shown")
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) { header }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ChatStanding(
                model: model, subject: subject, showing: $showing, putDown: putDown,
                actions: actions)
        }
        .toolbar(.hidden, for: .navigationBar)
        .accessibilityElement(children: .contain)
        .reported("chat", value: subject.name)
        .sheet(isPresented: $placeOpen) {
            PlaceSheet(subject: subject)
                .presentationDetents([.height(280)])
                .presentationDragIndicator(.visible)
        }
    }

    // MARK: - The header

    private var detached: Bool { model.frame?.waiting == .detached }

    private var placeLine: String {
        if detached, subject.reachable { return String(localized: "\(subject.host) · out of reach") }
        if case .exited(let cause)? = model.frame?.phase {
            return [String(localized: "Exited"), cause].compactMap { $0 }.joined(separator: " · ")
        }
        return subject.place
    }

    /// The place line is a path unless it states why the chat cannot be reached; a path keeps
    /// its ends and a sentence keeps its opening words.
    private var placeIsPath: Bool {
        if detached, subject.reachable { return false }
        if case .exited? = model.frame?.phase { return false }
        return true
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 8) {
                HStack(spacing: 8) {
                    Button {
                        putDown += 1
                        actions(.back)
                    } label: {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(design.inkMuted.color)
                            .thumbTarget(x: 15, y: 15)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Back")
                    .identified("chat.back", label: "Back")
                    .reclaimingThumbTarget(x: 15, y: 15)
                    Button {
                        putDown += 1
                        placeOpen = true
                    } label: {
                        VStack(alignment: .leading, spacing: 0) {
                            Text(subject.name)
                                .designFont(.identifier, design)
                                .foregroundStyle(design.ink.color)
                                .lineLimit(1)
                            HStack(spacing: 5) {
                                if detached || !subject.reachable {
                                    Image(systemName: "circle.dashed")
                                        .font(.system(size: 9, weight: .bold))
                                        .foregroundStyle(design.inkFaint.color)
                                }
                                Text(placeLine)
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.inkFaint.color)
                                    .lineLimit(1)
                                    .truncationMode(placeIsPath ? .middle : .tail)
                            }
                        }
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("\(subject.name), \(placeLine)")
                    .accessibilityHint("Shows the host, directory and address in full")
                    .identified("chat.title", label: subject.name, value: subject.name)
                }
                .padding(.horizontal, 13)
                .padding(.vertical, 8)
                .frosted(Capsule(), as: .glass)
                .identified(
                    "chat.header", label: placeLine,
                    value: detached ? "detached" : (subject.reachable ? "live" : "away"))
                // The pill keeps its own width but may grow up to the buttons' ordinary
                // spacing, so a long place line loses as little as it can.
                .frame(maxWidth: .infinity, alignment: .leading)
                if let changes = model.changes {
                    Button {
                        putDown += 1
                        actions(.review)
                    } label: {
                        HStack(spacing: 5) {
                            Text(verbatim: "+\(changes.added)").foregroundStyle(design.added.color)
                            Text(verbatim: "−\(changes.removed)").foregroundStyle(design.removed.color)
                        }
                        .designFont(.monoSmall, design)
                        .padding(.horizontal, 12)
                        .frame(height: 36)
                        .frosted(Capsule(), as: .glass)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel(ChatWords.changes(
                        files: changes.files, added: changes.added, removed: changes.removed))
                    .accessibilityHint("Opens the review")
                    .identified("chat.changes", label: "+\(changes.added) −\(changes.removed)", value: "shown")
                }
                overflow
            }
            if let family, family.parent != nil || !family.children.isEmpty {
                FamilyLine(family: family) { actions(.open($0)) }
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 2)
        .padding(.bottom, 6)
        // Rows scrolled up fade into the ground behind the header and the status bar
        // instead of showing crisp beside the pill. A drawn fade rather than a safe-area
        // bar's scroll edge effect: that blur is not drawn the same way twice.
        .background {
            LinearGradient(
                stops: [.init(color: design.ground.color, location: 0.6),
                        .init(color: design.ground.color.opacity(0), location: 1)],
                startPoint: .top, endPoint: .bottom)
            .ignoresSafeArea(edges: .top)
        }
    }

    private var overflow: some View {
        Menu {
            Button { showing = .rename } label: {
                Label(String(localized: "Rename"), systemImage: "pencil")
            }
            Button { actions(.copyAddress) } label: {
                Label(String(localized: "Copy Address"), systemImage: "doc.on.doc")
            }
            if model.frame.map({ if case .exited = $0.phase { false } else { true } }) ?? false {
                Button { actions(.stopAgent) } label: {
                    Label(String(localized: "Stop Agent"), systemImage: "stop.circle")
                }
            }
            Button(role: .destructive) { showing = .delete } label: {
                Label(String(localized: "Delete Agent"), systemImage: "trash")
            }
        } label: {
            GlassIcon(glyph: "ellipsis", size: 36)
                .thumbTarget(x: 4, y: 4)
        }
        .simultaneousGesture(TapGesture().onEnded { putDown += 1 })
        .accessibilityLabel("More")
        .identified("chat.more", label: "More")
        .reclaimingThumbTarget(x: 4, y: 4)
    }

    // MARK: - The rows

    private var feed: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                top
                ForEach(Array(model.ids.enumerated()), id: \.element) { index, id in
                    ChatCell(model: model, id: id)
                        .onAppear { if index < 8 { model.reachedTop() } }
                }
            }
            .scrollTargetLayout()
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 8)
        }
        .scrollIndicators(.hidden)
        .scrollEdgeEffectStyle(.soft, for: .top)
        .scrollDismissesKeyboard(.interactively)
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.containerSize.height - geometry.contentInsets.bottom
                >= geometry.contentSize.height - 24
        } action: { _, now in
            atNewest = now
            if !userScrolling, now { model.reading(atNewest: true) }
        }
        // New rows, and a change in what stands at the bottom (a card
        // opening, an ask docking, the keyboard rising), keep the newest row
        // in view for somebody following along. A chat that fits above the
        // bottom holds its top instead: a held bottom edge is applied again
        // whenever the space changes, and for content shorter than the space
        // that put the rows under the header and out of the gutter.
        .onScrollGeometryChange(for: Fit.self) { geometry in
            Fit(
                content: geometry.contentSize.height.rounded(),
                space: geometry.containerSize.height.rounded())
        } action: { _, fit in
            guard model.following, !userScrolling else { return }
            if fit.content <= fit.space {
                position.scrollTo(point: .zero)
            } else {
                position.scrollTo(edge: .bottom)
            }
        }
        .onScrollPhaseChange { _, phase in
            switch phase {
            case .tracking, .interacting:
                userScrolling = true
            case .idle:
                if userScrolling {
                    userScrolling = false
                    model.reading(atNewest: atNewest)
                }
            default:
                break
            }
        }
        .onChange(of: model.toNewest) {
            if photographed {
                position.scrollTo(edge: .bottom)
            } else {
                withAnimation(Motion.quick) { position.scrollTo(edge: .bottom) }
            }
        }
    }

    /// How tall the rows are against the space the scroll view shows them in.
    private struct Fit: Equatable {
        var content: CGFloat
        var space: CGFloat
    }

    /// Above the oldest row: older history on its way or out of reach, or
    /// while the chat is empty, the loading hint or the away notice.
    @ViewBuilder
    private var top: some View {
        switch model.paging {
        case .fetching:
            notice(String(localized: "Loading older messages…"), id: "chat.paging", value: "fetching")
        case .unreachable:
            notice(
                String(localized: "Can’t reach \(subject.host) for older messages"),
                id: "chat.paging", value: "unreachable")
        case .failed(let reason):
            notice(reason, id: "chat.paging", value: "failed")
        case .idle:
            EmptyView()
        }
        if model.ids.isEmpty, !(model.frame?.caughtUp ?? false) {
            if !subject.reachable {
                notice(awayNotice, id: "chat.away", value: subject.away.map { "\($0)" } ?? "plain")
            } else if model.loadingHint {
                notice(String(localized: "Loading this chat from \(subject.host)…"), id: "chat.loading")
            }
        }
    }

    private var awayNotice: String {
        switch subject.away {
        case .revoked?:
            String(localized: "\(subject.host) no longer trusts this phone. Pair again to see this chat.")
        case .signedOut?:
            String(localized: "This phone is signed out, so \(subject.host) is out of reach. This chat fills in when you sign in again.")
        default:
            String(localized: "\(subject.host) is away. This chat fills in when it is back.")
        }
    }

    private func notice(_ text: String, id: String, value: String = "shown") -> some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .frame(maxWidth: .infinity, alignment: .center)
            .multilineTextAlignment(.center)
            .padding(.vertical, 18)
            .identified(id, label: text, value: value)
    }
}

/// What stands at the bottom of a chat, one thing at a time: a card about
/// the agent itself, the head ask docked where the composer was, a card
/// saying why the agent cannot take a message, or the composer with the
/// queue and the facts strip above it. The draft outlives all of them.
public struct ChatStanding: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let model: ChatModel
    let subject: ChatSubject
    @Binding var showing: ChatOverlay?
    let putDown: Int
    let actions: (ChatAction) -> Void
    @FocusState private var focused: Bool

    public init(
        model: ChatModel, subject: ChatSubject, showing: Binding<ChatOverlay?> = .constant(nil),
        putDown: Int = 0, actions: @escaping (ChatAction) -> Void = { _ in }
    ) {
        self.model = model
        self.subject = subject
        _showing = showing
        self.putDown = putDown
        self.actions = actions
    }

    public var body: some View {
        standing.onChange(of: putDown) { focused = false }
    }

    @ViewBuilder
    private var standing: some View {
        VStack(spacing: 8) {
            if let notice = model.notice {
                Text(notice)
                    .designFont(.detail, design)
                    .foregroundStyle(design.accent.color)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 6)
                    .identified("chat.notice", label: notice)
            }
            switch showing {
            case .rename?:
                RenameCard(current: subject.name, cancel: { showing = nil }) { name in
                    showing = nil
                    actions(.rename(name))
                }
            case .delete?:
                DeleteCard(name: subject.name, cancel: { showing = nil }) {
                    showing = nil
                    actions(.delete)
                }
            case .settings?:
                if let settings = model.settings {
                    SettingsCard(
                        view: settings, kind: model.frame?.kind, change: model.change,
                        close: { showing = nil })
                } else {
                    composerStack
                }
            case nil:
                if let ask = model.ask, ask.state != .dismissed {
                    AskCardView(card: ask, act: answer)
                } else {
                    if let ask = model.ask { AskCardView(card: ask, act: answer) }
                    composerStack
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 10)
        .animation(photographed ? nil : Motion.standard, value: showing)
    }

    @ViewBuilder
    private var composerStack: some View {
        ChatTray(model: model)
        if let strip = model.strip { StripLine(strip: strip) }
        let matches = model.slashMatches
        if !matches.isEmpty { SlashRows(commands: matches, codex: model.frame?.kind == .codex, pick: model.pick) }
        if let signIn = model.strip?.signIn {
            FootCard(kind: "sign-in", title: ChatWords.signIn(signIn), detail: signIn.message)
        } else if let usage = model.strip?.usage, usage.blocked {
            FootCard(
                kind: "usage", title: String(localized: "Usage limit reached"),
                detail: usage.credits ?? "")
        } else {
            ComposerBox(
                model: model, placeholder: placeholder, activity: model.frame?.composer.activity,
                activitySubject: activitySubject, attach: { choice in
                    focused = false
                    actions(.attach(choice))
                }, dictate: { actions($0) }, openSettings: {
                    focused = false
                    showing = .settings
                }, focused: $focused)
        }
    }

    private var placeholder: String {
        var working = false
        if case .working? = model.frame?.phase { working = true }
        return ChatWords.placeholder(
            model.frame?.composer.mode, agent: subject.name, host: subject.host,
            away: subject.away, working: working)
    }

    private var activitySubject: String? {
        guard case .running(let key)? = model.frame?.composer.activity?.kind else { return nil }
        return model.cell(for: key).row.flatMap(ChatWords.subject(of:))
    }

    private func answer(_ action: AskAction) {
        switch action {
        case .choose(let index, let note): model.answer(choice: index, note: note)
        case .pick(let picks, let note): model.answer(picks: picks, note: note)
        case .submit(let index, let content): model.submit(choice: index, content: content)
        case .stop: model.interrupt()
        case .resend: model.resendAnswer()
        case .discard: model.discardAnswer()
        }
    }
}

/// One row cell: it observes its own row only.
private struct ChatCell: View {
    let model: ChatModel
    let id: String

    var body: some View {
        RowCellView(cell: model.cell(for: id), model: model)
    }
}

private struct RowCellView: View {
    let cell: RowCell
    let model: ChatModel

    var body: some View {
        if let row = cell.row, model.shows(row) {
            ChatRowView(
                row: row, expanded: model.isExpanded(row.id), bytes: model.bytes(of:),
                toggle: { model.toggle(row.id) })
        } else {
            Color.clear.frame(height: 0)
        }
    }
}

/// The chat's family: who started this agent and whom it started, each one
/// tap from its own chat, with a mark on any that needs the person.
private struct FamilyLine: View {
    @Environment(\.design) private var design
    let family: FamilyHeader
    let open: (AgentKey) -> Void

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                if let parent = family.parent {
                    chip(String(localized: "From \(parent.name)"), parent, glyph: "arrow.up.left")
                }
                ForEach(family.children, id: \.agent) { child in
                    chip(child.name, child, glyph: "arrow.turn.down.right")
                }
            }
        }
        .scrollIndicators(.hidden)
        .identified("chat.family", value: "\(family.children.count)")
    }

    private func chip(_ title: String, _ card: FleetCard, glyph: String) -> some View {
        Button { open(card.agent) } label: {
            HStack(spacing: 5) {
                if card.familyAttention == .needsYou || card.attention == .needsYou {
                    NeedsYouDot()
                } else {
                    Image(systemName: glyph)
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                }
                Text(title)
                    .designFont(.caption, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frosted(Capsule(), as: .glass)
        }
        .buttonStyle(.amuxControl)
        .identified(
            "chat.family.\(card.name)", label: title,
            value: card.attention == .needsYou ? "needs-you" : nil)
    }
}

/// Renaming the agent, in place of the composer.
private struct RenameCard: View {
    @Environment(\.design) private var design
    let current: String
    let cancel: () -> Void
    let confirm: (String) -> Void
    @State private var name: String
    @FocusState private var focused: Bool

    init(current: String, cancel: @escaping () -> Void, confirm: @escaping (String) -> Void) {
        self.current = current
        self.cancel = cancel
        self.confirm = confirm
        _name = State(initialValue: current)
    }

    private var trimmed: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Rename \(current)"))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            TextField(String(localized: "Name"), text: $name)
                .designFont(.mono, design)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .focused($focused)
                .submitLabel(.done)
                .onSubmit { if !trimmed.isEmpty { confirm(trimmed) } }
                .padding(10)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("chat.rename.field", value: name)
            HStack(spacing: 10) {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "chat.rename.cancel", action: cancel)
                choiceButton(
                    String(localized: "Rename"), kind: .primary, id: "chat.rename.confirm",
                    enabled: !trimmed.isEmpty
                ) { confirm(trimmed) }
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .onAppear { focused = true }
    }
}

/// Deleting the agent, with what that does named first.
private struct DeleteCard: View {
    @Environment(\.design) private var design
    let name: String
    let cancel: () -> Void
    let confirm: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Delete \(name)?"))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            consequence("checkmark", String(localized: "Its edits stay. Nothing is reverted."), kept: true)
            consequence("xmark", String(localized: "Its session ends. Unfinished work stops."), kept: false)
            consequence("xmark", String(localized: "The chat is deleted on every device."), kept: false)
            HStack(spacing: 10) {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "chat.delete.cancel", action: cancel)
                Button(action: confirm) {
                    Text("Delete")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.onAccent.color)
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .background {
                            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                                .fill(design.accent.color)
                        }
                }
                .buttonStyle(.amuxControl)
                .identified("chat.delete.confirm", label: "Delete")
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
    }

    private func consequence(_ glyph: String, _ text: String, kept: Bool) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: glyph)
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(kept ? design.inkMuted.color : design.accent.color)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
        }
    }
}

/// The host, directory and address in full, to read or copy.
private struct PlaceSheet: View {
    @Environment(\.design) private var design
    let subject: ChatSubject

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(subject.name)
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
            if !subject.host.isEmpty { fact(String(localized: "Host"), subject.host) }
            if !subject.directory.isEmpty { fact(String(localized: "Directory"), subject.directory) }
            fact(String(localized: "Address"), subject.address)
            Spacer(minLength: 0)
        }
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .identified("chat.place", label: subject.address)
    }

    private func fact(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
            Text(value)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .textSelection(.enabled)
        }
    }
}
