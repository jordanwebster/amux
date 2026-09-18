import XCTest

/// Opening an agent from the fleet, and coming back to the same fleet.
///
/// This is the one part of the home journey that has to be a UI test rather
/// than a door conversation. The door reads what a screen declared, which is
/// enough to assert what is on it, but it cannot press a SwiftUI control:
/// SwiftUI builds its accessibility tree only for an attached accessibility
/// client, and the app is not one from inside its own process. XCUITest is
/// that client, so a tap here is a tap — and a drag from the edge of the
/// display is the platform's own gesture, which belongs to no screen's
/// vocabulary.
///
/// The fleet this drives is whatever the phone remembers: the journey seeds
/// the cache before the test runs and nothing here fills a store.
final class FleetReturnTests: XCTestCase {
    /// Where the screenshot is left for the journey to collect. The test
    /// process has its own container on the device and the Mac reads it back
    /// out of there, which is the only path between the two.
    static let screenshot = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("opened.png")

    /// The tree the test read, left beside the screenshot. A journey that
    /// fails here fails about a screen nobody can see afterwards, so what
    /// XCUITest could reach is written down every run.
    static let tree = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("opened-tree.txt")

    private let waiting: TimeInterval = 30

    func testAConversationComesBackToTheFleetItWasOpenedFrom() throws {
        let app = XCUIApplication()
        app.launch()

        let home = element(app, "home")
        XCTAssertTrue(home.waitForExistence(timeout: waiting), "the app did not draw the home")
        let remembered = identifiers(app, startingWith: "home.row.")
        XCTAssertGreaterThan(remembered.count, 1, "the home drew \(remembered.count) rows")

        // Open a row: what a person does to a list.
        press(app, remembered[0])
        let conversation = element(app, "conversation")
        XCTAssertTrue(conversation.waitForExistence(timeout: waiting),
                      "opening a row did not lead to a conversation")
        XCTAssertNotNil(value(app, "conversation"), "the conversation did not say whose it is")
        try? app.debugDescription.write(to: Self.tree, atomically: true, encoding: .utf8)
        // The way out is on the conversation's own chrome, where the thumb
        // looks for it, and there is no tab bar under the composer.
        XCTAssertTrue(element(app, "conversation.back").exists,
                      "the conversation offers no way back")
        XCTAssertFalse(element(app, "tab.agents").exists,
                       "the tab bar stands under an open conversation")
        try photograph()

        // Back by the chevron.
        press(app, "conversation.back")
        XCTAssertTrue(waitUntil { !conversation.exists && home.exists },
                      "the back chevron did not return to the home")
        XCTAssertEqual(identifiers(app, startingWith: "home.row."), remembered,
                       "the fleet came back as a different list")

        // And again with the platform's edge gesture, which does the same.
        press(app, remembered[0])
        XCTAssertTrue(conversation.waitForExistence(timeout: waiting),
                      "opening the row again did not lead to a conversation")
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.5))
        let inside = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.5))
        edge.press(forDuration: 0.05, thenDragTo: inside)
        XCTAssertTrue(waitUntil { !conversation.exists && home.exists },
                      "the edge swipe did not return to the home")
        XCTAssertEqual(identifiers(app, startingWith: "home.row."), remembered,
                       "the fleet came back as a different list")
    }

    private func waitUntil(_ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(waiting)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        }
        return condition()
    }

    // MARK: - Reading a screen by the names it declares

    /// One element by name. A screen names a thing once, but a name can land
    /// on more than one element in the tree the system builds from it, so the
    /// first is taken rather than the query left ambiguous.
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// What a named element says its value is, from whichever of its elements
    /// carries it.
    private func value(_ app: XCUIApplication, _ identifier: String) -> String? {
        app.descendants(matching: .any).matching(identifier: identifier)
            .allElementsBoundByIndex.compactMap { $0.value as? String }.first
    }

    /// Every distinct name beginning with this, in the order the screen draws
    /// them.
    private func identifiers(_ app: XCUIApplication, startingWith prefix: String) -> [String] {
        let matching = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
        var seen: [String] = []
        for element in matching.allElementsBoundByIndex where !seen.contains(element.identifier) {
            seen.append(element.identifier)
        }
        return seen
    }

    /// Presses the thing with this name where a finger would land on it.
    private func press(_ app: XCUIApplication, _ identifier: String) {
        let candidates = app.descendants(matching: .any)
            .matching(identifier: identifier).allElementsBoundByIndex
        let hittable = candidates.first(where: { $0.isHittable })
        guard let target = hittable ?? candidates.first else {
            return XCTFail("nothing on screen is named \(identifier)")
        }
        if target.isHittable {
            target.tap()
        } else {
            target.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    /// A photograph of the screen, left where the Mac can read it out of this
    /// process's container.
    private func photograph() throws {
        let shot = XCUIScreen.main.screenshot()
        try shot.pngRepresentation.write(to: Self.screenshot)
        let attached = XCTAttachment(screenshot: shot)
        attached.name = "opened"
        attached.lifetime = .keepAlways
        add(attached)
    }
}
