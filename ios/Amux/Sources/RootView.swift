import AmuxCore
import AmuxDesign
import SwiftUI

/// The shell's root. Screens are hung off it as they are built; for now it
/// draws the design's ground and type so the token set is exercised by the
/// running app and not only by its tests.
struct RootView: View {
    var body: some View {
        #if AMUX_DEBUG_TOOLS
        // A debug build shows whatever a driver has opened, the launch the
        // performance suite asked to time, and the app itself when nothing
        // has asked for anything.
        if let probe = ColdStartProbe.requested {
            ColdStartProbe.view(probe)
        } else {
            DrivenRoot { AppRoot() }
        }
        #else
        AppRoot()
        #endif
    }
}

struct AppRoot: View {
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
