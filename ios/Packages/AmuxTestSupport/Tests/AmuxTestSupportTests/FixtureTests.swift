import XCTest
@testable import AmuxTestSupport

final class FixtureTests: XCTestCase {
    func testEveryFixtureIsReachableByName() {
        XCTAssertFalse(Fixtures.all.isEmpty)
        for fixture in Fixtures.all {
            XCTAssertEqual(Fixtures.named(fixture.id)?.id, fixture.id)
        }
        XCTAssertNil(Fixtures.named("no-such-fixture"))
    }

    func testFixtureIdentifiersAreUnique() {
        let identifiers = Fixtures.all.map(\.id)
        XCTAssertEqual(Set(identifiers).count, identifiers.count)
    }
}
