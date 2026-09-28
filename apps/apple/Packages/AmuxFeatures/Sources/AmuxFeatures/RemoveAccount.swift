import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did about taking an account off this phone.
public enum RemoveAccountAction: Equatable, Sendable {
    case cancel
    case confirm
}

/// Taking an account off this phone, with what that does and does not do.
///
/// It sits beside Delete Account and must never be mistaken for it, so what
/// stays is said first: the account on amux.sh, what it pays for, and every
/// agent on every host are untouched. What goes is this phone's own part —
/// the sign-in, and the hosts this phone paired for that account, which have
/// to be paired again if the account comes back. Nothing is typed to confirm
/// it, because nothing here cannot be put back by signing in.
struct RemoveAccountCard: View {
    @Environment(\.design) private var design
    let entry: AccountEntry
    let actions: @MainActor (RemoveAccountAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Remove from this phone?")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Text(entry.account.email)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
            VStack(alignment: .leading, spacing: 9) {
                Consequence("checkmark", kept: true,
                            "Your amux.sh account and subscription stay as they are.")
                Consequence("checkmark", kept: true, "Your agents and files stay on your hosts.")
                Consequence("xmark", kept: false,
                            "This phone signs out and forgets the hosts it paired for this account.")
            }
            buttons
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(
            RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous),
            wash: 0.9)
        .accessibilityElement(children: .contain)
        .identified("remove", value: entry.account.email)
    }

    private var buttons: some View {
        ButtonPair {
            choiceButton(String(localized: "Cancel"), kind: .outline, id: "remove.cancel") { actions(.cancel) }
            choiceButton(String(localized: "Remove"), kind: .destructive, id: "remove.confirm") { actions(.confirm) }
        }
        .padding(.top, 2)
    }

}

/// The question over the page it was asked from, the way deleting one is.
///
/// Looked up by the account asked about rather than by the account on screen:
/// an account signed out of can be removed from its own row without ever being
/// selected.
public struct RemoveAccountOverlay<Content: View>: View {
    private let accounts: AccountRegistry
    private let model: RemovalStore
    private let actions: @MainActor (RemoveAccountAction) -> Void
    private let content: Content

    public init(
        accounts: AccountRegistry,
        model: RemovalStore,
        actions: @escaping @MainActor (RemoveAccountAction) -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.accounts = accounts
        self.model = model
        self.actions = actions
        self.content = content()
    }

    private var entry: AccountEntry? {
        model.account.flatMap { id in accounts.accounts.first { $0.id == id } }
    }

    public var body: some View {
        ZStack {
            content
            if let entry {
                Color.black.opacity(Glass.scrim)
                    .ignoresSafeArea()
                    .onTapGesture { actions(.cancel) }
                    .accessibilityHidden(true)
                RemoveAccountCard(entry: entry, actions: actions)
                    .padding(.horizontal, 12)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                    .padding(.bottom, 10)
            }
        }
        .moving(value: entry != nil)
    }
}
