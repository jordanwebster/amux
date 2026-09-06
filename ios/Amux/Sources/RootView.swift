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

    var body: some View {
        #if AMUX_DEBUG_TOOLS
        if let probe = ColdStartProbe.requested {
            ColdStartProbe.view(probe)
        } else {
            DrivenRoot { app }
                // What a driver queries is what is on screen, and until it
                // opens a screen by name that is the app itself.
                .onAppear {
                    DoorHost.shared.adopt(composition.stores)
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
