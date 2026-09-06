import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

/// Which screens the door can show, and how each one is built from the stores
/// a fixture filled.
///
/// A screen that is not here is not silently something else: opening it is a
/// typed refusal, so a golden run over the whole catalogue names every screen
/// still to be built instead of passing on a placeholder. Which states are
/// drawn is `Fixtures.isBuilt`, which the door asks before it ever gets here —
/// one screen can draw several states, and they are built one at a time.
enum DoorScreens {
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
        // One screen, four names. Whether a turn is still running, who else
        // has spoken in it, and whether it left anything to review are all
        // facts the conversation reads off its own store rather than screens of
        // their own: `run-live`, `voices` and `review-cta` are `run` with a
        // different feed and session in it.
        case .run, .runLive, .voices, .reviewCta:
            Conversation(
                model: host.stores.conversation(Scenario.focus),
                subject: ConversationSubject(
                    agent: Scenario.focus, in: host.stores.fleet)) { _ in }
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
        // The appearance is not set here. It is the window's interface style
        // and nothing else, because the design's colours are dynamic system
        // colours and the glass is a system material, and both of those read
        // the trait collection rather than SwiftUI's colour scheme. Overriding
        // the environment as well gave the two sources a frame to disagree in,
        // and a capture taken in that frame showed white plates over a
        // near-black ground.
        //
        // Built afresh on every appearance request rather than moved into the
        // new one: a material already on screen cross-fades over a length of
        // time nobody publishes, and a still of that fade is a picture of
        // neither appearance.
        .id(host.appearances)
        .dynamicTypeSize(host.typeSize)
        .onPreferenceChange(IdentifiedElements.self) { declared in
            Task { @MainActor in DoorHost.shared.declared = declared }
        }
    }
}
