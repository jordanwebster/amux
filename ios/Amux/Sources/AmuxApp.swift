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
            RootView()
                .readingAssistiveSettings()
        }
    }
}
