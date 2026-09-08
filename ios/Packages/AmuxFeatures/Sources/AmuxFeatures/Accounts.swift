import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did about an account.
public enum AccountsAction: Equatable, Sendable {
    case select(AccountId)
    case add
    /// Sign back into an account this phone still remembers.
    case signIn(AccountId)
    case signOut(AccountId)
    case delete(AccountId)
    /// Open what this account has bought.
    case subscription
    case appearance(Appearance?)
    case identity
    case support
    case report
    /// Put the switcher away without changing anything.
    case dismiss
}

/// One account, as a row.
///
/// The second line is what this phone actually knows: how many machines the
/// account reaches where there is a connection to have counted them, that it
/// is signed out where it is, and the address otherwise. Nothing here is
/// inferred — an account nobody has connected to says its own name and no more.
struct AccountRow: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let selected: Bool
    let actions: @MainActor (AccountsAction) -> Void

    var body: some View {
        Button { actions(entry.signedIn ? .select(entry.id) : .signIn(entry.id)) } label: {
            HStack(spacing: 13) {
                InitialsDisc(entry: entry, filled: selected)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.name)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                    Text(entry.line)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                trailing
            }
            .padding(.vertical, 11)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
        .identified(
            "account.\(entry.account.email)",
            label: "\(entry.name), \(entry.line)",
            value: selected ? "selected" : entry.signedIn ? "signed in" : "signed out")
    }

    @ViewBuilder
    private var trailing: some View {
        if selected {
            Image(systemName: "checkmark")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(design.accent.color)
                .accessibilityHidden(true)
        } else if !entry.signedIn {
            Text("Sign In")
                .designFont(.mono, design)
                .foregroundStyle(design.accent.color)
        } else if let waiting = entry.attention, waiting > 0 {
            // Only ever drawn from a count something actually reported. An
            // account this phone has no connection to says nothing here.
            Text("\(waiting)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.onAccent.color)
                .frame(minWidth: 24, minHeight: 24)
                .background(Circle().fill(design.accent.color))
                .accessibilityLabel("\(waiting) need you")
                // Named in its own right: the row's own name carries what the
                // row says about the account, and the number is a fact about
                // somewhere else that changes while the row does not.
                .identified(
                    "account.\(entry.account.email).waiting",
                    label: "\(waiting) need you", value: "\(waiting)")
        }
    }
}

/// The two letters an account is recognised by before its name is read.
struct InitialsDisc: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let filled: Bool
    var size: CGFloat = 34

    var body: some View {
        Text(initials)
            .designFont(.monoSmall, design)
            .foregroundStyle(filled ? design.onAccent.color : design.inkMuted.color)
            .frame(width: size, height: size)
            .background(Circle().fill(filled ? design.accent.color : design.sunken.color))
            .accessibilityHidden(true)
    }

    /// Two letters, because one is not enough to tell two accounts apart at a
    /// glance and three stops reading as a monogram. A name of two words gives
    /// one letter from each; a single word gives its first two.
    private var initials: String {
        let words = entry.name.split(separator: " ").prefix(2)
        guard let first = words.first else { return "?" }
        let letters = words.count > 1
            ? words.compactMap(\.first).map(String.init).joined()
            : String(first.prefix(2))
        return letters.uppercased()
    }
}

/// The accounts this phone knows, as a panel under the title that named them.
///
/// It is a panel and not a system menu because the rows are not menu items:
/// each carries what this phone knows about that account, and one of them
/// offers to sign back in rather than to switch. A menu would flatten all of
/// that into a list of words.
public struct AccountSwitcher: View {
    @Environment(\.design) private var design
    private let accounts: AccountRegistry
    private let actions: @MainActor (AccountsAction) -> Void

    public init(
        accounts: AccountRegistry, actions: @escaping @MainActor (AccountsAction) -> Void
    ) {
        self.accounts = accounts
        self.actions = actions
    }

    public var body: some View {
        VStack(spacing: 0) {
            ForEach(accounts.accounts) { entry in
                AccountRow(entry: entry, selected: entry.id == accounts.selected, actions: actions)
                    .padding(.horizontal, 14)
                Rectangle()
                    .fill(design.hairline.color)
                    .frame(height: design.metrics.hairline)
                    .padding(.leading, 61)
            }
            Button { actions(.add) } label: {
                HStack(spacing: 13) {
                    Image(systemName: "plus")
                        .font(.system(size: 17, weight: .medium))
                        .frame(width: 34)
                    Text("Add Account")
                        .designFont(.body, design)
                    Spacer(minLength: 0)
                }
                .foregroundStyle(design.accent.color)
                .padding(.vertical, 13)
                .padding(.horizontal, 14)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .identified("accounts.add", label: "Add Account")
        }
        .frosted(RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("accounts.switcher", value: accounts.selected?.value ?? "none")
    }
}

/// The switcher over the screen it was opened from.
///
/// Drawn over the real screen rather than beside it: what it covers, what its
/// edge uncovers and how the list behind it dims are facts about the screen
/// underneath, so a capture is of both at once.
struct SwitcherOverlay<Content: View>: View {
    @Environment(\.design) private var design
    @Binding var open: Bool
    let accounts: AccountRegistry
    let actions: @MainActor (AccountsAction) -> Void
    @ViewBuilder let content: Content

    var body: some View {
        ZStack(alignment: .top) {
            content
            if open {
                // Everything under the panel is dimmed and takes the tap that
                // puts it away: a panel you have to aim at a close button to
                // dismiss is a panel that traps a thumb.
                Color.black.opacity(0.22)
                    .ignoresSafeArea()
                    .onTapGesture { actions(.dismiss) }
                    .accessibilityHidden(true)
                AccountSwitcher(accounts: accounts, actions: actions)
                    .padding(.horizontal, design.metrics.gutter)
                    // Under the title it hangs from, which is where the eye
                    // already is after pressing it.
                    .padding(.top, 76)
            }
        }
    }
}

/// You: the accounts this phone knows, what the one on screen has, and the
/// things that belong to the phone rather than to any account.
///
/// One page rather than a list of screens. Appearance is a control on its own
/// row instead of a value behind a page, support sits with reporting because
/// "something is wrong" is one intent with two exits, and an account's own
/// actions sit under the list of accounts because they are that list's
/// selected row continued.
public struct YouScreen: View {
    @Environment(\.design) private var design
    private let accounts: AccountRegistry
    private let appearance: Appearance?
    private let identity: String?
    private let debugTools: Bool
    private let actions: @MainActor (AccountsAction) -> Void

    public init(
        accounts: AccountRegistry,
        appearance: Appearance? = nil,
        identity: String? = nil,
        debugTools: Bool = false,
        actions: @escaping @MainActor (AccountsAction) -> Void
    ) {
        self.accounts = accounts
        self.appearance = appearance
        self.identity = identity
        self.debugTools = debugTools
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    Text("You")
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                        .padding(.top, 8)
                        .identified("you.title", value: "You")
                    accountList
                    if let entry = accounts.selectedAccount { account(entry) }
                    phone
                    help
                }
                .padding(.horizontal, design.metrics.gutter)
                .padding(.bottom, 34)
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them: without this every row underneath answers to the screen's name.
        .accessibilityElement(children: .contain)
        .identified("you", value: accounts.selectedAccount?.account.email ?? "none")
    }

    private var accountList: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Accounts")
            VStack(spacing: 0) {
                ForEach(accounts.accounts) { entry in
                    AccountRow(
                        entry: entry, selected: entry.id == accounts.selected, actions: actions)
                    rule(inset: 47)
                }
                Button { actions(.add) } label: {
                    HStack(spacing: 13) {
                        Image(systemName: "plus")
                            .font(.system(size: 17, weight: .medium))
                            .frame(width: 34)
                        Text("Add Account")
                            .designFont(.body, design)
                        Spacer(minLength: 0)
                    }
                    .foregroundStyle(design.accent.color)
                    .padding(.vertical, 13)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .identified("you.add", label: "Add Account")
            }
        }
    }

    /// The selected account's own actions, under the list they continue.
    private func account(_ entry: AccountEntry) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: entry.name)
            VStack(spacing: 0) {
                row("Subscription", value: entry.entitlement.summary, id: "subscription") {
                    actions(.subscription)
                }
                rule()
                action("Sign Out", id: "signOut") { actions(.signOut(entry.id)) }
                rule()
                // The one destructive thing on the page, in the colour this
                // app keeps for exactly that.
                action("Delete Account", id: "delete", danger: true) {
                    actions(.delete(entry.id))
                }
            }
        }
    }

    private var phone: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "This phone")
            VStack(spacing: 0) {
                appearanceRow
                if let identity {
                    rule()
                    row("Identity", value: identity, id: "identity") { actions(.identity) }
                }
            }
        }
    }

    /// Appearance is the value, not a page holding it: three words, and the
    /// screen changes under your thumb.
    private var appearanceRow: some View {
        HStack {
            Text("Appearance")
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
            Spacer(minLength: 8)
            HStack(spacing: 0) {
                choice("Light", .light)
                choice("Dark", .dark)
                choice("System", nil)
            }
            .padding(3)
            .background(Capsule().fill(design.sunken.color))
        }
        .padding(.vertical, 11)
        .accessibilityElement(children: .contain)
        .identified("you.appearance", value: appearance?.rawValue ?? "system")
    }

    private func choice(_ title: String, _ wanted: Appearance?) -> some View {
        let chosen = appearance == wanted
        return Button { actions(.appearance(wanted)) } label: {
            Text(title)
                .designFont(.monoSmall, design)
                .foregroundStyle(chosen ? design.ground.color : design.inkMuted.color)
                .padding(.horizontal, 11)
                .padding(.vertical, 7)
                .background {
                    if chosen { Capsule().fill(design.ink.color) }
                }
                .thumbTarget(y: 7)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "you.appearance.\(wanted?.rawValue ?? "system")", label: title,
            value: chosen ? "chosen" : "not chosen")
        .reclaimingThumbTarget(y: 7)
    }

    private var help: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Help")
            VStack(spacing: 0) {
                glyphRow("Contact Support", glyph: "envelope", id: "support") {
                    actions(.support)
                }
                // Only where the tools to write one exist. A build a person
                // installs has no report to send.
                if debugTools {
                    rule(inset: 47)
                    glyphRow("Report a Problem", glyph: "ladybug", id: "report") {
                        actions(.report)
                    }
                }
            }
        }
    }

    // MARK: - Rows

    private func row(
        _ title: String, value: String, id: String, press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            HStack(spacing: 8) {
                Text(title)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Text(value)
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.vertical, 13)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .identified("you.\(id)", label: title, value: value)
    }

    private func action(
        _ title: String, id: String, danger: Bool = false,
        press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            HStack {
                Text(title)
                    .designFont(.body, design)
                    .foregroundStyle(danger ? design.removed.color : design.accent.color)
                Spacer(minLength: 0)
            }
            .padding(.vertical, 13)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .identified("you.\(id)", label: title)
    }

    private func glyphRow(
        _ title: String, glyph: String, id: String, press: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: press) {
            HStack(spacing: 13) {
                Image(systemName: glyph)
                    .font(.system(size: 16, weight: .regular))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 34)
                Text(title)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.vertical, 13)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .identified("you.\(id)", label: title)
    }

    private func rule(inset: CGFloat = 0) -> some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, inset)
    }
}
