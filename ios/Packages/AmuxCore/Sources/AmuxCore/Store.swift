import Foundation
import Observation

/// One thing that can be bought, as the store describes it.
///
/// The price is a string and not a number, because it is the store's own
/// rendering in the person's own currency and locale. An app that formatted a
/// number itself would eventually print a price the App Store does not charge.
public struct Plan: Sendable, Equatable, Identifiable, Codable {
    public enum Period: String, Sendable, Equatable, Codable {
        case monthly
        case yearly

        public var title: String {
            switch self {
            case .monthly: "Monthly"
            case .yearly: "Yearly"
            }
        }

        /// How the button says what is being bought: *Subscribe · £7.99 a
        /// month*.
        public var spelled: String {
            switch self {
            case .monthly: "a month"
            case .yearly: "a year"
            }
        }
    }

    public var id: String
    public var period: Period
    public var price: String
    /// What the longer plan is worth saying about itself, where there is
    /// something. Nothing rather than "0% off" when the two are priced the
    /// same: a saving that is not one is an argument, not a fact.
    public var saving: String?

    public init(id: String, period: Period, price: String, saving: String? = nil) {
        self.id = id
        self.period = period
        self.price = price
        self.saving = saving
    }

    /// The products this app sells. The identifiers are the account service's,
    /// which is what the receipt is matched against when the store tells it
    /// somebody paid.
    public static let monthlyID = "amux_pro_monthly"
    public static let yearlyID = "amux_pro_yearly"
    public static let identifiers = [monthlyID, yearlyID]
}

/// What buying or restoring came to.
public enum PurchaseOutcome: Sendable, Equatable {
    /// Paid for, and the store says so now.
    case bought
    /// The store has taken it and cannot finish it yet — a child's purchase
    /// waiting on a parent, or a bank asking for a second factor. Nothing is
    /// owed and nothing has been bought; it may land minutes or days later.
    case pending
    case cancelled
    /// Nothing on this Apple Account to restore. Not a failure: it is the
    /// answer to the question the button asks.
    case nothingToRestore
}

public enum StoreError: Error, Sendable, Equatable {
    /// The store could not be reached or has nothing to sell.
    case unavailable(String)
    case failed(String)
}

/// Everything this app asks of the App Store.
///
/// One protocol, so the paywall never sees StoreKit: the production front and
/// the scripted one are the same shape, and every state a person can reach —
/// cancelled, pending, refused — is reachable in a test without a sandbox
/// account.
public protocol StoreFront: Sendable {
    func plans() async throws(StoreError) -> [Plan]
    func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome
    func restore() async throws(StoreError) -> PurchaseOutcome
}

/// Subscribing: what is on offer, what is chosen, and how a purchase went.
@MainActor
@Observable
public final class PaywallStore {
    public enum Phase: Sendable, Equatable {
        case ready
        /// The store's own sheet is up. Nothing on this screen may move while
        /// it is: what happens next is the store's to say.
        case buying
        /// Taken but not finished. The screen says so and stops offering to
        /// buy, because buying again would be a second charge for the same
        /// month.
        case awaitingApproval
        case failed(String)
        case bought(EntitlementSource)
    }

    public private(set) var plans: [Plan] = []
    /// Which plan is selected. Yearly by default, which is what the drawing
    /// shows: it is the cheaper of the two per month, and preselecting the
    /// dearer one would be the app arguing for its own revenue.
    public private(set) var chosen: Plan.Period = .yearly
    public private(set) var phase: Phase = .ready
    /// What this account is already entitled to. A subscription bought
    /// anywhere else — on the web, through the CLI — is honoured here: the
    /// screen says where it came from instead of offering to sell a second.
    public private(set) var entitlement: Entitlement

    public init(entitlement: Entitlement = .none, plans: [Plan] = [], phase: Phase = .ready) {
        self.entitlement = entitlement
        self.plans = plans
        self.phase = phase
    }

    public var plan: Plan? {
        plans.first { $0.period == chosen } ?? plans.first
    }

    /// Whether there is anything to sell this person.
    ///
    /// An active entitlement is an active entitlement wherever it was bought.
    /// The App Store's own rules aside, selling a second subscription to
    /// somebody who already pays for one on the web would be taking money for
    /// nothing.
    public var entitled: Bool {
        if case .active = entitlement { return true }
        return false
    }

    /// Where this device's entitlement came from, for the one line that says
    /// so. Nothing when there is none to describe.
    public var source: EntitlementSource? {
        switch entitlement {
        case .active(let source, _), .lapsed(let source, _): source
        case .none: nil
        }
    }

    /// Whether the screen is waiting on the store and must not be pressed.
    public var working: Bool { phase == .buying }

    public func choose(_ period: Plan.Period) {
        guard !working else { return }
        chosen = period
    }

    /// Asks the store what it has. A store with nothing to sell is said
    /// plainly rather than drawn as an empty list of plans.
    public func load(from store: any StoreFront) async {
        do {
            plans = try await store.plans()
            if plans.isEmpty { phase = .failed("the App Store has nothing to sell right now") }
        } catch {
            phase = Self.phase(after: error)
        }
    }

    /// Buys the chosen plan.
    ///
    /// Cancelling puts the screen back exactly where it was, with the same
    /// plan chosen: closing the store's sheet is a person deciding not to buy
    /// now, and an error over it would be the app arguing with that.
    @discardableResult
    public func buy(_ store: any StoreFront) async -> PurchaseOutcome? {
        guard !working, !entitled, let plan else { return nil }
        phase = .buying
        do {
            let outcome = try await store.buy(plan)
            settle(outcome)
            return outcome
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    /// Puts back a subscription this Apple Account already has.
    ///
    /// It exists for a person on a new phone, and it is the App Store's own
    /// requirement. Finding nothing is an answer and is said as one.
    @discardableResult
    public func restore(_ store: any StoreFront) async -> PurchaseOutcome? {
        guard !working else { return nil }
        phase = .buying
        do {
            let outcome = try await store.restore()
            settle(outcome)
            return outcome
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    private func settle(_ outcome: PurchaseOutcome) {
        switch outcome {
        case .bought:
            phase = .bought(.appStore)
            entitlement = .active(source: .appStore, renews: nil)
        case .pending: phase = .awaitingApproval
        case .cancelled: phase = .ready
        case .nothingToRestore:
            phase = .failed("there is nothing on this Apple Account to restore")
        }
    }

    /// What the cloud says this account is entitled to, which outranks
    /// anything the store said: the account service is where a subscription
    /// actually lives, and the store only knows about the ones bought on it.
    public func entitled(_ entitlement: Entitlement) {
        self.entitlement = entitlement
        if case .active(let source, _) = entitlement, phase != .buying {
            phase = .bought(source)
        }
    }

    static func phase(after error: StoreError) -> Phase {
        switch error {
        case .unavailable(let what): .failed(what)
        case .failed(let what): .failed(what)
        }
    }
}

extension EntitlementSource {
    /// Where a subscription was bought, as a person would say it. The App
    /// Store's own name, because that is where they would go to cancel it.
    public var named: String {
        switch self {
        case .appStore: "App Store"
        case .web: "amux.sh"
        }
    }

    /// The same place as a sentence names it. Apart from `named` because a row
    /// reporting where a subscription came from wants the bare name, and a
    /// sentence about going there to stop it wants the article.
    public var place: String {
        switch self {
        case .appStore: "the App Store"
        case .web: "amux.sh"
        }
    }
}

extension Entitlement {
    /// The one line a settings row shows: what this account has and where it
    /// came from. *Active · App Store*, *Ended · amux.sh*, *None*.
    public var summary: String {
        switch self {
        case .none: "None"
        case .active(let source, _): "Active · \(source.named)"
        case .lapsed(let source, _): "Ended · \(source.named)"
        }
    }
}
