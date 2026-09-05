import SwiftUI

@main
struct AmuxApp: App {
    init() {
        #if AMUX_DEBUG_TOOLS
        DoorServer.startIfRequested()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}
