import XCTest
@testable import AmuxDesign

final class AppearanceTests: XCTestCase {
    func testEveryAppearanceResolvesAColorScheme() {
        XCTAssertEqual(Appearance.allCases.map(\.rawValue), ["light", "dark"])
        XCTAssertEqual(Appearance.light.colorScheme, .light)
        XCTAssertEqual(Appearance.dark.colorScheme, .dark)
    }
}
