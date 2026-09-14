import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened on the Agents home. The screen never navigates and never
/// reaches the network; it says what the person did and the shell decides
/// where that leads.
public enum HomeAction: Equatable, Sendable {
    case open(AgentId)
    case newAgent
    case switchAccount(AccountId)
    case addAccount
    case signIn
    case subscribe
    /// Pair with a machine — the named one where the home is offering it, or
    /// whichever one this phone has found where it is not.
    case pair(HostId?)
    case openExceptions
    /// The list was pulled, or the screen came back into view. Only then may
    /// the ordering regroup.
    case refresh
}

/// The screen the app opens onto.
///
/// A row's only job is to be worth opening or worth skipping. Nothing here
/// decides anything about an agent: a decision belongs to the conversation
/// that owns it, not to a button on a list.
///
/// The screen draws its own header rather than using the system navigation
/// bar. The design's header is a large title with a live subtitle under it and
/// round glass controls beside it, and a navigation bar cannot hold that
/// without fighting it on every scroll.
public struct AgentsHome: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    private let model: FleetStore
    private let accounts: AccountRegistry
    /// The machines, for the one thing this screen says about them: a phone
    /// with no agents yet is a phone that has not paired, and what it has
    /// found on its network is the shortest way out of that.
    private let hosts: HostsStore
    private let actions: @MainActor (HomeAction) -> Void
    /// The fold is view state, not fleet state: opening it is a thing this
    /// screen is doing, and coming back to the screen starts it closed again.
    @State private var foldOpen = false
    /// Whether the account switcher is out. View state for the same reason,
    /// and handed in only so a capture can ask for the panel: which accounts
    /// this phone has is a fact, having the list open is not.
    @State private var switcherOpen: Bool
    @State private var filter: HomeFilter = .all
    @State private var choosingFilter = false

    public init(
        model: FleetStore,
        accounts: AccountRegistry,
        hosts: HostsStore,
        accountsOpen: Bool = false,
        actions: @escaping @MainActor (HomeAction) -> Void
    ) {
        self.model = model
        self.accounts = accounts
        self.hosts = hosts
        self.actions = actions
        _switcherOpen = State(initialValue: accountsOpen)
    }

    public var body: some View {
        SwitcherOverlay(
            open: $switcherOpen, accounts: accounts, actions: switcher
        ) {
            home
        }
    }

    private var home: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                header
                // The list whenever there is one. An account is never what
                // decides this: a phone that is signed out and paired with a
                // machine on its own network has a fleet worth reading, and a
                // phone that remembers agents it cannot refresh has one too.
                // What it cannot do is said on the exceptions line.
                if model.rows.isEmpty {
                    nothingPairedYet
                } else {
                    fleet
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .onAppear { actions(.refresh) }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("home", value: accounts.gate.name)
    }

    // MARK: - Header

    /// Whether anybody is signed in on this phone. An account listed with Sign
    /// In beside it is remembered, not signed in.
    private var signedIn: Bool { accounts.accounts.contains(where: \.signedIn) }

    /// Whether there is an account question on this phone worth putting under
    /// the title: another account to read, or nobody signed in at all.
    private var switchable: Bool {
        accounts.accounts.count > 1 || !signedIn
    }

    /// Whether any machine would actually run something started now.
    private var canStartAnAgent: Bool {
        model.hosts.values.contains { model.reach(of: $0).live }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            if switchable { accountDisc }
            VStack(alignment: .leading, spacing: 1) {
                title
                Text(subtitle)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .identified("home.subtitle", value: subtitle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // Starting an agent needs a machine something can be started on.
            // Not an account — a phone paired with a host on its own network
            // has one — and not a subscription, which buys the route to a
            // machine rather than the right to use it. Without one the button
            // would open onto a screen with nothing on it.
            if canStartAnAgent {
                HStack(spacing: 8) {
                    if !switcherOpen {
                        Button { choosingFilter = true } label: {
                            GlassIcon(glyph: "line.3.horizontal.decrease")
                                .thumbTarget(x: 5, y: 5)
                        }
                        .buttonStyle(.amuxControl)
                        .accessibilityLabel("Filter Agents")
                        .identified("home.filter", label: "Filter Agents", value: filter.rawValue)
                        .reclaimingThumbTarget(x: 5, y: 5)
                        .confirmationDialog(
                            "Show Agents", isPresented: $choosingFilter,
                            titleVisibility: .visible
                        ) {
                            ForEach(HomeFilter.allCases) { choice in
                                Button(choice.title) { filter = choice }
                            }
                        }
                    }
                    Button { actions(.newAgent) } label: {
                        GlassIcon(glyph: "plus", prominent: true)
                            .thumbTarget(x: 5, y: 5)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("New Agent")
                    .identified("home.newAgent", label: "New Agent")
                    .reclaimingThumbTarget(x: 5, y: 5)
                }
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
    }

    /// The title carries the account menu only when there is an account
    /// question to answer — a second account to switch to, or no usable
    /// account at all. On a working phone with one account, the title is a
    /// title and the account lives under You.
    @ViewBuilder
    private var title: some View {
        if switchable {
            Button { switcherOpen.toggle() } label: {
                HStack(spacing: 5) {
                    Text("Agents")
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(design.inkFaint.color)
                }
                .thumbTarget(y: 7)
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("Agents, switch account")
            .identified(
                "home.title", label: "Agents, switch account",
                value: switcherOpen ? "open" : "Agents")
            .reclaimingThumbTarget(y: 7)
        } else {
            Text("Agents")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("home.title", value: "Agents")
        }
    }

    /// What the switcher asked for, in the words this screen speaks.
    ///
    /// Switching, adding and signing back in all leave the screen, because
    /// none of them is something a list can do; putting the panel away is the
    /// one thing this screen decides for itself.
    private func switcher(_ action: AccountsAction) {
        switch action {
        case .select(let id):
            switcherOpen = false
            actions(.switchAccount(id))
        case .add:
            switcherOpen = false
            actions(.addAccount)
        case .signIn:
            switcherOpen = false
            actions(.signIn)
        case .dismiss:
            switcherOpen = false
        // Nothing else the panel can say reaches this screen: the rest of an
        // account's actions live under You, where there is room to state what
        // they do.
        case .signOut, .delete, .subscription, .appearance, .identity, .support, .report:
            break
        }
    }

    /// Hollow rather than absent. An account is a thing this app has; not
    /// having a usable one yet is a state, not a hole.
    @ViewBuilder
    private var accountDisc: some View {
        if let initials = accounts.selectedAccount.map(initials(of:)) {
            Circle()
                .fill(design.accent.color)
                .frame(width: 32, height: 32)
                .overlay(
                    Text(initials)
                        .font(.system(size: 32 * 0.36, weight: .semibold))
                        .foregroundStyle(design.onAccent.color))
                .accessibilityHidden(true)
        } else {
            Circle()
                .strokeBorder(design.inkFaint.color, lineWidth: 1.5)
                .frame(width: 32, height: 32)
                .overlay(
                    Image(systemName: "person")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(design.inkFaint.color))
                .accessibilityHidden(true)
        }
    }

    private func initials(of entry: AccountEntry) -> String {
        let source = entry.account.displayName ?? entry.account.email
        let words = source.split(whereSeparator: { !$0.isLetter })
        let letters = words.prefix(2).compactMap(\.first)
        return letters.isEmpty ? "?" : String(letters).uppercased()
    }

    /// The subtitle counts what is on the list whenever there is a list to
    /// count. It only becomes the account's word when the account is the whole
    /// screen, because otherwise the same fact would be said twice: once here
    /// and once on the exceptions line above the rows.
    private var subtitle: String {
        if accounts.accounts.count > 1, let entry = accounts.selectedAccount {
            let waiting = model.sections.first { $0.kind == .needsYou }?.rows.count ?? 0
            return waiting == 0
                ? "\(entry.name) · nothing needs you"
                : "\(entry.name) · \(waiting) need you"
        }
        if !model.rows.isEmpty { return model.subtitle }
        // Not "Not signed in" and not "Not subscribed". A phone with no agents
        // is a phone that has paired with nothing, whatever its account is
        // doing: amux is free on the network this phone is already on, so an
        // empty list is never evidence that something has to be bought.
        return "Nothing paired yet"
    }

    // MARK: - The list

    private var sections: [FleetSection] {
        switch filter {
        case .all:
            model.sections
        case .needsYou:
            model.sections.filter { $0.kind == .needsYou }
        case .running:
            [FleetSection(
                kind: .everythingElse, title: "Running",
                rows: model.rows.filter { $0.phase == .running }, folded: false)]
        }
    }

    private var fleet: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if let exceptions {
                    exceptionsLine(exceptions.text, exceptions.act)
                }
                ForEach(sections) { section in
                    VStack(alignment: .leading, spacing: 8) {
                        if section.kind != .older {
                            SectionHead(
                                title: section.title,
                                trailing: section.kind == .needsYou
                                    ? "\(section.rows.count)" : nil)
                        }
                        if section.folded && !foldOpen {
                            fold(section)
                        } else {
                            RowGroup(items: section.rows, prominence: .subject) { row in
                                agentRow(row)
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 6)
            .padding(.bottom, 120)
            // Keyed on which rows are where and nothing else. A row is
            // equatable over its whole card, so animating on the sections
            // themselves would set the list moving every time an agent
            // changed its headline or aged by a minute. Safe because
            // regrouping only happens on a refresh, which is something the
            // reader did.
            .moving(value: sections.map { $0.rows.map(\.id) })
        }
        .scrollIndicators(.hidden)
        .refreshable { actions(.refresh) }
    }

    @ViewBuilder
    private func agentRow(_ row: AgentRow) -> some View {
        let host = model.host(row.hostId)
        let state = RowState(row: row, host: host, reach: model.reach(ofHost: row.hostId))
        let content = AgentRowView(
            row: row, state: state, host: host?.name, now: model.orderedAt)
        // An agent run by a provider this build has no case for is listed and
        // not offered to open. A button that led to a conversation of which
        // not one row could be read would be a worse answer than the row
        // saying so where it stands.
        if row.readable {
            Button { actions(.open(row.id)) } label: { content }
                .buttonStyle(.amuxRow)
                .accessibilityLabel(spoken(row, state))
                .identified(
                    "home.row.\(row.id)", label: spoken(row, state),
                    value: row.confirmed ? state.name : "\(state.name), remembered")
        } else {
            content
                .accessibilityElement(children: .combine)
                .accessibilityLabel(spoken(row, state))
                .identified("home.row.\(row.id)", label: spoken(row, state), value: state.name)
        }
    }

    /// What a row says to somebody who cannot see it, in the order the row
    /// says it: who, what, where, how long, and what it needs.
    private func spoken(_ row: AgentRow, _ state: RowState) -> String {
        var parts = [row.name]
        if let headline = row.headline { parts.append(headline) }
        if let said = state.spoken { parts.append(said) }
        if case .finished(let outcome) = state, let outcome { parts.append(outcome.arithmetic) }
        parts.append([model.host(row.hostId)?.name, row.workingDirectory]
            .compactMap { $0 }.joined(separator: ", "))
        parts.append(row.age(at: model.orderedAt) + " ago")
        if row.unread { parts.append("unread") }
        // Said aloud too: a row nobody has confirmed yet looks different and
        // must sound different, or VoiceOver reports a memory as a fact.
        if !row.confirmed { parts.append("remembered, not confirmed yet") }
        return parts.joined(separator: ", ")
    }

    /// A section worth naming but not worth listing, until you say otherwise.
    ///
    /// Nothing is hidden — the names are on the line and one tap opens it.
    /// Work that has been quiet for a day is still work, and deleting it from
    /// the screen to keep the screen short is how a list starts lying about
    /// what exists.
    private func fold(_ section: FleetSection) -> some View {
        let names = section.rows.map(\.name).joined(separator: ", ")
        let title = "\(section.title) · \(section.rows.count)"
        return Button {
            foldOpen = true
        } label: {
            Surface {
                HStack(spacing: 10) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(design.inkFaint.color)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title)
                            .designFont(.detail, design)
                            .foregroundStyle(design.ink.color)
                        Text(names)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .padding(13)
                .frame(minHeight: 44)
            }
        }
        .buttonStyle(.amuxRow(cornerRadius: design.metrics.cardRadius))
        .accessibilityLabel("\(title), \(names)")
        .identified("home.fold.\(section.id)", label: "\(title), \(names)", value: names)
    }

    /// The one line a home is allowed above the list.
    ///
    /// It is not a summary — a summary is a card that must be filled, so it
    /// fills itself with counts nobody asked for. It is an exceptions line: it
    /// appears only when something is actually wrong, it takes one row when it
    /// does, and when everything is fine the top of the screen is the list.
    ///
    /// The order is the one the desktop's banner follows, and for the same
    /// reason: a connection that is down outranks everything, then a machine
    /// the relay can see and this account may not reach, then a machine
    /// nothing can reach on a phone with no account, and last the plain fact
    /// that a machine is offline. A phone that can reach every machine it owns
    /// shows none of it and is never asked for an account.
    private var exceptions: (text: String, act: HomeAction)? {
        // A link that is down outranks everything — on a phone that has one.
        // Nobody signed in means no relay was ever dialled, and a phone told
        // it was offline would be told about the absence of something it never
        // had; what is actually true about such a phone is said below, machine
        // by machine.
        if signedIn, model.connection.state == .disconnected, let sentence = model.exceptions {
            return (sentence, .openExceptions)
        }
        if let away = model.awayHost {
            return ("\(away) is away · subscribe to reach your agents from anywhere", .subscribe)
        }
        // With nobody signed in there is one thing worth saying and one thing
        // to do about it: a machine this phone cannot reach, and the account
        // that would reach it from somewhere else. Everything else a link
        // could report belongs to a phone that has one.
        guard signedIn else {
            return model.unreachableHost == nil ? nil : (SignInCopy.caption, .signIn)
        }
        return model.exceptions.map { ($0, .openExceptions) }
    }

    private func exceptionsLine(_ text: String, _ act: HomeAction) -> some View {
        Button {
            actions(act)
        } label: {
            Surface {
                HStack(spacing: 11) {
                    Image(systemName: glyph(for: act))
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(design.inkMuted.color)
                        .frame(width: 18)
                    // One row when it fits, which is what an exceptions line
                    // is for. At an accessibility size the sentence does not
                    // fit on one, and a sentence about the one thing that is
                    // wrong is worth more than the row it was promised, so it
                    // wraps.
                    Text(text)
                        .designFont(.detail, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(typeSize.isAccessibilitySize ? 4 : 1)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 4)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                }
                .padding(.horizontal, 13)
                .padding(.vertical, 11)
                .frame(minHeight: 44)
            }
        }
        .buttonStyle(.amuxRow(cornerRadius: design.metrics.cardRadius))
        .accessibilityLabel(text)
        .identified("home.exceptions", label: text, value: text)
    }

    /// What the line is about: a link that is down, or an account that would
    /// open one.
    private func glyph(for act: HomeAction) -> String {
        switch act {
        case .signIn, .subscribe: "person.slash"
        default: "wifi.slash"
        }
    }

    // MARK: - Nothing to reach yet

    /// Not a splash: the real home screen, empty.
    ///
    /// A splash would teach that this is a service you subscribe to; an empty
    /// list teaches that it is a client for machines you own. So the one
    /// action is pairing, and an account is offered under it as what pairing
    /// is not: reaching those machines when you are not on their network. A
    /// phone that has already found something on this network leads with that
    /// — there is nothing to type and nothing to sign into, and the machine is
    /// right there.
    ///
    /// There is one of these and not two. The screen that used to stand here
    /// for an account with nothing bought said "Subscribe to pair one", which
    /// was not true: pairing with a machine on this network has never needed a
    /// subscription, and an empty phone that was told otherwise would have
    /// paid to do something already free.
    private var nothingPairedYet: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("No agents yet")
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                        .identified("home.empty.firstRun", value: "No agents yet")
                    Explain("Pair with a host and its agents appear here.")
                        .identified("home.empty.explain")
                }
                if !found.isEmpty { offers }
                Button { actions(.pair(found.count == 1 ? found[0].id : nil)) } label: {
                    ActionLabel("Pair a Host", kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("Pair a Host")
                .identified("home.empty.pair", label: "Pair a Host")
                if !signedIn {
                    SignInCallToAction(identifier: "home.empty.signIn") { actions(.signIn) }
                }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 30)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// The machines on this network this phone has found and not paired with.
    ///
    /// Offered here and not only on the Hosts tab because this is where
    /// somebody is standing when they have nothing: the shortest true sentence
    /// about an empty phone on a network with a host on it is that the host is
    /// right there.
    private var found: [HostEntry] { hosts.candidates(.onThisNetwork) }

    private var offers: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "On this network")
            RowGroup(items: found) { host in offer(host) }
        }
    }

    private func offer(_ host: HostEntry) -> some View {
        HStack(spacing: 11) {
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(offered(host))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 6)
            Button { actions(.pair(host.id)) } label: {
                ActionLabel("Pair", kind: .outline)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel("Pair with \(host.name)")
            .identified("home.pair.\(host.id)", label: "Pair with \(host.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified("home.offer.\(host.id)", label: spokenOffer(host), value: "found")
    }

    private func offered(_ host: HostEntry) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append("found")
        return parts.joined(separator: " · ")
    }

    private func spokenOffer(_ host: HostEntry) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append("found, not paired")
        return parts.joined(separator: ", ")
    }

}

private enum HomeFilter: String, CaseIterable, Identifiable {
    case all
    case needsYou
    case running

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: "All Agents"
        case .needsYou: "Needs You"
        case .running: "Running"
        }
    }
}

extension FleetGate {
    /// The state's own word, for a capture and a door query to agree on.
    public var name: String {
        switch self {
        case .ready: "ready"
        case .signedOut: "signed-out"
        case .unsubscribed: "unsubscribed"
        }
    }
}

/// One agent, as a dense list row.
///
/// Three lines: who it is and how long ago, what it is doing in its own words,
/// and where it runs. The state is said in words on the third line wherever a
/// mark would have done the job badly.
struct AgentRowView: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    let row: AgentRow
    let state: RowState
    let host: String?
    let now: Date

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            AttentionMark(attention: state.attentionMark)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(row.name)
                        .designFont(row.unread ? .identifierUnread : .identifier, design)
                        .foregroundStyle(design.ink.color)
                    Spacer(minLength: 4)
                    Text(row.age(at: now))
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
                if let headline = row.headline {
                    Text(headline)
                        .designFont(.detail, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                third
            }
        }
        .padding(.horizontal, 13)
        .padding(.vertical, design.metrics.rowPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The state, in words, wherever a mark was doing the work badly.
    /// "Finished · 4 files · +118 −40" is both more precise than a tick and
    /// readable without having learnt a vocabulary first. When the provider
    /// never counted the changes the word stands alone: an absent count is not
    /// a zero.
    private var third: some View {
        Group {
            if typeSize.isAccessibilitySize {
                // Three things competing for one line leave each of them a few
                // characters and an ellipsis once the text is turned up —
                // "Fini… · 1 fi… mini" says less than nothing. The same words
                // stacked and allowed to wrap still say what happened.
                VStack(alignment: .leading, spacing: 2) {
                    Text([state.word, detail].compactMap { $0 }.joined(separator: " · "))
                    if let host, showsHost { Text(host) }
                }
                .fixedSize(horizontal: false, vertical: true)
            } else {
                HStack(spacing: 6) {
                    if let word = state.word {
                        Text(word)
                        Text("·")
                    }
                    Text(detail)
                    Spacer(minLength: 0)
                    if let host, showsHost { Text(host) }
                }
                .lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .designFont(.monoSmall, design)
        .foregroundStyle(design.inkFaint.color)
        .padding(.top, 1)
    }

    /// The machine goes on the trailing edge beside a state word, and into
    /// the detail without one — except where the word has already named it,
    /// which is what an offline machine's word is.
    private var showsHost: Bool {
        state.word != nil && !state.namesTheHost
    }

    private var detail: String {
        guard state.word != nil else {
            return [host, row.workingDirectory].compactMap { $0 }.joined(separator: " · ")
        }
        return state.elaboration ?? row.workingDirectory
    }
}
