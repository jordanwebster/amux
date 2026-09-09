import Foundation
import Observation

/// One account this phone knows about.
///
/// A signed-out account stays listed. Forgetting it the moment its token
/// expires would lose the only thing the user recognises — their own address —
/// and make signing back in look like adding a stranger.
public struct AccountEntry: Sendable, Equatable, Identifiable, Codable {
    public var account: SignedInAccount
    public var signedIn: Bool
    public var entitlement: Entitlement
    /// How many machines this account reaches, where this phone knows. Only
    /// the account on screen has a connection behind it, so the others are
    /// unknown rather than zero: writing zero would say an account has no
    /// hosts when the truth is that nobody has asked.
    public var hosts: Int?
    /// How many agents on this account are waiting for you.
    ///
    /// Absent until something actually subscribes to that account's fleet in
    /// the background. A phone with one connection cannot see another
    /// account's agents, and a number this phone cannot see is not a number it
    /// may invent — an inactive account with a fabricated "1" beside it would
    /// send somebody to look at nothing.
    public var attention: Int?

    public var id: AccountId { account.id }

    public init(
        account: SignedInAccount, signedIn: Bool = true, entitlement: Entitlement = .none,
        hosts: Int? = nil, attention: Int? = nil
    ) {
        self.account = account
        self.signedIn = signedIn
        self.entitlement = entitlement
        self.hosts = hosts
        self.attention = attention
    }

    /// What this account's row says under its name.
    public var line: String {
        if !signedIn { return "Signed out" }
        guard let hosts else { return account.email }
        return hosts == 1 ? "1 host" : "\(hosts) hosts"
    }

    /// What the person is called, falling back to the address when the account
    /// service gave no name.
    public var name: String {
        account.displayName ?? account.email
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
    /// What to tell whoever holds the runtime when the account on screen
    /// changes.
    ///
    /// The registry is what a screen presses; the runtime is somewhere else
    /// entirely, and a phone that changed the list without re-pointing the
    /// connection would draw one account's name over another account's
    /// machines. Nothing is assumed about how long that takes: the switch here
    /// is immediate, and results still arriving for the account just left are
    /// refused by `deliver` as the late answers they are.
    public var switching: (@MainActor (AccountId) -> Void)?

    /// Connection ownership follows sign-in and removal as well as selection.
    public var changed: (@MainActor () -> Void)?
    @ObservationIgnored private let file: URL?
    public private(set) var persistenceFailed = false

    private struct Remembered: Codable {
        var accounts: [AccountEntry]
        var selected: AccountId?
    }

    public init(file: URL? = nil) {
        self.file = file
        guard let file, let data = try? Data(contentsOf: file),
              let saved = try? AmuxJSON.decoder.decode(Remembered.self, from: data) else { return }
        accounts = saved.accounts
        selected = saved.selected.flatMap { id in accounts.contains { $0.id == id } ? id : nil }
        if let selected, accounts.contains(where: { $0.id == selected && $0.signedIn }) {
            stores = StoreBundle(account: selected)
        }
    }

    private func persist() {
        guard let file else { return }
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try AmuxJSON.encoder.encode(Remembered(accounts: accounts, selected: selected))
                .write(to: file, options: .atomic)
            persistenceFailed = false
        } catch {
            persistenceFailed = true
        }
    }

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
        if selected == nil {
            select(account.id)
        } else {
            if selected == account.id, stores == nil { stores = StoreBundle(account: account.id) }
            persist()
            changed?()
        }
    }

    /// Signing out keeps the account listed with Sign In beside it, and takes
    /// its stores down: nothing of a signed-out account stays on screen.
    public func signOut(_ id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return }
        accounts[index].signedIn = false
        accounts[index].entitlement = .none
        if selected == id { stores = nil }
        persist()
        changed?()
    }

    public func forget(_ id: AccountId) {
        accounts.removeAll { $0.id == id }
        if selected == id {
            selected = accounts.first?.id
            stores = selectedAccount?.signedIn == true ? selected.map { StoreBundle(account: $0) } : nil
        }
        persist()
        changed?()
    }

    /// Puts a whole set of accounts back, as a launch that remembered them or
    /// a declared state has them.
    ///
    /// Apart from `add` because it carries everything an entry knows —
    /// whether it is signed in, how many machines it reached, what is waiting
    /// on it — and `add` deliberately does not: adding an account is the end
    /// of a sign-in, and a sign-in knows none of that yet.
    public func restore(_ entries: [AccountEntry], selected wanted: AccountId? = nil) {
        accounts = entries
        let chosen = wanted ?? entries.first(where: \.signedIn)?.id ?? entries.first?.id
        selected = chosen
        stores = selectedAccount?.signedIn == true ? chosen.map { StoreBundle(account: $0) } : nil
        persist()
        changed?()
    }

    public func select(_ id: AccountId) {
        guard accounts.contains(where: { $0.id == id }) else { return }
        guard selected != id else { return }
        selected = id
        stores = selectedAccount?.signedIn == true ? StoreBundle(account: id) : nil
        persist()
        switching?(id)
        changed?()
    }

    public func entitlement(_ entitlement: Entitlement, for id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }),
              accounts[index].entitlement != entitlement else { return }
        accounts[index].entitlement = entitlement
        persist()
        changed?()
    }

    /// How many agents on an account that is not on screen are waiting.
    ///
    /// Written from that account's own live subscription, which the runtime
    /// keeps folding while the app is in front of somebody. Nothing here
    /// counts or guesses: an account that has reported nothing says nothing in
    /// the switcher, which is the truth.
    public func attention(_ count: Int?, for id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return }
        accounts[index].attention = count
    }

    /// Apply a batch that answers for one account. Returns whether it landed.
    @discardableResult
    public func deliver(_ batch: [Event], for account: AccountId) -> Bool {
        // What an account nobody is looking at has waiting arrives on the
        // selected account's stream naming its own account, so it is credited
        // to the account it names rather than to the one that carried it.
        for event in batch {
            guard case .attention(let named, let waiting) = event else { continue }
            attention(waiting, for: AccountId(named))
        }
        guard account == selected, let stores, stores.account == account else {
            dropped += 1
            return false
        }
        stores.apply(batch)
        // How many machines this account reaches, from the connection that
        // just answered. The switcher reads it, and it is a fact rather than
        // a guess exactly because it came from here.
        if let index = accounts.firstIndex(where: { $0.id == account }) {
            accounts[index].hosts = stores.hosts.hosts.count
        }
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
