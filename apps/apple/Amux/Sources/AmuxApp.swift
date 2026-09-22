import AmuxCore
import AmuxDesign
import SwiftUI

@main
struct AmuxApp: App {
    init() {
        // First, before anything this app does: the mark that divides the
        // system's share of a launch from this app's.
        Signposts.emit(.appEntered)
        #if AMUX_DEBUG_TOOLS
        DoorServer.startIfRequested()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            hostedRoot
        }
    }

    @ViewBuilder private var hostedRoot: some View {
        #if AMUX_DEBUG_TOOLS
        // An app-hosted unit test still launches the application's scene. The
        // component suite supplies and renders its own roots, so constructing
        // the real composition here would open stores and start the runtime
        // only to leave them behind an invisible test window.
        if ProcessInfo.processInfo.environment["AMUX_COMPONENT_SNAPSHOTS"] == "1" {
            Color.clear
        } else {
            RootView().readingAssistiveSettings()
        }
        #else
        RootView().readingAssistiveSettings()
        #endif
    }
}
