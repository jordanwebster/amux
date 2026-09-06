import XCTest

/// Opening an agent, reaching the whole fleet from inside it, and coming back.
///
/// This is the one part of the home journey that has to be a UI test rather
/// than a door conversation. The door reads what a screen declared, which is
/// enough to assert what is on it, but it cannot press a SwiftUI control:
/// SwiftUI builds its accessibility tree only for an attached accessibility
/// client, and the app is not one from inside its own process. XCUITest is
/// that client, so a tap here is a tap — including the navigation bar's own
/// back button, which belongs to the system and to no screen's vocabulary.
///
/// The fleet this drives is whatever the phone remembers: the journey seeds
/// the cache before the test runs and nothing here fills a store.
final class DrawerTests: XCTestCase {
    /// Where the screenshot is left for the journey to collect. The test
    /// process has its own container on the device and the Mac reads it back
    /// out of there, which is the only path between the two.
    static let screenshot = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("drawer.png")

    /// The tree the test read, left beside the screenshot. A journey that
    /// fails here fails about a screen nobody can see afterwards, so what
    /// XCUITest could reach is written down every run.
    static let tree = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("drawer-tree.txt")

    private let waiting: TimeInterval = 30

    func testTheDrawerReachesTheFleetAndComesBack() throws {
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
        let opened = value(app, "conversation")
        XCTAssertNotNil(opened, "the conversation did not say whose it is")

        // The fleet, borrowing the conversation's screen.
        press(app, "conversation.drawer")
        let drawer = element(app, "drawer")
        XCTAssertTrue(drawer.waitForExistence(timeout: waiting), "the drawer did not open")
        XCTAssertEqual(identifiers(app, startingWith: "drawer.row.").count, remembered.count,
                       "the drawer lists a different fleet than the home")
        try? app.debugDescription.write(to: Self.tree, atomically: true, encoding: .utf8)
        for foot in ["drawer.hosts", "drawer.you", "drawer.online"] {
            XCTAssertTrue(element(app, foot).exists, "the drawer's foot is missing \(foot)")
        }
        try photograph()

        // Closing it comes back to the conversation it was opened over, not to
        // a fresh one and not to the list. The way back is the sliver of that
        // page still on screen beside the panel, which is what the scrim is.
        XCTAssertTrue(element(app, "drawer.scrim").exists,
                      "there is no way back to the page the drawer came out over")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0.5)).tap()
        // Gone means its rows are gone. The panel keeps a place in the
        // hierarchy while it is off screen, so what is asserted is the thing
        // a person would look for: the fleet is no longer over the page.
        let closed = expectation(description: "the drawer is off screen")
        let poll = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { timer in
            if self.identifiers(app, startingWith: "drawer.row.").isEmpty {
                timer.invalidate()
                closed.fulfill()
            }
        }
        wait(for: [closed], timeout: waiting)
        poll.invalidate()
        XCTAssertTrue(element(app, "conversation").exists,
                      "closing the drawer left the conversation")
        XCTAssertEqual(value(app, "conversation"), opened,
                       "closing the drawer came back to a different conversation")

        // And back to the fleet the way a person goes back from a conversation,
        // which has no navigation bar to go back from: reaching for the tab
        // already on show, which the platform reads as "take me to the top of
        // it".
        //
        // The tab bar is the system's control and carries no name of the app's:
        // an identifier put on a `Tab` names the page behind it, not the button
        // in the bar. So it is reached the way the system publishes it and the
        // way a person sees it — by the word written under the glyph.
        pressTab(app, "Agents")
        XCTAssertTrue(home.waitForExistence(timeout: waiting),
                      "going back did not return to the home")
        XCTAssertEqual(identifiers(app, startingWith: "home.row."), remembered,
                       "the fleet came back as a different list")
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

    /// Presses a tab in the system's own tab bar, by the word on it.
    private func pressTab(_ app: XCUIApplication, _ title: String) {
        let button = app.tabBars.buttons[title]
        guard button.waitForExistence(timeout: waiting) else {
            return XCTFail("the tab bar has no \(title) tab")
        }
        button.tap()
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
        attached.name = "drawer"
        attached.lifetime = .keepAlways
        add(attached)
    }
}
