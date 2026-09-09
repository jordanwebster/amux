import AmuxCore
import Foundation

/// What the App Store will answer, declared by the test rather than discovered
/// at runtime.
///
/// The store cannot be driven from outside the app — a purchase sheet belongs
/// to another process and a sandbox account is a person with a password — so
/// every outcome the paywall can reach is stated here instead. That is what
/// makes cancellation, a pending purchase and a refusal states a capture and a
/// journey can actually stand in.
public struct ScriptedStoreState: Codable, Sendable, Equatable {
    public var plans: [Plan]
    /// What pressing Subscribe comes to.
    public var purchase: Outcome
    /// What Restore Purchases comes to.
    public var restore: Outcome
    /// How long the store takes to answer. Zero is instant.
    public var latency: Duration

    public init(
        plans: [Plan] = Self.offered,
        purchase: Outcome = .bought,
        restore: Outcome = .nothingToRestore,
        latency: Duration = .zero
    ) {
        self.plans = plans
        self.purchase = purchase
        self.restore = restore
        self.latency = latency
    }

    public enum Outcome: Codable, Sendable, Equatable {
        case bought
        case pending
        case cancelled
        case nothingToRestore
        case fails(String)
    }

    /// The two subscriptions, at the prices the design was drawn with.
    public static let offered = [
        Plan(id: Plan.monthlyID, period: .monthly, price: "£7.99"),
        Plan(id: Plan.yearlyID, period: .yearly, price: "£79.99", saving: "2 months free"),
    ]

    /// What a scripted purchase carries where the App Store would have put a
    /// signed transaction. It is not a JWS and nothing verifies it: the only
    /// thing that reads it is the scripted account service on the other side.
    public static let signedTransaction = "scripted.signed.transaction"

    /// A store that has nothing to sell — no network, or a build the App Store
    /// has never heard of.
    public static var silent: ScriptedStoreState {
        ScriptedStoreState(plans: [], purchase: .fails("the App Store could not be reached"))
    }
}

/// One call the paywall made, in the order it made it.
public enum StoreCall: Sendable, Equatable {
    case plans
    case buy(String)
    case restore
    case finish(String)
    case unfinished
}

/// The App Store, scripted. It answers exactly what the state says and records
/// what it was asked.
public final class ScriptedStoreFront: StoreFront, @unchecked Sendable {
    private let lock = NSLock()
    private var state: ScriptedStoreState
    private var recorded: [StoreCall] = []
    /// Purchases bought here and not yet finished, exactly as the real store
    /// holds them: something is only taken off this list once whoever bought
    /// it says it is dealt with.
    private var holding: [SignedPurchase] = []

    public init(state: ScriptedStoreState = ScriptedStoreState()) {
        self.state = state
    }

    public var calls: [StoreCall] { lock.withLock { recorded } }

    public var scripted: ScriptedStoreState {
        get { lock.withLock { state } }
        set { lock.withLock { state = newValue } }
    }

    public func reset() {
        lock.withLock {
            recorded = []
            holding = []
        }
    }

    private func record(_ call: StoreCall) -> ScriptedStoreState {
        lock.withLock {
            recorded.append(call)
            return state
        }
    }

    private func wait(_ state: ScriptedStoreState) async {
        guard state.latency > .zero else { return }
        try? await Task.sleep(for: state.latency)
    }

    public func plans() async throws(StoreError) -> [Plan] {
        let state = record(.plans)
        await wait(state)
        return state.plans
    }

    public func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome {
        let state = record(.buy(plan.id))
        await wait(state)
        return try outcome(state.purchase, of: plan.id)
    }

    public func restore() async throws(StoreError) -> PurchaseOutcome {
        let state = record(.restore)
        await wait(state)
        return try outcome(state.restore, of: Plan.yearlyID)
    }

    /// What the store is still holding, and what has been finished with it.
    ///
    /// A driver reads this to prove the order: a purchase reaches the account
    /// service before it is finished, and one the cloud refused is still here
    /// afterwards to be sent again.
    public var held: [SignedPurchase] { lock.withLock { holding } }

    public func finish(_ purchase: SignedPurchase) async {
        lock.withLock {
            recorded.append(.finish(purchase.id))
            holding.removeAll { $0.id == purchase.id }
        }
    }

    public func unfinished() async -> [SignedPurchase] {
        lock.withLock {
            recorded.append(.unfinished)
            return holding
        }
    }

    /// Nothing arrives after the fact in a scripted store: an approval is
    /// something the App Store decides, and a state a driver declares is
    /// already the state after it decided.
    public func approvals() -> AsyncStream<SignedPurchase> {
        AsyncStream { $0.finish() }
    }

    private func outcome(
        _ declared: ScriptedStoreState.Outcome, of plan: String
    ) throws(StoreError) -> PurchaseOutcome {
        switch declared {
        case .bought:
            // A stand-in for what the App Store would have signed. It never
            // leaves the app's own doubles, and the account service that would
            // check it is scripted too.
            let purchase = SignedPurchase(
                id: "scripted-transaction", productID: plan,
                signed: ScriptedStoreState.signedTransaction)
            lock.withLock { holding.append(purchase) }
            return .bought(purchase)
        case .pending: return .pending
        case .cancelled: return .cancelled
        case .nothingToRestore: return .nothingToRestore
        case .fails(let reason): throw StoreError.failed(reason)
        }
    }
}

/// What the scripted App Store will answer, in the door's own words. The same
/// spelling as the cloud's script, for the same reason.
public struct StoreScript: Codable, Sendable, Equatable {
    /// Whether there is anything on sale at all. A store with nothing to sell
    /// is a build the App Store has never heard of, or no network.
    public var selling = true
    /// What pressing Subscribe comes to: `bought`, `pending`, `cancelled`,
    /// `nothingToRestore` or `fails`.
    public var purchase = "bought"
    /// What Restore Purchases comes to, in the same words.
    public var restore = "nothingToRestore"
    /// What the store said when it failed.
    public var reason = "the App Store could not complete this purchase"
    public var latencyMillis = 0

    public init() {}

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        selling = try fields.decodeIfPresent(Bool.self, forKey: .selling) ?? selling
        purchase = try fields.decodeIfPresent(String.self, forKey: .purchase) ?? purchase
        restore = try fields.decodeIfPresent(String.self, forKey: .restore) ?? restore
        reason = try fields.decodeIfPresent(String.self, forKey: .reason) ?? reason
        latencyMillis = try fields.decodeIfPresent(Int.self, forKey: .latencyMillis)
            ?? latencyMillis
    }

    public var state: ScriptedStoreState {
        ScriptedStoreState(
            plans: selling ? ScriptedStoreState.offered : [],
            purchase: outcome(purchase), restore: outcome(restore),
            latency: .milliseconds(latencyMillis))
    }

    private func outcome(_ said: String) -> ScriptedStoreState.Outcome {
        switch said {
        case "pending": .pending
        case "cancelled": .cancelled
        case "nothingToRestore": .nothingToRestore
        case "fails": .fails(reason)
        default: .bought
        }
    }
}
