import AmuxCore
import AmuxDesign
import SwiftUI

/// The shell's root. Screens are hung off it as they are built; for now it
/// draws the design's ground and type so the token set is exercised by the
/// running app and not only by its tests.
struct RootView: View {
    @Environment(\.design) private var design

    var body: some View {
        ZStack {
            Ground()
            VStack(spacing: 8) {
                Text("root.title")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .accessibilityIdentifier("root.title")
                Text(verbatim: Bridge.version)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .accessibilityIdentifier("root.bridgeVersion")
            }
        }
        .accessibilityIdentifier("root")
    }
}
