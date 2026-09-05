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
        default: false
        }
    }

    @MainActor
    @ViewBuilder
    static func view(for screen: Screen, host: DoorHost) -> some View {
        switch screen {
        case .probe: ProbeScreen()
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
