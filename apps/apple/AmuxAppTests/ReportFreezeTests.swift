import AmuxCore
import UIKit
import XCTest
@testable import Amux

/// The freeze every build takes, including one with no driving tools in it.
@MainActor
final class ReportFreezeTests: XCTestCase {
    private func window() -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 120, height: 200))
        let root = UIViewController()
        root.view.backgroundColor = .systemTeal
        window.rootViewController = root
        window.isHidden = false
        return window
    }

    /// Photographed, named, and honest about the one recording it cannot make:
    /// a build without the driving tools records no view state, and the bundle
    /// says why rather than leaving the part out.
    func testAFreezeWithoutTheDrivingToolsDeclaresTheTraceAbsent() throws {
        let shown = window()
        let freezer = ReportFreeze(window: { shown }, route: { "you" })

        let capture = try XCTUnwrap(freezer.freeze())

        XCTAssertEqual(capture.frame.width, 120)
        XCTAssertEqual(capture.frame.height, 200)
        XCTAssertNotNil(capture.frame.image, "the photograph does not decode")
        XCTAssertEqual(capture.route, "you")
        XCTAssertNil(capture.trace)
        XCTAssertEqual(capture.traceAbsent, "this build does not record view state")
        // The session and host records are either carried or declared absent
        // with a reason; they are never silently missing.
        XCTAssertTrue(capture.snapshot != nil || capture.snapshotAbsent != nil)

        let bundle = ReportAssembly.bundle(
            from: capture, draft: ReportDraft(note: "wrong"), build: "amux-ios/test",
            log: AppFiles.logTail)
        XCTAssertEqual(bundle.part(ReportAssembly.frameFile)?.present, true)
        XCTAssertEqual(
            bundle.part(ReportAssembly.traceFile)?.absenceReason,
            "this build does not record view state")
    }

    func testNothingOnScreenIsNothingToReport() {
        let freezer = ReportFreeze(window: { nil })
        XCTAssertNil(freezer.freeze())
    }
}
