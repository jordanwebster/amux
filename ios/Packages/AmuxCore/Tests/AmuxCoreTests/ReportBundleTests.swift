import Foundation
import XCTest
@testable import AmuxCore

/// A capture with everything in it, so a test that wants a part missing takes
/// that one part away rather than building a different capture.
private func wholeCapture() -> ReportCapture {
    ReportCapture(
        frame: FrozenFrame(png: Data([0x89, 0x50, 0x4E, 0x47]), width: 402, height: 874, scale: 3),
        snapshot: """
            {"msgs":{"format_version":1,"checkpoint":{"agents":[]},\
            "msgs":["{\\"kind\\":\\"fleet\\"}"]},"daemon":"{\\"hosts\\":[]}"}
            """,
        trace: "{\"kind\":\"route\",\"screen\":\"run\"}\n",
        route: "run")
}

private let noLog = Result<String, PartAbsent>.failure(
    PartAbsent("this app logs through the system, which keeps no file it can read back"))

/// `report.json` as a reader sees it: parsed back out of the bytes that would
/// have been sent, rather than read off the value that produced them.
private func header(_ bundle: ReportBundle) throws -> [String: Any] {
    let report = try XCTUnwrap(bundle.part(ReportAssembly.reportFile))
    let bytes = try XCTUnwrap(report.data)
    return try XCTUnwrap(JSONSerialization.jsonObject(with: bytes) as? [String: Any])
}

private func parts(_ bundle: ReportBundle) throws -> [String: Any] {
    try XCTUnwrap(try header(bundle)["parts"] as? [String: Any])
}

final class ReportBundleTests: XCTestCase {
    /// Every part the layout names is declared, and each declaration matches a
    /// file that is actually here. The account service refuses a bundle whose
    /// declarations and whose files disagree, so this is the whole contract.
    func testEveryPartIsDeclaredAndCarried() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(),
            draft: ReportDraft(note: "the queued message stays on screen"),
            build: "amux-ios/0.1.0", log: .success("two lines\nof log\n"))

        let declared = try parts(bundle)
        for part in ["frame", "trace", "msgs", "daemon", "log"] {
            XCTAssertEqual(
                declared[part] as? String, "present",
                "\(part) should be declared present")
        }
        for file in [
            ReportAssembly.reportFile, ReportAssembly.frameFile, ReportAssembly.traceFile,
            ReportAssembly.messagesFile, ReportAssembly.daemonFile, ReportAssembly.logFile,
        ] {
            XCTAssertEqual(
                bundle.part(file)?.present, true, "\(file) should be carried")
        }
        XCTAssertEqual(bundle.parts.first?.name, ReportAssembly.reportFile)
    }

    /// A part that is missing says why. A reader who finds no log has to be
    /// able to tell "withheld" from "lost" from "never existed".
    func testAMissingPartCarriesTheReasonItIsMissing() throws {
        var capture = wholeCapture()
        capture.snapshot = nil
        capture.snapshotAbsent = "nothing was connected"
        capture.trace = nil
        capture.traceAbsent = "the view-state recording could not be written"

        let bundle = ReportAssembly.bundle(
            from: capture, draft: ReportDraft(), build: "amux-ios/0.1.0", log: noLog)

        let declared = try parts(bundle)
        for (part, reason) in [
            ("trace", "the view-state recording could not be written"),
            ("msgs", "nothing was connected"),
            ("daemon", "nothing was connected"),
            ("log", "this app logs through the system, which keeps no file it can read back"),
        ] {
            let absence = try XCTUnwrap(
                (declared[part] as? [String: Any])?["absent"] as? [String: Any],
                "\(part) should be declared absent")
            XCTAssertEqual(absence["reason"] as? String, reason)
        }
        // Declared absent means not carried. A bundle that declared a part
        // absent and shipped it anyway is refused by the account service.
        for file in [
            ReportAssembly.traceFile, ReportAssembly.messagesFile,
            ReportAssembly.daemonFile, ReportAssembly.logFile,
        ] {
            XCTAssertEqual(bundle.part(file)?.present, false, "\(file) should not be carried")
        }
        XCTAssertNil(declared["trace_kind"], "a bundle with no trace names no recorder")
    }

    /// Which recorder made the trace decides where the trace can be replayed,
    /// so a trace that names none is not worth storing. The phone's came from
    /// a native view, which a terminal cannot put back.
    func testATraceNamesTheRecorderThatMadeIt() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(), draft: ReportDraft(), build: "amux-ios/0.1.0", log: noLog)

        XCTAssertEqual(try parts(bundle)["trace_kind"] as? String, "native_view")
    }

    /// The picture alone says nothing about how big the screen was, so the
    /// rectangles beside it would mean nothing without the geometry.
    func testTheFrameRecordsThePointsItsRectanglesAreMeasuredIn() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(),
            draft: ReportDraft(marks: [
                ReportMark(x: 24, y: 236.5, width: 354, height: 30, note: "this row"),
            ]),
            build: "amux-ios/0.1.0", log: noLog)

        let read = try header(bundle)
        let geometry = try XCTUnwrap(read["image_frame"] as? [String: Any])
        XCTAssertEqual(geometry["width_pt"] as? Double, 402)
        XCTAssertEqual(geometry["height_pt"] as? Double, 874)
        XCTAssertEqual(geometry["scale"] as? Int, 3)
        // A terminal's viewport is cells and this frame has none.
        XCTAssertTrue(read["viewport"] is NSNull)

        let marks = try XCTUnwrap(read["marks"] as? [[String: Any]])
        XCTAssertEqual(marks.count, 1)
        XCTAssertEqual(marks[0]["x"] as? Double, 24)
        XCTAssertEqual(marks[0]["y"] as? Double, 236.5)
        XCTAssertEqual(marks[0]["note"] as? String, "this row")
    }

    /// The header is read by a Rust type whose spellings are the contract.
    func testTheHeaderIsWrittenInTheWordsTheReaderKnows() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(), draft: ReportDraft(note: "it stays on screen"),
            build: "amux-ios/0.1.0",
            createdAt: Date(timeIntervalSince1970: 1_788_395_144.348), log: noLog)

        let read = try header(bundle)
        XCTAssertEqual(read["schema_version"] as? Int, 2)
        XCTAssertEqual(read["build"] as? String, "amux-ios/0.1.0")
        XCTAssertEqual(read["kind"] as? String, "bug")
        XCTAssertEqual(read["status"] as? String, "open")
        XCTAssertEqual(read["note"] as? String, "it stays on screen")
        XCTAssertEqual(read["detail"] as? String, "run")
        XCTAssertEqual(read["created_at"] as? String, "2026-09-03T00:25:44.348Z")
        // Nothing on the phone replayed it. A native trace replays on the
        // platform that drew it, so the phone declines to claim a verdict.
        XCTAssertEqual(read["replay"] as? String, "unchecked")
    }

    /// `msgs.jsonl` is the checkpoint as a header line with the folded
    /// messages under it, and the daemon's dump is its own file.
    func testTheRuntimeRecordingIsSplitIntoTheTwoFilesABundleCarries() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(), draft: ReportDraft(), build: "amux-ios/0.1.0", log: noLog)

        let messages = try XCTUnwrap(bundle.part(ReportAssembly.messagesFile)?.data)
        let lines = String(decoding: messages, as: UTF8.self)
            .split(separator: "\n", omittingEmptySubsequences: true)
        XCTAssertEqual(lines.count, 2)
        let head = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(lines[0].utf8)) as? [String: Any])
        XCTAssertEqual(head["format_version"] as? Int, 1)
        XCTAssertNotNil(head["checkpoint"])
        XCTAssertEqual(lines[1], "{\"kind\":\"fleet\"}")

        let daemon = try XCTUnwrap(bundle.part(ReportAssembly.daemonFile)?.data)
        XCTAssertEqual(String(decoding: daemon, as: UTF8.self), "{\"hosts\":[]}")
    }

    /// A runtime that answered with no recording in it is a missing part with
    /// its own reason, not a bundle that quietly carries an empty file.
    func testARuntimeThatAnsweredWithoutARecordingSaysSo() throws {
        var capture = wholeCapture()
        capture.snapshot = "{\"daemon_absent_reason\":\"the daemon did not reply in time\"}"

        let bundle = ReportAssembly.bundle(
            from: capture, draft: ReportDraft(), build: "amux-ios/0.1.0", log: noLog)

        let declared = try parts(bundle)
        let msgs = try XCTUnwrap((declared["msgs"] as? [String: Any])?["absent"] as? [String: Any])
        XCTAssertEqual(msgs["reason"] as? String, "the runtime answered without a recording in it")
        let daemon = try XCTUnwrap(
            (declared["daemon"] as? [String: Any])?["absent"] as? [String: Any])
        XCTAssertEqual(daemon["reason"] as? String, "the daemon did not reply in time")
    }

    /// A failed upload keeps everything. Somebody who wrote three notes about
    /// a bug on a train must not lose them to a tunnel.
    @MainActor
    func testAFailedUploadKeepsTheDraftSoRetryIsOnePress() async throws {
        let reports = ReportStore(
            capture: wholeCapture(),
            draft: ReportDraft(note: "it stays on screen", marks: [
                ReportMark(x: 1, y: 2, width: 3, height: 4, note: "this row"),
            ]),
            open: true)
        let cloud = OneUpload(.failure(.network("offline")))

        await reports.send(with: cloud, as: AccountId("ada"), build: "amux-ios/0.1.0", log: noLog)

        XCTAssertEqual(reports.sending, .failed("offline"))
        XCTAssertEqual(reports.draft.note, "it stays on screen")
        XCTAssertEqual(reports.draft.marks.count, 1)
        XCTAssertNotNil(reports.capture)

        cloud.answer = .success(ReportReceipt(id: "report-7", receivedAt: Date()))
        await reports.send(with: cloud, as: AccountId("ada"), build: "amux-ios/0.1.0", log: noLog)

        XCTAssertEqual(reports.sending, .sent(ReportReceipt(id: "report-7", receivedAt: cloud.at)))
        // The same report went both times, with everything that was written on
        // it: a retry is not a second, emptier report.
        XCTAssertEqual(cloud.sent.count, 2)
        XCTAssertEqual(cloud.sent.last?.map(\.name), [
            ReportAssembly.reportFile, ReportAssembly.frameFile, ReportAssembly.traceFile,
            ReportAssembly.messagesFile, ReportAssembly.daemonFile, ReportAssembly.logFile,
        ])
    }
}

/// A cloud that answers an upload one way and remembers what it was handed.
@MainActor
private final class OneUpload: CloudService {
    var answer: Result<ReportReceipt, CloudError>
    private(set) var sent: [[ReportPart]] = []
    let at = Date(timeIntervalSince1970: 1_788_395_144)

    init(_ answer: Result<ReportReceipt, CloudError>) {
        self.answer = answer
    }

    func uploadReport(
        _ id: AccountId, bundle: ReportBundle
    ) async throws(CloudError) -> ReportReceipt {
        sent.append(bundle.parts)
        switch answer {
        case .success(let receipt): return ReportReceipt(id: receipt.id, receivedAt: at)
        case .failure(let error): throw error
        }
    }

    func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        throw .unauthenticated
    }
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts { throw .timeout }
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { .none }
    func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken { throw .timeout }
    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome { .deleted }
}
