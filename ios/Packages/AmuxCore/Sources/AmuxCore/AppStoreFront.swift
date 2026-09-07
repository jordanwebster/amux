import Foundation
import StoreKit

/// The App Store, as this app uses it.
///
/// Everything StoreKit is here and nowhere else. A screen holds a `StoreFront`
/// and cannot tell whether there is a real store behind it, which is what lets
/// a cancelled purchase, a pending one and a refused one all be photographed
/// and driven without a sandbox account.
public struct AppStoreFront: StoreFront {
    private let identifiers: [String]

    public init(identifiers: [String] = Plan.identifiers) {
        self.identifiers = identifiers
    }

    public func plans() async throws(StoreError) -> [Plan] {
        let products: [Product]
        do {
            products = try await Product.products(for: identifiers)
        } catch {
            throw StoreError.unavailable(error.localizedDescription)
        }
        var plans = products.compactMap(Self.plan)
        // What the year is worth saying about itself, worked out from the two
        // prices rather than written down: a saving typed into the app would
        // go on claiming two months after a price change made it one.
        if let saving = Self.monthsFree(products),
           let year = plans.firstIndex(where: { $0.period == .yearly }) {
            plans[year].saving = saving
        }
        // Longer plan last, the way the drawing reads: the cheaper unit price
        // is the one worth arriving at.
        return plans.sorted { $0.period == .monthly && $1.period == .yearly }
    }

    public func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome {
        let products: [Product]
        do {
            products = try await Product.products(for: [plan.id])
        } catch {
            throw StoreError.unavailable(error.localizedDescription)
        }
        guard let product = products.first else {
            throw StoreError.unavailable("the App Store has no such subscription")
        }
        let result: Product.PurchaseResult
        do {
            result = try await product.purchase()
        } catch {
            throw StoreError.failed(error.localizedDescription)
        }
        switch result {
        case .success(let verification):
            let transaction = try Self.verified(verification)
            // Finished here rather than left for a later launch: an unfinished
            // transaction is offered again on every start, which a person
            // reads as the app asking them to pay twice.
            await transaction.finish()
            return .bought
        case .userCancelled: return .cancelled
        case .pending: return .pending
        @unknown default:
            // A result this build has no name for is not silently a purchase.
            throw StoreError.failed("the App Store answered in a way this app does not know")
        }
    }

    /// Puts back what this Apple Account already has.
    ///
    /// `AppStore.sync()` is asked for first because that is what the button
    /// means to somebody pressing it — go and look again — and only then are
    /// the entitlements read.
    public func restore() async throws(StoreError) -> PurchaseOutcome {
        do {
            try await AppStore.sync()
        } catch {
            // Cancelling the sign-in the sync asks for is not a failure to
            // restore; it is a person deciding not to.
            if case .userCancelled? = error as? StoreKitError { return .cancelled }
            throw StoreError.failed(error.localizedDescription)
        }
        for await entitlement in Transaction.currentEntitlements {
            guard let transaction = try? Self.verified(entitlement) else { continue }
            if identifiers.contains(transaction.productID) { return .bought }
        }
        return .nothingToRestore
    }

    /// What the store signed, or nothing.
    ///
    /// An unverified transaction is refused rather than trusted: it is the one
    /// thing StoreKit's own signature is for, and an app that took it anyway
    /// would be honouring a receipt anybody could write.
    private static func verified(
        _ result: VerificationResult<Transaction>
    ) throws(StoreError) -> Transaction {
        switch result {
        case .verified(let transaction): return transaction
        case .unverified:
            throw StoreError.failed("the App Store could not verify that purchase")
        }
    }

    /// How many months of the year the yearly plan does not charge for, where
    /// that is a whole month or more.
    private static func monthsFree(_ products: [Product]) -> String? {
        guard let monthly = products.first(where: { $0.id == Plan.monthlyID })?.price,
              let yearly = products.first(where: { $0.id == Plan.yearlyID })?.price,
              monthly > 0
        else { return nil }
        let year = (yearly as NSDecimalNumber).doubleValue
        let month = (monthly as NSDecimalNumber).doubleValue
        let free = Int((12 - year / month).rounded())
        guard free >= 1 else { return nil }
        return free == 1 ? "1 month free" : "\(free) months free"
    }

    private static func plan(_ product: Product) -> Plan? {
        guard let subscription = product.subscription else { return nil }
        let period: Plan.Period
        switch (subscription.subscriptionPeriod.unit, subscription.subscriptionPeriod.value) {
        case (.month, 1): period = .monthly
        case (.year, 1): period = .yearly
        // Anything else is a product this app does not draw a row for, and
        // guessing which row it belongs in would put a price under the wrong
        // word.
        default: return nil
        }
        return Plan(id: product.id, period: period, price: product.displayPrice)
    }
}
