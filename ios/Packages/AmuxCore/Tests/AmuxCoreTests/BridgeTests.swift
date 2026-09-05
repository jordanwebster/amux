import XCTest
@testable import AmuxCore

final class BridgeTests: XCTestCase {
    func testTheLinkedBridgeReportsItsVersion() {
        let version = Bridge.version
        XCTAssertFalse(version.isEmpty)
        XCTAssertEqual(version.split(separator: ".").count, 3, "expected a semantic version, got \(version)")
    }
}
