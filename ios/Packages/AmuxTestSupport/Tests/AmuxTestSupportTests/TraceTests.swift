import AmuxCore
import AmuxDesign
import XCTest

@testable import AmuxTestSupport

/// The view-state trace is written by the app and read back by a replay,
/// possibly months later and possibly by a different build, so what it puts on
/// a line is pinned here.
final class TraceTests: XCTestCase {
    private let agent = AgentId(UUID(uuidString: "6f1c1f8e-0000-4000-8000-000000000001")!)

    func testEveryTraceEventSurvivesTheWire() throws {
        let events: [TraceEvent] = [
            .route("home"),
            .sheet("new-agent"),
            .sheet(nil),
            .scroll(agent, 1_248.5),
            .appearance(.dark),
            .dynamicType("accessibility3"),
        ]
        XCTAssertEqual(try Trace.events(Trace.lines(events)), events)
    }

    func testATraceIsOneEventPerLine() throws {
        let lines = try Trace.lines([.route("home"), .appearance(.dark)])
        XCTAssertEqual(lines.split(separator: "\n").count, 2)
        XCTAssertTrue(lines.hasSuffix("\n"))
        let first = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(lines.split(separator: "\n")[0].utf8))
                as? [String: Any])
        XCTAssertEqual(first["kind"] as? String, "route")
        XCTAssertEqual(first["screen"] as? String, "home")
    }

    /// A dismissed sheet is a real event — it says the screen underneath came
    /// back — so it is written with its field absent rather than dropped.
    func testADismissedSheetIsRecorded() throws {
        let events = try Trace.events(Trace.lines([.sheet(nil)]))
        XCTAssertEqual(events, [.sheet(nil)])
        XCTAssertNil(
            try XCTUnwrap(
                JSONSerialization.jsonObject(with: Data(Trace.lines([.sheet(nil)]).utf8))
                    as? [String: Any])["sheet"])
    }

    /// Blank lines happen when a bundle is copied about; an event nobody
    /// defined is a bundle from a build that knew something this one does not,
    /// and reading it as though it were empty would replay the wrong screen.
    func testBlankLinesAreSkippedAndUnknownEventsAreRefused() throws {
        XCTAssertEqual(
            try Trace.events("\n" + #"{"kind":"route","screen":"home"}"# + "\n\n"),
            [.route("home")])
        XCTAssertThrowsError(try Trace.events(#"{"kind":"levitate"}"#))
    }

    func testTheBundleNamesItsParts() {
        XCTAssertEqual(Trace.messagesFile, "msgs.jsonl")
        XCTAssertEqual(Trace.traceFile, "trace.jsonl")
        XCTAssertEqual(Trace.screenFile, "screen.png")
    }
}
