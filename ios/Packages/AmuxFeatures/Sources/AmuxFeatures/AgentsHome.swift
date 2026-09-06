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
    private let model: FleetStore
    private let accounts: AccountRegistry
    private let actions: @MainActor (HomeAction) -> Void
    /// The fold is view state, not fleet state: opening it is a thing this
    /// screen is doing, and coming back to the screen starts it closed again.
    @State private var foldOpen = false

    public init(
        model: FleetStore,
        accounts: AccountRegistry,
        actions: @escaping @MainActor (HomeAction) -> Void
    ) {
        self.model = model
        self.accounts = accounts
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                header
                // An account problem is only the whole screen when there is
                // nothing else on it. A phone that is signed out but still
                // remembers agents has a list worth reading; what it cannot do
                // is refresh it, and that belongs on the exceptions line.
                if accounts.gate == .ready || !model.rows.isEmpty {
                    fleet
                } else {
                    gated
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .onAppear { actions(.refresh) }
        .identified("home", value: accounts.gate.name)
    }

    // MARK: - Header

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            if accounts.gate != .ready { accountDisc }
            VStack(alignment: .leading, spacing: 1) {
                title
                Text(subtitle)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .identified("home.subtitle", value: subtitle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button { actions(.newAgent) } label: {
                GlassIcon(glyph: "plus", prominent: true)
            }
            .accessibilityLabel("New Agent")
            .identified("home.newAgent", label: "New Agent")
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
        if accounts.accounts.count > 1 || accounts.gate != .ready {
            Menu {
                ForEach(accounts.accounts) { entry in
                    Button {
                        actions(.switchAccount(entry.id))
                    } label: {
                        Label(
                            entry.account.email,
                            systemImage: entry.id == accounts.selected ? "checkmark" : "person")
                    }
                    .accessibilityIdentifier("home.account.\(entry.account.email)")
                }
                Button("Add Account") { actions(.addAccount) }
                    .accessibilityIdentifier("home.addAccount")
            } label: {
                HStack(spacing: 5) {
                    Text("Agents")
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            .accessibilityLabel("Agents, switch account")
            .identified("home.title", label: "Agents, switch account", value: "Agents")
        } else {
            Text("Agents")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("home.title", value: "Agents")
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
        if accounts.gate == .ready || !model.rows.isEmpty { return model.subtitle }
        switch accounts.gate {
        case .ready: return model.subtitle
        case .signedOut: return "Not signed in"
        case .unsubscribed: return "Not subscribed"
        }
    }

    // MARK: - The list

    private var fleet: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if let exceptions {
                    exceptionsLine(exceptions)
                }
                ForEach(model.sections) { section in
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
        }
        .scrollIndicators(.hidden)
        .refreshable { actions(.refresh) }
    }

    @ViewBuilder
    private func agentRow(_ row: AgentRow) -> some View {
        let content = AgentRowView(row: row, host: model.host(row.hostId)?.name, now: model.orderedAt)
            .shimmering(!row.confirmed)
        // An agent run by a provider this build has no case for is listed and
        // not offered to open. A button that led to a conversation of which
        // not one row could be read would be a worse answer than the row
        // saying so where it stands.
        if row.readable {
            Button { actions(.open(row.id)) } label: { content }
                .buttonStyle(.plain)
                .accessibilityLabel(spoken(row))
                .identified(
                    "home.row.\(row.id)", label: spoken(row),
                    value: row.confirmed ? row.attention.spoken : "\(row.attention.spoken), remembered")
        } else {
            content
                .accessibilityElement(children: .combine)
                .accessibilityLabel(spoken(row))
                .identified("home.row.\(row.id)", label: spoken(row), value: "cannot be read")
        }
    }

    /// What a row says to somebody who cannot see it, in the order the row
    /// says it: who, what, where, how long, and what it needs.
    private func spoken(_ row: AgentRow) -> String {
        var parts = [row.name]
        if let headline = row.headline { parts.append(headline) }
        parts.append(row.attention.spoken)
        if row.why == .finished, let outcome = row.outcome { parts.append(outcome.arithmetic) }
        parts.append([model.host(row.hostId)?.name, row.workingDirectory]
            .compactMap { $0 }.joined(separator: ", "))
        parts.append(row.age(at: model.orderedAt) + " ago")
        if !row.readable { parts.append("this build cannot read it") }
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
        .buttonStyle(.plain)
        .accessibilityLabel("\(title), \(names)")
        .identified("home.fold.\(section.id)", label: "\(title), \(names)", value: names)
    }

    /// The one line a home is allowed above the list.
    ///
    /// It is not a summary — a summary is a card that must be filled, so it
    /// fills itself with counts nobody asked for. It is an exceptions line: it
    /// appears only when something is actually wrong, it takes one row when it
    /// does, and when everything is fine the top of the screen is the list.
    private var exceptions: String? {
        switch accounts.gate {
        case .ready: model.exceptions
        // Said once, where the one thing that is wrong goes. The rows below it
        // are real; they are just not going to change until this is fixed.
        case .signedOut: "Not signed in · nothing is live"
        case .unsubscribed: "Not subscribed · nothing is live"
        }
    }

    private func exceptionsLine(_ text: String) -> some View {
        Button {
            switch accounts.gate {
            case .ready: actions(.openExceptions)
            case .signedOut: actions(.signIn)
            case .unsubscribed: actions(.subscribe)
            }
        } label: {
            Surface {
                HStack(spacing: 11) {
                    Image(systemName: accounts.gate == .ready ? "wifi.slash" : "person.slash")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(design.inkMuted.color)
                        .frame(width: 18)
                    Text(text)
                        .designFont(.detail, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
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
        .buttonStyle(.plain)
        .accessibilityLabel(text)
        .identified("home.exceptions", label: text, value: text)
    }

    // MARK: - Nothing to reach yet

    /// Not a splash: the real home screen, empty.
    ///
    /// A splash would teach that this is a service you subscribe to; an empty
    /// list teaches that it is a client for hosts you own. There is one action
    /// because there is one thing to do, and the headline is still the list
    /// being empty, because that is what the screen is.
    private var gated: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 8) {
                Text("No hosts yet")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .identified("home.empty.title", value: "No hosts yet")
                Explain(accounts.gate == .signedOut
                    ? "Sign in to pair one." : "Subscribe to pair one.")
                    .identified("home.empty.explain")
            }
            Button {
                actions(accounts.gate == .signedOut ? .signIn : .subscribe)
            } label: {
                ActionLabel(gateActionTitle, kind: .primary, fill: true)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(gateActionTitle)
            .identified("home.empty.action", label: gateActionTitle)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 40)
    }

    private var gateActionTitle: String {
        accounts.gate == .signedOut ? "Sign In" : "Subscribe"
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
    let host: String?
    let now: Date

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            AttentionMark(attention: row.attention)
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
                    Text([stateWord, detail].compactMap { $0 }.joined(separator: " · "))
                    if stateWord != nil, let host { Text(host) }
                }
                .fixedSize(horizontal: false, vertical: true)
            } else {
                HStack(spacing: 6) {
                    if let word = stateWord {
                        Text(word)
                        Text("·")
                    }
                    Text(detail)
                    Spacer(minLength: 0)
                    if stateWord != nil, let host { Text(host) }
                }
                .lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .designFont(.monoSmall, design)
        .foregroundStyle(design.inkFaint.color)
        .padding(.top, 1)
    }

    private var stateWord: String? {
        // Said in words rather than left to the mark. There is no glyph for
        // "this build has no case for what runs here", and an agent that
        // cannot be read is not idle.
        guard row.readable else { return "Cannot be read" }
        switch row.attention {
        case .needsYou(why: .finished): return "Finished"
        case .idle: return "Idle"
        default: return nil
        }
    }

    private var detail: String {
        if stateWord != nil {
            // Whatever the turn changed, wherever the row has room to say it.
            // An agent that has gone quiet since finishing changed exactly
            // what it changed, and the numbers are the readable part.
            if let outcome = row.outcome { return outcome.arithmetic }
            return row.workingDirectory
        }
        return [host, row.workingDirectory].compactMap { $0 }.joined(separator: " · ")
    }
}
