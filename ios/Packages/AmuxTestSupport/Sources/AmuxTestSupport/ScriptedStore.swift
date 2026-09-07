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
}

/// The App Store, scripted. It answers exactly what the state says and records
/// what it was asked.
public final class ScriptedStoreFront: StoreFront, @unchecked Sendable {
    private let lock = NSLock()
    private var state: ScriptedStoreState
    private var recorded: [StoreCall] = []

    public init(state: ScriptedStoreState = ScriptedStoreState()) {
        self.state = state
    }

    public var calls: [StoreCall] { lock.withLock { recorded } }

    public var scripted: ScriptedStoreState {
        get { lock.withLock { state } }
        set { lock.withLock { state = newValue } }
    }

    public func reset() {
        lock.withLock { recorded = [] }
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
        return try Self.outcome(state.purchase)
    }

    public func restore() async throws(StoreError) -> PurchaseOutcome {
        let state = record(.restore)
        await wait(state)
        return try Self.outcome(state.restore)
    }

    private static func outcome(
        _ declared: ScriptedStoreState.Outcome
    ) throws(StoreError) -> PurchaseOutcome {
        switch declared {
        case .bought: .bought
        case .pending: .pending
        case .cancelled: .cancelled
        case .nothingToRestore: .nothingToRestore
        case .fails(let reason): throw StoreError.failed(reason)
        }
    }
}
