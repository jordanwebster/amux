import XCTest

@testable import AmuxFeatures

/// The manifest against the catalogue.
///
/// `Screen` is the design's index as this repository holds it — one case per
/// catalogue screen, named by the design's own kebab id — so asserting the
/// manifest against it is asserting it against the index. The intake bundle
/// itself is working material and is not a build input.
final class GoldenManifestTests: XCTestCase {
    private struct Manifest: Decodable {
        let screens: [Entry]
    }

    private struct Entry: Decodable {
        let id: String
        let stage: Int
        let screen: String
        let fixture: String
        let origin: String
        let capture: String?
        let reason: String?
        let simulator: String
        let appearances: [String]
    }

    private func manifest() throws -> Manifest {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "manifest", withExtension: "json"))
        return try JSONDecoder().decode(Manifest.self, from: Data(contentsOf: url))
    }

    func testEveryCatalogueScreenIsOwedAGolden() throws {
        let entries = try manifest().screens
        let drawn = Set(entries.map(\.screen))
        for screen in Screen.allCases {
            XCTAssertTrue(
                drawn.contains(screen.rawValue),
                "\(screen.rawValue) is a screen of the app that no golden covers")
        }
    }

    func testTheThirtyThreeScreensTheDesignPicturedAreTheReferenceScreens() throws {
        let entries = try manifest().screens
        let references = Set(entries.filter { $0.origin == "reference" }.map(\.id))
        XCTAssertEqual(references.count, 33)
        // The catalogue is every screen of the app; the probe is the harness's
        // own target and the drawer is a state of the home screen the design
        // has no preserved capture of, so both are owed as added states. The
        // notification screen is out of scope and is not a case at all.
        let catalogue = Set(Screen.allCases.map(\.rawValue))
        XCTAssertEqual(catalogue.subtracting(references), ["probe", "drawer"])
        XCTAssertTrue(references.isSubset(of: catalogue))
    }

    func testEveryAddedStateDrawsAScreenTheCatalogueHas() throws {
        let entries = try manifest().screens
        let added = entries.filter { $0.origin == "added_state" }
        XCTAssertFalse(added.isEmpty)
        for entry in added {
            XCTAssertNotNil(
                Screen(rawValue: entry.screen),
                "\(entry.id) draws \(entry.screen), which is not a screen")
            XCTAssertNotNil(entry.reason, "\(entry.id) does not say why it is owed")
        }
    }

    func testEveryEntryNamesAMilestoneASimulatorAndBothAppearances() throws {
        for entry in try manifest().screens {
            XCTAssertTrue(
                (4...13).contains(entry.stage), "\(entry.id) has no milestone")
            XCTAssertTrue(
                ["amux-golden", "amux-small"].contains(entry.simulator),
                "\(entry.id) names an unpinned simulator")
            XCTAssertEqual(entry.appearances, ["light", "dark"])
            if entry.origin == "reference" {
                XCTAssertEqual(entry.capture, entry.id)
            }
        }
    }

    func testNoTwoEntriesShareAnIdentity() throws {
        let ids = try manifest().screens.map(\.id)
        XCTAssertEqual(Set(ids).count, ids.count)
    }
}
