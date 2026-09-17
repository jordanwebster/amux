import AmuxDesign
import SwiftUI

/// The terminal surface for a store the runtime cannot use.
///
/// It replaces the whole shell rather than sitting over a fleet or
/// conversation: once persistence is unavailable there is no second,
/// degraded source of state a person can keep using.
public struct StoreFailureScreen: View {
    @Environment(\.design) private var design
    private let message: String
    private let relaunch: @MainActor () -> Void

    public init(message: String, relaunch: @escaping @MainActor () -> Void) {
        self.message = message
        self.relaunch = relaunch
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                Spacer(minLength: 72)
                Image(systemName: "externaldrive.badge.exclamationmark")
                    .font(.system(size: 30, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .accessibilityHidden(true)
                Text("Store unavailable")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .padding(.top, 18)
                    .identified("store-failure.title", value: "Store unavailable")
                Text("amux stopped because it can’t use this account’s store.")
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 8)
                    .identified("store-failure.cause")
                Text(message)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 22)
                    .identified("store-failure.remedy", value: message)
                Spacer(minLength: 32)
                Button(action: relaunch) {
                    ActionLabel("Relaunch", kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .identified("store-failure.relaunch", label: "Relaunch")
                .padding(.bottom, 24)
            }
            .padding(.horizontal, design.metrics.gutter)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .accessibilityElement(children: .contain)
        .identified("store-failure", value: message)
    }
}
