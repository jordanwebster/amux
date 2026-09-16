import AmuxCore
import AmuxFeatures
import AmuxShell
import XCTest
@testable import Amux

@MainActor
final class ScenarioShellTests: XCTestCase {
    func testReopeningHomeStartsAtTheRootAndResetsEnvironment() async {
        let door = DoorHost.shared
        _ = await door.handle(.open(screen: "home", fixture: "home"))
        let first = ScenarioScene(screen: .home, stores: door.stores, overlay: nil)
        first.router.open(.conversation(Scenario.focus))
        first.router.tab = .hosts
        _ = await door.handle(.dynamicType("accessibility3"))
        _ = await door.handle(.assist(motion: true, transparency: true))
        let original = door.stores

        let reopened = await door.handle(.open(screen: "home", fixture: "home"))
        XCTAssertEqual(reopened, .ack)
        let second = ScenarioScene(screen: .home, stores: door.stores, overlay: nil)
        XCTAssertFalse(door.stores === original)
        XCTAssertEqual(second.router.tab, .agents)
        XCTAssertTrue(second.router.path.isEmpty)
        XCTAssertEqual(door.typeSize, .large)
        XCTAssertFalse(door.reduceMotion)
        XCTAssertFalse(door.reduceTransparency)
    }

    func testConversationAndPlanEnterTheProductionConversationRoute() async {
        let door = DoorHost.shared
        for screen in [Screen.run, .plan] {
            let reply = await door.handle(.open(screen: screen.rawValue, fixture: nil))
            XCTAssertEqual(reply, .ack)
            let scene = ScenarioScene(screen: screen, stores: door.stores, overlay: .plus)
            XCTAssertEqual(scene.router.tab, .agents)
            XCTAssertEqual(scene.router.path, [.conversation(Scenario.focus)])
            XCTAssertEqual(scene.recording.showing[Scenario.focus], ConversationOverlay.plus.rawValue)
            XCTAssertNotNil(door.stores.conversations[Scenario.focus])
        }
    }

    /// A phone paired with a machine running nothing is not a phone that has
    /// paired with nothing: it is told to start an agent, not to pair again.
    func testAPairedHomeWithNoAgentsOffersNewAgentRatherThanPairing() async throws {
        let door = DoorHost.shared
        let reply = await door.handle(.open(screen: "home", fixture: "home-no-agents"))
        XCTAssertEqual(reply, .ack)
        _ = await door.handle(.settle)
        let names = Set(door.declared.map(\.identifier))
        XCTAssertTrue(names.contains("home.empty.noAgents"), "\(names)")
        XCTAssertTrue(names.contains("home.empty.newAgent"), "\(names)")
        XCTAssertFalse(names.contains("home.empty.firstRun"), "\(names)")
        XCTAssertFalse(names.contains("home.empty.howToPair"), "\(names)")
        let subtitle = door.declared.first { $0.identifier == "home.subtitle" }
        XCTAssertEqual(subtitle?.value, "No agents yet · \(Scenario.reachableHosts.count) hosts")
    }

    func testHomeCaptureContainsTheActualShellAndAllThreeTabs() async throws {
        let door = DoorHost.shared
        let reply = await door.handle(.open(screen: "home", fixture: "home"))
        XCTAssertEqual(reply, .ack)
        _ = await door.handle(.settle)
        let names = Set(door.declared.map(\.identifier))
        for name in ["shell", "home", "tab.agents", "tab.hosts", "tab.you"] {
            XCTAssertTrue(names.contains(name), "Missing \(name) from the rendered app")
        }
    }
}
