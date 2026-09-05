import Foundation
import Observation

/// One account this phone knows about.
///
/// A signed-out account stays listed. Forgetting it the moment its token
/// expires would lose the only thing the user recognises — their own address —
/// and make signing back in look like adding a stranger.
public struct AccountEntry: Sendable, Equatable, Identifiable {
    public var account: SignedInAccount
    public var signedIn: Bool
    public var entitlement: Entitlement

    public var id: AccountId { account.id }

    public init(account: SignedInAccount, signedIn: Bool = true, entitlement: Entitlement = .none) {
        self.account = account
        self.signedIn = signedIn
        self.entitlement = entitlement
    }
}

/// Why a fleet may be empty before anything has even been asked for.
public enum FleetGate: Sendable, Equatable {
    /// Signed in and subscribed: an empty fleet means no paired hosts.
    case ready
    case signedOut
    case unsubscribed
}

/// The accounts this phone knows, and which one is on screen.
///
/// Every remote fact is tagged with the account it answers for. A result that
/// arrives after you have switched away is dropped: it answers a question
/// about someone else's fleet, and writing it into the visible stores would
/// show one account's agents under another account's name.
@MainActor
@Observable
public final class AccountRegistry {
    public private(set) var accounts: [AccountEntry] = []
    public private(set) var selected: AccountId?
    /// The selected account's stores. Switching accounts replaces them: the
    /// new account's cache repopulates them from its own connection.
    public private(set) var stores: StoreBundle?
    /// Late results refused because they answered for a deselected account.
    public private(set) var dropped = 0

    public init() {}

    public var selectedAccount: AccountEntry? {
        accounts.first { $0.id == selected }
    }

    /// Whether this phone can reach anything at all.
    ///
    /// Both gates are the same screen with one word changed, because the state
    /// they describe is the same: every host is reached through the relay, the
    /// relay is what an account is, and the subscription is what the relay
    /// accepts. Without either, nothing is reachable and the fleet is empty
    /// for a reason worth saying.
    public var gate: FleetGate {
        guard let entry = selectedAccount, entry.signedIn else { return .signedOut }
        if case .active = entry.entitlement { return .ready }
        return .unsubscribed
    }

    public func add(_ account: SignedInAccount, entitlement: Entitlement = .none) {
        if let index = accounts.firstIndex(where: { $0.id == account.id }) {
            accounts[index].account = account
            accounts[index].signedIn = true
            accounts[index].entitlement = entitlement
        } else {
            accounts.append(AccountEntry(account: account, entitlement: entitlement))
        }
        if selected == nil { select(account.id) }
    }

    /// Signing out keeps the account listed with Sign In beside it, and takes
    /// its stores down: nothing of a signed-out account stays on screen.
    public func signOut(_ id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return }
        accounts[index].signedIn = false
        accounts[index].entitlement = .none
        if selected == id { stores = nil }
    }

    public func forget(_ id: AccountId) {
        accounts.removeAll { $0.id == id }
        if selected == id {
            selected = accounts.first?.id
            stores = selected.map { StoreBundle(account: $0) }
        }
    }

    public func select(_ id: AccountId) {
        guard accounts.contains(where: { $0.id == id }) else { return }
        guard selected != id else { return }
        selected = id
        stores = StoreBundle(account: id)
    }

    public func entitlement(_ entitlement: Entitlement, for id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return }
        accounts[index].entitlement = entitlement
    }

    /// Apply a batch that answers for one account. Returns whether it landed.
    @discardableResult
    public func deliver(_ batch: [Event], for account: AccountId) -> Bool {
        guard account == selected, let stores, stores.account == account else {
            dropped += 1
            return false
        }
        stores.apply(batch)
        return true
    }

    /// The same rule for anything the cloud answers: a value tagged for an
    /// account that is no longer selected is not a value this screen may use.
    public func accept<Value>(_ value: Value, for account: AccountId) -> Value? {
        guard account == selected else {
            dropped += 1
            return nil
        }
        return value
    }
}
