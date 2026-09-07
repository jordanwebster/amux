import Foundation
import XCTest
@testable import AmuxCore

/// A store that answers one way, so every outcome a person can reach is
/// reachable without a sandbox account or a purchase sheet.
private struct OneStore: StoreFront {
    var offered: [Plan] = [
        Plan(id: Plan.monthlyID, period: .monthly, price: "£7.99"),
        Plan(id: Plan.yearlyID, period: .yearly, price: "£79.99", saving: "2 months free"),
    ]
    var purchase: Result<PurchaseOutcome, StoreError> = .success(.bought)
    var restored: Result<PurchaseOutcome, StoreError> = .success(.nothingToRestore)
    var listing: StoreError?

    func plans() async throws(StoreError) -> [Plan] {
        if let listing { throw listing }
        return offered
    }

    func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome {
        switch purchase {
        case .success(let outcome): return outcome
        case .failure(let error): throw error
        }
    }

    func restore() async throws(StoreError) -> PurchaseOutcome {
        switch restored {
        case .success(let outcome): return outcome
        case .failure(let error): throw error
        }
    }
}

@MainActor
final class PaywallStoreTests: XCTestCase {
    private func loaded(_ store: OneStore = OneStore()) async -> PaywallStore {
        let model = PaywallStore()
        await model.load(from: store)
        return model
    }

    func testTheYearIsChosenBeforeAnybodyChoosesAnything() async {
        let model = await loaded()
        XCTAssertEqual(model.plans.map(\.period), [.monthly, .yearly])
        XCTAssertEqual(model.chosen, .yearly)
        XCTAssertEqual(model.plan?.price, "£79.99")
    }

    func testChoosingTheMonthChangesWhatWouldBeBought() async {
        let model = await loaded()
        model.choose(.monthly)
        XCTAssertEqual(model.plan?.id, Plan.monthlyID)
    }

    func testBuyingLeavesTheAccountSubscribedThroughTheAppStore() async {
        let model = await loaded()
        let outcome = await model.buy(OneStore())
        XCTAssertEqual(outcome, .bought)
        XCTAssertEqual(model.phase, .bought(.appStore))
        XCTAssertEqual(model.entitlement, .active(source: .appStore, renews: nil))
        XCTAssertTrue(model.entitled)
    }

    func testClosingTheStoresSheetLeavesTheScreenExactlyWhereItWas() async {
        let model = await loaded()
        model.choose(.monthly)
        let outcome = await model.buy(OneStore(purchase: .success(.cancelled)))

        // Cancelling is a decision, not a failure: nothing is said, and the
        // plan that was chosen is still chosen.
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertEqual(model.phase, .ready)
        XCTAssertEqual(model.chosen, .monthly)
    }

    func testAPurchaseTheStoreCannotFinishStopsOfferingToBuyAgain() async {
        let model = await loaded()
        let outcome = await model.buy(OneStore(purchase: .success(.pending)))

        XCTAssertEqual(outcome, .pending)
        XCTAssertEqual(model.phase, .awaitingApproval)
        // Nothing is bought and nothing is owed: the entitlement has not moved.
        XCTAssertEqual(model.entitlement, Entitlement.none)
        XCTAssertFalse(model.entitled)
    }

    func testAPurchaseTheStoreRefusesSaysWhatTheStoreSaid() async {
        let model = await loaded()
        let outcome = await model.buy(
            OneStore(purchase: .failure(.failed("your payment method was declined"))))

        XCTAssertNil(outcome)
        XCTAssertEqual(model.phase, .failed("your payment method was declined"))
    }

    func testRestoringFindsWhatThisAppleAccountAlreadyHas() async {
        let model = await loaded()
        let outcome = await model.restore(OneStore(restored: .success(.bought)))

        XCTAssertEqual(outcome, .bought)
        XCTAssertEqual(model.entitlement, .active(source: .appStore, renews: nil))
    }

    func testRestoringNothingIsAnAnswerAndIsSaidAsOne() async {
        let model = await loaded()
        let outcome = await model.restore(OneStore())

        XCTAssertEqual(outcome, .nothingToRestore)
        XCTAssertEqual(
            model.phase, .failed("there is nothing on this Apple Account to restore"))
    }

    func testASubscriptionBoughtOnTheWebIsHonouredRatherThanSoldAgain() async {
        let model = await loaded()
        let renews = Date(timeIntervalSince1970: 1_701_004_800)

        // The CLI bought it; the account service is what says so. Nothing on
        // this phone was purchased and there is nothing to restore.
        model.entitled(.active(source: .web, renews: renews))

        XCTAssertTrue(model.entitled)
        XCTAssertEqual(model.source, .web)
        XCTAssertEqual(model.phase, .bought(.web))
        // The paywall refuses to sell a second subscription for the same thing.
        let outcome = await model.buy(OneStore())
        XCTAssertNil(outcome)
    }

    func testTheSourceIsSaidWhereverAnEntitlementIsShown() {
        XCTAssertEqual(
            Entitlement.active(source: .appStore, renews: nil).summary, "Active · App Store")
        XCTAssertEqual(Entitlement.active(source: .web, renews: nil).summary, "Active · amux.sh")
        XCTAssertEqual(
            Entitlement.lapsed(source: .web, endedAt: Date()).summary, "Ended · amux.sh")
        XCTAssertEqual(Entitlement.none.summary, "None")
    }

    func testAStoreWithNothingToSellSaysSoRatherThanDrawingAnEmptyList() async {
        let model = await loaded(OneStore(offered: []))
        XCTAssertTrue(model.plans.isEmpty)
        XCTAssertEqual(model.phase, .failed("the App Store has nothing to sell right now"))
    }

    func testAStoreThatCannotBeReachedSaysWhy() async {
        let model = await loaded(OneStore(listing: .unavailable("no network")))
        XCTAssertEqual(model.phase, .failed("no network"))
    }
}
