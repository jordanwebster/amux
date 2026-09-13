import XCTest

final class LaunchTests: XCTestCase {
    func testTheAppLaunchesToItsRoot() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.otherElements["root"].waitForExistence(timeout: 30))
    }
}
