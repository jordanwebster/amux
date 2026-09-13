import AmuxCore
import AmuxFeatures
import AmuxShell
import SwiftUI

/// Initial navigation and presentation state for a deterministic app scene.
/// The stores are already filled by the fixture; this router deliberately has
/// no network loader. Screens and their local interactions remain production
/// code, while service-dependent actions need a scripted/live journey to prove
/// their outcome.
@MainActor
final class ScenarioScene {
    let router = Router()
    let recording = ConversationRecording()

    init(screen: Screen, stores: StoreBundle, overlay: ConversationOverlay?) {
        switch screen {
        case .home, .homeQuiet, .firstRun, .firstRunPaid:
            break
        case .run, .plan:
            let agent = stores.conversations.keys.contains(Scenario.focus)
                ? Scenario.focus : Scenario.agentId("spec-suite")
            recording.showing[agent] = overlay?.rawValue
            router.open(.conversation(agent))
        default:
            preconditionFailure("No shell scenario for \(screen.rawValue)")
        }
    }
}

/// The real application shell supplied with deterministic fixture state.
/// Its identity is the fixture's store bundle, so reopening a scenario discards
/// navigation and view-local state from the preceding visit.
struct ScenarioShell: View {
    private let host: DoorHost
    @State private var scene: ScenarioScene

    init(screen: Screen, host: DoorHost) {
        self.host = host
        _scene = State(initialValue: ScenarioScene(
            screen: screen, stores: host.stores, overlay: host.overlay))
    }

    var body: some View {
        Shell(
            router: scene.router, accounts: host.accounts, stores: host.stores,
            signIn: host.signIn, paywall: host.paywall, deletion: host.deletion,
            appearance: host.appearance, recording: scene.recording,
            actions: { _ in })
    }
}
