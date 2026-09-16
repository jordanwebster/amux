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
                consequence("checkmark", kept: true,
                            "Your amux.sh account and subscription stay as they are.")
                consequence("checkmark", kept: true, "Your agents and files stay on your hosts.")
                consequence("xmark", kept: false,
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
        HStack(spacing: 10) {
            Button { actions(.cancel) } label: {
                ActionLabel("Cancel", kind: .quiet, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("remove.cancel", label: "Cancel")
            Button { actions(.confirm) } label: {
                Text("Remove")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(Color.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 11)
                    .frame(minHeight: 44)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.removed.color)
                    }
            }
            .buttonStyle(.amuxControl)
            .identified("remove.confirm", label: "Remove")
        }
        .padding(.top, 2)
    }

    private func consequence(_ glyph: String, kept: Bool, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 9) {
            Image(systemName: glyph)
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(kept ? design.inkMuted.color : design.removed.color)
                .frame(width: 14)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
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
