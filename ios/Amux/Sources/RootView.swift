import AmuxCore
import AmuxDesign
import AmuxShell
import SwiftUI

/// The app's root.
///
/// A debug build shows whatever a driver has opened and the launch the
/// performance suite asked to time; anything else, and any build a person
/// installs, is the app itself.
struct RootView: View {
    @State private var composition = Composition()
    @Environment(\.scenePhase) private var phase

    var body: some View {
        scene
            // Whether anybody is looking at this phone is a fact about the app,
            // so it is said to the runtime rather than to any one screen. Put
            // away, the link is released at once: a socket left for the system
            // to freeze leaves every machine this phone was watching holding a
            // connection nobody is reading.
            .onChange(of: phase) { _, now in
                BridgeClient.running?.setActive(now != .background)
            }
    }

    @ViewBuilder private var scene: some View {
        #if AMUX_DEBUG_TOOLS
        if let probe = ColdStartProbe.requested {
            ColdStartProbe.view(probe)
        } else {
            DrivenRoot { app }
                // What a driver queries is what is on screen, and until it
                // opens a screen by name that is the app itself.
                .onAppear {
                    DoorHost.shared.adopt(composition.stores, accounts: composition.accounts)
                    DoorHost.shared.connectAsLaunchAsks()
                }
        }
        #else
        app
        #endif
    }

    private var app: some View {
        Shell(
            router: composition.router,
            accounts: composition.accounts,
            stores: composition.stores,
            actions: { composition.handle($0) }
        )
        .onOpenURL { composition.router.open($0) }
    }
}
