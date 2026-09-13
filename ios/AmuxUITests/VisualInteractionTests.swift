import Foundation
import XCTest

/// Small visual regressions that only appear after a finger has moved through
/// the interface. Static screen captures cannot expose either of these states.
final class VisualInteractionTests: JourneyCase {
    @MainActor
    func testTabGeometryAndDismissedFilter() throws {
        let port = "8791"
        let app = XCUIApplication()
        app.launchArguments = ["-amux-door-port", port, "-amux-scripted-cloud"]
        app.launch()

        waitFor(app, "home", "the app did not open on Agents")
        let initial = tabFrames(app)
        capture(app, "tabs-agents")

        pressVisibleTab(app, "Hosts")
        waitFor(app, "hosts", "Hosts did not appear")
        XCTAssertEqual(tabFrames(app), initial, "selecting Hosts moved the tab bar")
        capture(app, "tabs-hosts")

        pressVisibleTab(app, "You")
        waitFor(app, "you", "You did not appear")
        XCTAssertEqual(tabFrames(app), initial, "selecting You moved the tab bar")
        capture(app, "tabs-you")

        pressVisibleTab(app, "Agents")
        waitFor(app, "home", "Agents did not reappear")
        XCTAssertEqual(tabFrames(app), initial, "returning to Agents moved the tab bar")
        capture(app, "tabs-agents-returned")

        let door = try Lines(address: "127.0.0.1:\(port)")
        _ = try door.ask(["kind": "open", "screen": "home", "fixture": "home"])
        _ = try door.ask(["kind": "settle"])
        let filter = element(app, "home.filter")
        XCTAssertTrue(filter.waitForExistence(timeout: 5), "the home fixture has no filter")
        filter.tap()
        let all = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "All Agents")).firstMatch
        XCTAssertTrue(all.waitForExistence(timeout: 5), "the filter menu did not open")
        all.tap()
        XCTAssertTrue(waitUntil(within: 5) { !all.exists }, "the filter menu did not close")
        RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        capture(app, "filter-dismissed")
    }

    @MainActor
    private func tabFrames(_ app: XCUIApplication) -> [CGRect] {
        ["Agents", "Hosts", "You"].map { name in
            let tab = app.buttons[name]
            XCTAssertTrue(tab.waitForExistence(timeout: 5), "the tab bar has no \(name) tab")
            return tab.frame
        }
    }

    @MainActor
    private func pressVisibleTab(_ app: XCUIApplication, _ name: String) {
        let tab = app.buttons[name]
        guard tab.waitForExistence(timeout: 5) else {
            return XCTFail("the tab bar has no \(name) tab")
        }
        tab.tap()
    }

    @MainActor
    private func capture(_ app: XCUIApplication, _ name: String) {
        let url = Self.inContainer("visual-\(name).png")
        try? app.screenshot().pngRepresentation.write(to: url)
    }
}
