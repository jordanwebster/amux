import Foundation
import XCTest
@testable import AmuxCore

/// The three things a home row says that are computed rather than reported:
/// how old it is, what its finished turn changed, and whether this phone can
/// reach anything at all.
final class RowFactsTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_764_580_800)

    private func row(secondsAgo: TimeInterval) -> AgentRow {
        AgentRow(
            card: Made.card(1, name: "one", minutesAgo: secondsAgo / 60, now: now),
            unread: false)
    }

    func testAgeIsOneUnitAndAlwaysTheShortestTrueOne() {
        XCTAssertEqual(row(secondsAgo: 0).age(at: now), "0s")
        XCTAssertEqual(row(secondsAgo: 14).age(at: now), "14s")
        XCTAssertEqual(row(secondsAgo: 59).age(at: now), "59s")
        XCTAssertEqual(row(secondsAgo: 60).age(at: now), "1m")
        XCTAssertEqual(row(secondsAgo: 3_599).age(at: now), "59m")
        XCTAssertEqual(row(secondsAgo: 3_600).age(at: now), "1h")
        XCTAssertEqual(row(secondsAgo: 86_399).age(at: now), "23h")
        XCTAssertEqual(row(secondsAgo: 86_400).age(at: now), "1d")
        XCTAssertEqual(row(secondsAgo: 5 * 86_400).age(at: now), "5d")
    }

    /// A clock that has run backwards must not produce a negative age; the
    /// host's clock and this phone's are not the same clock.
    func testAnActivityInTheFutureIsNotNegative() {
        XCTAssertEqual(row(secondsAgo: -30).age(at: now), "0s")
    }

    func testArithmeticReadsAsCountsAndNotAsPunctuation() {
        XCTAssertEqual(
            TurnOutcome(files: 4, insertions: 118, deletions: 40, note: "3 tests added")
                .arithmetic,
            "4 files · +118 \u{2212}40 · 3 tests added")
        XCTAssertEqual(
            TurnOutcome(files: 1, insertions: 64, deletions: 2).arithmetic,
            "1 file · +64 \u{2212}2")
    }

    /// Absent counts mean "not known". A row states the outcome without the
    /// arithmetic rather than claiming nothing changed.
    func testAnOutcomeIsAbsentRatherThanZeroWhenNobodyCounted() {
        XCTAssertNil(row(secondsAgo: 10).outcome)
    }

    @MainActor
    func testTheGateNamesWhyNothingIsReachable() {
        let registry = AccountRegistry()
        XCTAssertEqual(registry.gate, .signedOut)

        let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com", displayName: "Ada")
        registry.add(ada)
        XCTAssertEqual(registry.gate, .unsubscribed)

        registry.entitlement(.active(source: .web, renews: nil), for: ada.id)
        XCTAssertEqual(registry.gate, .ready)

        registry.entitlement(.lapsed(source: .appStore, endedAt: now), for: ada.id)
        XCTAssertEqual(registry.gate, .unsubscribed)

        registry.signOut(ada.id)
        XCTAssertEqual(registry.gate, .signedOut)
    }
}
