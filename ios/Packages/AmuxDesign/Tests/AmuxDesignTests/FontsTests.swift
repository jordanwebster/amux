import XCTest
@testable import AmuxDesign

final class FontsTests: XCTestCase {
    func testTheBundledFacesRegisterAndAreAvailable() {
        XCTAssertTrue(BundledFonts.register(), "the bundled faces did not register")
        XCTAssertTrue(BundledFonts.isAvailable(Design.app.faces.body))
        XCTAssertTrue(BundledFonts.isAvailable(Design.app.faces.mono))
    }

    func testEveryBundledFontFileIsInTheResourceBundle() {
        for file in BundledFonts.files {
            XCTAssertNotNil(BundledFonts.url(file), "\(file) is missing")
        }
    }
}
