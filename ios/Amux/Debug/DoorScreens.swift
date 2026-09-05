import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

/// Which screens the door can show, and how each one is built from the stores
/// a fixture filled.
///
/// A screen that is not here is not silently something else: opening it is a
/// typed refusal, so a golden run over the whole catalogue names every screen
/// still to be built instead of passing on a placeholder.
enum DoorScreens {
    @MainActor
    static func isBuilt(_ screen: Screen) -> Bool {
        switch screen {
        case .probe: true
        case .home, .homeQuiet, .firstRun, .firstRunPaid: true
        case .drawer: true
        default: false
        }
    }

    @MainActor
    @ViewBuilder
    static func view(for screen: Screen, host: DoorHost) -> some View {
        switch screen {
        case .probe: ProbeScreen()
        // The gated states are the same screen: what is empty and why is an
        // account fact the screen already reads, not a screen of its own.
        case .home, .homeQuiet, .firstRun, .firstRunPaid:
            AgentsHome(model: host.stores.fleet, accounts: host.accounts) { _ in }
        // The drawer is drawn over the screen it was opened from, and the
        // conversation it is opened from is the next milestone's. Until then
        // what is behind it is the app's own ground, so the baseline is the
        // panel, the dimming and the card edge and nothing pretending to be a
        // transcript. `ios/Goldens/BASELINE.md` says so beside the capture.
        case .drawer:
            DrawerOverlay(
                open: .constant(true),
                drawer: AgentsDrawer(
                    model: host.stores.fleet, hosts: host.stores.hosts,
                    current: Scenario.focus) { _ in }
            ) {
                Ground()
            }
        default: EmptyView()
        }
    }
}

/// The root a debug build draws: whatever the door has been asked to show,
/// under the appearance and type size it was asked for, and the app itself
/// when nothing has asked for anything.
struct DrivenRoot<Content: View>: View {
    @State private var host = DoorHost.shared
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        Group {
            if let screen = host.screen {
                DoorScreens.view(for: screen, host: host)
            } else {
                content
            }
        }
        .environment(\.design, host.design)
        .environment(\.colorScheme, host.appearance.colorScheme)
        .dynamicTypeSize(host.typeSize)
        .onPreferenceChange(IdentifiedElements.self) { declared in
            Task { @MainActor in DoorHost.shared.declared = declared }
        }
    }
}
