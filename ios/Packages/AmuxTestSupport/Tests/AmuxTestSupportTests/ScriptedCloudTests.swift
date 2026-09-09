import AmuxCore
import XCTest
@testable import AmuxTestSupport

final class ScriptedCloudTests: XCTestCase {
    private let ada = AccountId("ada")

    func testItAnswersWhatTheStateSaysAndRecordsWhatItWasAsked() async throws {
        let cloud = ScriptedCloudService(state: ScriptedCloudState(
            entitlement: .active(grant: .purchased(.appStore), renews: Scenario.now),
            token: "connect-me",
            deletion: .deleted,
            upload: .accepted(id: "report-7")))

        let account = try await cloud.signIn(presenting: ScriptedWebAuth())
        XCTAssertEqual(account.email, "ada@example.com")
        let entitlement = try await cloud.entitlement(ada)
        XCTAssertEqual(entitlement, .active(grant: .purchased(.appStore), renews: Scenario.now))
        let token = try await cloud.connectToken(ada)
        XCTAssertEqual(token.bearer, "connect-me")
        let facts = try await cloud.account(ada)
        XCTAssertEqual(facts.entitlement, .active(grant: .purchased(.appStore), renews: Scenario.now))
        let deletion = try await cloud.requestDeletion(ada, confirmedEmail: "ada@example.com")
        XCTAssertEqual(deletion, .deleted)
        let receipt = try await cloud.uploadReport(ada, bundle: ReportBundle(
            parts: [ReportPart(name: "report.json", data: Data("{}".utf8)),
                    ReportPart(name: "frame.png", data: Data([0x89])),
                    ReportPart(name: "trace.jsonl", absenceReason: "tracing was off")]))
        XCTAssertEqual(receipt.id, "report-7")

        XCTAssertEqual(cloud.calls, [
            .signIn,
            .entitlement(ada),
            .connectToken(ada),
            .account(ada),
            .requestDeletion(ada, confirmedEmail: "ada@example.com"),
            .uploadReport(ada, parts: ["report.json", "frame.png", "trace.jsonl"]),
        ])
    }

    func testEveryFailureIsDeclaredRatherThanImprovised() async {
        let cancelled = ScriptedCloudService(state: ScriptedCloudState(signIn: .cancelled))
        await assert(CloudError.cancelled) { try await cancelled.signIn(presenting: ScriptedWebAuth()) }

        let refused = ScriptedCloudService(state: ScriptedCloudState(signIn: .refused("no such account")))
        await assert(CloudError.refused("no such account")) {
            try await refused.signIn(presenting: ScriptedWebAuth())
        }

        let offline = ScriptedCloudService(state: ScriptedCloudState(signIn: .offline, token: nil))
        await assert(CloudError.network("offline")) { try await offline.signIn(presenting: ScriptedWebAuth()) }
        await assert(CloudError.unauthenticated) { try await offline.connectToken(ada) }

        let unreachable = ScriptedCloudService(state: ScriptedCloudState(upload: .offline))
        await assert(CloudError.network("offline")) {
            try await unreachable.uploadReport(ada, bundle: ReportBundle(parts: []))
        }
    }

    /// Latency is a state, not an accident: a screen that only exists while a
    /// request is in flight cannot be captured unless the test can hold the
    /// request open.
    func testLatencyIsHonoured() async throws {
        let cloud = ScriptedCloudService(state: ScriptedCloudState(latency: .milliseconds(120)))
        let started = ContinuousClock.now
        _ = try await cloud.entitlement(ada)
        XCTAssertGreaterThanOrEqual(ContinuousClock.now - started, .milliseconds(100))
    }

    func testAWebSignInThatIsCancelledNeverReturnsAnAccount() async throws {
        // The app hands off to the browser and is told what came back; it has
        // no credential field of its own to fall back on.
        let cloud = ScriptedCloudService()
        let account = try await cloud.signIn(presenting: ScriptedWebAuth(outcome: .cancelled))
        XCTAssertEqual(account.id, ScriptedCloudState.ada.id)
        XCTAssertEqual(cloud.calls, [.signIn])
    }

    func testTheStateCanBeRewrittenBetweenCalls() async throws {
        let cloud = ScriptedCloudService(state: .unsubscribed)
        let before = try await cloud.entitlement(ada)
        XCTAssertEqual(before, .none)
        cloud.scripted.entitlement = .active(grant: .purchased(.web), renews: nil)
        let after = try await cloud.entitlement(ada)
        XCTAssertEqual(after, .active(grant: .purchased(.web), renews: nil))
        cloud.reset()
        XCTAssertTrue(cloud.calls.isEmpty)
    }

    /// What a report was is only answerable from the bundle itself: the names
    /// of its parts say nothing about the reason beside a part that is not
    /// there, and a retry has to be readable as the same bundle as the attempt
    /// that failed.
    func testItKeepsEveryBundleItWasHandedIncludingTheOnesItRefused() async throws {
        let cloud = ScriptedCloudService(state: ScriptedCloudState(upload: .refused("too large")))
        let bundle = ReportBundle(parts: [
            ReportPart(name: "report.json", data: Data("{}".utf8)),
            ReportPart(name: "daemon.json", absenceReason: "the embedded daemon did not answer"),
        ])
        do {
            _ = try await cloud.uploadReport(ada, bundle: bundle)
            XCTFail("a refused upload answered with a receipt")
        } catch {
            XCTAssertEqual(error, CloudError.refused("too large"))
        }
        cloud.scripted.upload = .accepted(id: "report-9")
        let receipt = try await cloud.uploadReport(ada, bundle: bundle)

        XCTAssertEqual(receipt.id, "report-9")
        XCTAssertEqual(cloud.uploaded, [bundle, bundle])
        XCTAssertEqual(
            cloud.uploaded.last?.part("daemon.json")?.absenceReason,
            "the embedded daemon did not answer")
    }

    /// A driver writes what happens to a report in the door's own words, and
    /// changing that must not disturb what it has already said about signing
    /// in — which is why a refused upload has a reason of its own.
    func testAScriptSaysWhatBecomesOfAReportWithoutTouchingTheRest() {
        var script = CloudScript()
        script.signIn = "refused"
        script.reason = "amux.sh has no account for this sign-in"
        script.upload = "refused"
        script.uploadReason = "amux.sh could not take this report"

        XCTAssertEqual(script.state.signIn, .refused("amux.sh has no account for this sign-in"))
        XCTAssertEqual(script.state.upload, .refused("amux.sh could not take this report"))
        XCTAssertEqual(CloudScript().state.upload, .accepted(id: "report-1"))
    }

    func testEveryFixtureDeclaresItsCloud() {
        for fixture in Fixtures.all {
            XCTAssertNotNil(ScriptedCloudService(state: fixture.cloud).scripted.latency)
        }
        XCTAssertEqual(Fixtures.named("first-run")?.cloud.entitlement, Entitlement.none)
        XCTAssertEqual(Fixtures.named("upload-failed")?.cloud.upload, .offline)
    }

    private func assert(
        _ expected: CloudError, _ body: () async throws -> some Sendable,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await body()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch let error as CloudError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("expected \(expected), got \(error)", file: file, line: line)
        }
    }
}
