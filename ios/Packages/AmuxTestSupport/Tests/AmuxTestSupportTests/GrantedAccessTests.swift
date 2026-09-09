import AmuxCore
import XCTest
@testable import AmuxTestSupport

/// An account that was given its access rather than buying it.
///
/// This is the state the app got wrong for real people: the relay let such an
/// account in while the phone, which asked whether a subscription existed,
/// found none and drew a paywall. The scripted cloud can be entitled that way
/// now, and these drive the two screens that read an entitlement from it.
@MainActor
final class GrantedAccessTests: XCTestCase {
    private let ada = ScriptedCloudState.ada

    /// The home screen's gate is open, so nothing on it offers to subscribe.
    func testTheHomeDoesNotOfferToSellToAnAccountTheRelayWouldLetIn() async throws {
        let cloud = ScriptedCloudService(state: .granted)
        let entitlement = try await cloud.entitlement(ada.id)
        let registry = AccountRegistry()
        registry.add(ada, entitlement: entitlement)

        XCTAssertEqual(registry.gate, .ready)
        XCTAssertNotEqual(registry.gate, .unsubscribed)
    }

    /// Settings names what the account has without naming a store it was
    /// bought in or a subscription anybody could go and manage.
    func testSettingsSaysWhatWasGivenWithoutInventingAStore() async throws {
        let cloud = ScriptedCloudService(state: .granted)
        let entitlement = try await cloud.entitlement(ada.id)

        XCTAssertEqual(entitlement.noun, "Pro")
        XCTAssertEqual(entitlement.summary, "Active · Included")
        XCTAssertFalse(entitlement.summary.contains("App Store"))
        XCTAssertFalse(entitlement.summary.contains("amux.sh"))
    }

    /// The page behind that row does not sell a second subscription, and does
    /// not offer to manage one that does not exist.
    func testThePaywallHonoursGivenAccessRatherThanSellingIt() async throws {
        let cloud = ScriptedCloudService(state: .granted)
        let paywall = PaywallStore()
        paywall.entitled(try await cloud.entitlement(ada.id))

        XCTAssertTrue(paywall.entitled)
        XCTAssertEqual(paywall.grant, .granted)
        XCTAssertEqual(paywall.phase, .entitled(.granted))
    }
}
