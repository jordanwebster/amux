import AmuxCore
import AmuxDesign
import XCTest

@testable import AmuxTestSupport

/// The view-state trace is written by the app and read back by a replay,
/// possibly months later and possibly by a different build, so what it puts on
/// a line is pinned here.
final class TraceTests: XCTestCase {
    private let agent = AgentId(UUID(uuidString: "6f1c1f8e-0000-4000-8000-000000000001")!)
    private let ada = AccountEntry(
        account: SignedInAccount(id: AccountId("ada"), email: "ada@example.com"),
        entitlement: .active(grant: .granted, renews: nil))

    func testEveryTraceEventSurvivesTheWire() throws {
        let events: [TraceEvent] = [
            .route(.home),
            .route(.hosts),
            .route(.settings),
            .route(.conversation(agent)),
            .route(.review(agent)),
            .route(.screen("probe")),
            .sheet("new-agent"),
            .sheet(nil),
            .scroll(agent, 1_248.5),
            .appearance(.dark),
            .dynamicType("accessibility3"),
            .frozen(at: frozen, ordered: frozen.addingTimeInterval(-44)),
            .account(ada),
            .account(nil),
        ]
        XCTAssertEqual(try Trace.events(Trace.lines(events)), events)
    }

    /// An instant is written the way every other timestamp that leaves this
    /// app is, and comes back the same instant: a replay reads the ages on a
    /// rebuilt screen from it, and a second of drift is a second of wrong age
    /// on every row.
    private let frozen = Date(timeIntervalSince1970: 1_789_087_062.034)

    func testTheFrozenInstantsSurviveAsTimestamps() throws {
        let line = try Trace.lines([.frozen(at: frozen, ordered: frozen)])
        XCTAssertTrue(line.contains(#""at":"2026-09-11T00:37:42."#), line)
        XCTAssertEqual(
            try Trace.events(line), [.frozen(at: frozen, ordered: frozen)])
    }

    /// What a person was looking at is recorded as the place they were in, so
    /// a replay can put them back into the app rather than onto a picture of
    /// one screen. A conversation is named by agent id: a name belongs to the
    /// fleet and the fleet may rename it before anybody reads the report.
    func testAPlaceIsRecordedByWhatItIsAndWhatItIsAbout() throws {
        let written = try object(Trace.lines([.route(.conversation(agent))]))
        XCTAssertEqual(written["place"] as? String, "conversation")
        XCTAssertEqual(written["agent"] as? String, agent.description)
        XCTAssertNil(written["screen"])
        XCTAssertEqual(
            try object(Trace.lines([.route(.home)]))["place"] as? String, "home")
    }

    /// Nobody signed in is a fact worth recording: a replay that found no
    /// account fact could not tell a report taken signed out from a bundle
    /// written before accounts were recorded at all.
    func testASignedOutPhoneIsRecordedAsHavingNoAccount() throws {
        XCTAssertEqual(try Trace.events(Trace.lines([.account(nil)])), [.account(nil)])
        XCTAssertNil(try object(Trace.lines([.account(nil)]))["account"])
    }

    private func object(_ lines: String) throws -> [String: Any] {
        try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(lines.split(separator: "\n")[0].utf8))
                as? [String: Any])
    }

    func testATraceIsOneEventPerLine() throws {
        let lines = try Trace.lines([.route(.home), .appearance(.dark)])
        XCTAssertEqual(lines.split(separator: "\n").count, 2)
        XCTAssertTrue(lines.hasSuffix("\n"))
        let first = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(lines.split(separator: "\n")[0].utf8))
                as? [String: Any])
        XCTAssertEqual(first["kind"] as? String, "route")
        XCTAssertEqual(first["place"] as? String, "home")
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
            try Trace.events("\n" + #"{"kind":"route","place":"home"}"# + "\n\n"),
            [.route(.home)])
        XCTAssertThrowsError(try Trace.events(#"{"kind":"route","place":"atlantis"}"#))
        XCTAssertThrowsError(try Trace.events(#"{"kind":"levitate"}"#))
    }

    func testTheBundleNamesItsParts() {
        XCTAssertEqual(Trace.messagesFile, "msgs.jsonl")
        XCTAssertEqual(Trace.traceFile, "trace.jsonl")
        XCTAssertEqual(ReportAssembly.frameFile, "frame.png")
    }
}
