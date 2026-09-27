import Foundation
import Observation

/// One account this phone has signed in to.
public struct AccountEntry: Sendable, Equatable, Identifiable, Codable {
    public var account: SignedInAccount
    /// Signing out keeps the account listed with Sign In beside it: the
    /// address is what a person recognises, and forgetting it would make
    /// signing back in look like adding a stranger.
    public var signedIn: Bool
    public var entitlement: Entitlement
    /// How many machines it had paired when last on screen.
    public var hosts: Int?
    /// How many of its agents needed the person when last on screen.
    public var attention: Int?
    /// The directory under the app's support directory that holds this
    /// account's installation: its key, the machines it trusts, its store.
    public var installation: String

    public var id: AccountId { account.id }

    public init(
        account: SignedInAccount, signedIn: Bool = true, entitlement: Entitlement = .none,
        hosts: Int? = nil, attention: Int? = nil, installation: String = UUID().uuidString
    ) {
        self.account = account
        self.signedIn = signedIn
        self.entitlement = entitlement
        self.hosts = hosts
        self.attention = attention
        self.installation = installation
    }

    /// The second line an account row shows.
    public var line: String {
        if !signedIn { return "Signed out" }
        guard let hosts else { return account.email }
        return hosts == 1 ? "1 host" : "\(hosts) hosts"
    }

    public var name: String {
        account.displayName ?? account.email
    }
}

/// Whether the fleet on screen can reach machines through the relay.
public enum FleetGate: Sendable, Equatable {
    case ready
    case signedOut
    case unsubscribed
}

/// Every account on this phone, which one is on screen, and where each one's
/// installation lives.
///
/// An account's installation is a profile of the shared runtime with its own
/// key, trust and store; only the one on screen runs. A phone nobody has
/// signed in on runs an installation of its own, which the first sign-in
/// takes over — so the machines paired before there was an account are that
/// account's — and a fresh one takes its place if every account is removed.
@MainActor
@Observable
public final class AccountRegistry {
    public private(set) var accounts: [AccountEntry] = []
    public private(set) var selected: AccountId?
    /// The stores of the account on screen.
    public private(set) var stores: StoreBundle?
    /// The installation a phone nobody is signed in on runs.
    public private(set) var signedOutInstallation = UUID().uuidString
    /// Answers for an account that is not on screen, dropped.
    public private(set) var dropped = 0
    /// Told when the account on screen changes, before `changed`.
    @ObservationIgnored public var switching: (@MainActor (AccountId?) -> Void)?
    @ObservationIgnored public var changed: (@MainActor () -> Void)?
    @ObservationIgnored private let file: URL?
    public private(set) var persistenceFailed = false

    private struct Remembered: Codable {
        var accounts: [AccountEntry]
        var selected: AccountId?
        var signedOutInstallation: String?
    }

    public init(file: URL? = nil) {
        self.file = file
        guard let file, let data = try? Data(contentsOf: file),
              let saved = try? AmuxJSON.decoder.decode(Remembered.self, from: data) else {
            persist()
            return
        }
        accounts = saved.accounts
        if let installation = saved.signedOutInstallation { signedOutInstallation = installation }
        selected = saved.selected.flatMap { id in accounts.contains { $0.id == id } ? id : nil }
        stores = selected.map(bundle)
    }

    private func persist() {
        guard let file else { return }
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try AmuxJSON.encoder.encode(Remembered(
                accounts: accounts, selected: selected,
                signedOutInstallation: signedOutInstallation))
                .write(to: file, options: .atomic)
            persistenceFailed = false
        } catch {
            persistenceFailed = true
        }
    }

    public var selectedAccount: AccountEntry? {
        accounts.first { $0.id == selected }
    }

    /// The installation that runs now: the selected account's, or the
    /// signed-out phone's.
    public var installation: String {
        selectedAccount?.installation ?? signedOutInstallation
    }

    /// What the relay will carry for the account on screen: the link's own
    /// word first, then what the account service last said.
    public var gate: FleetGate {
        guard let entry = selectedAccount, entry.signedIn else { return .signedOut }
        switch stores?.hosts.account?.pro {
        case true?: return .ready
        case false?: return .unsubscribed
        case nil:
            if case .active = entry.entitlement { return .ready }
            return .unsubscribed
        }
    }

    /// Where a sign-in for `account` binds: its own installation when it is
    /// already here, the signed-out phone's when nobody is on screen — the
    /// first sign-in adopts what was paired before — or a new one.
    public func installation(for account: AccountId) -> String {
        if let known = accounts.first(where: { $0.id == account }) { return known.installation }
        if selected == nil { return signedOutInstallation }
        return UUID().uuidString
    }

    /// Keeps a signed-in account and puts it on screen.
    ///
    /// A new account goes on screen at once: its installation has to run to
    /// be bound, and whoever added it wants to see it.
    public func add(
        _ account: SignedInAccount, entitlement: Entitlement = .none, installation: String
    ) {
        if let index = accounts.firstIndex(where: { $0.id == account.id }) {
            accounts[index].account = account
            accounts[index].signedIn = true
            accounts[index].entitlement = entitlement
        } else {
            if installation == signedOutInstallation {
                signedOutInstallation = UUID().uuidString
            }
            accounts.append(AccountEntry(
                account: account, entitlement: entitlement, installation: installation))
        }
        if selected != account.id {
            select(account.id)
        } else {
            persist()
            changed?()
        }
    }

    /// Leaves an account; its installation keeps running on screen, signed
    /// out, and still reaches the machines on this network.
    public func signOut(_ id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return }
        accounts[index].signedIn = false
        accounts[index].entitlement = .none
        persist()
        changed?()
    }

    /// Takes an account off this phone and answers the installation to
    /// delete. The screen moves to the first signed-in account, then any
    /// account, then the signed-out phone.
    @discardableResult
    public func forget(_ id: AccountId) -> String? {
        guard let entry = accounts.first(where: { $0.id == id }) else { return nil }
        accounts.removeAll { $0.id == id }
        if selected == id {
            let next = accounts.first(where: \.signedIn)?.id ?? accounts.first?.id
            place(next)
        } else {
            persist()
            changed?()
        }
        return entry.installation
    }

    public func select(_ id: AccountId) {
        guard accounts.contains(where: { $0.id == id }), selected != id else { return }
        place(id)
    }

    private func place(_ id: AccountId?) {
        selected = id
        stores = id.map(bundle)
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

    /// The stores for an account going on screen, which report what it
    /// lists so its row can say so once it is off screen.
    private func bundle(_ id: AccountId) -> StoreBundle {
        let stores = StoreBundle(account: id)
        stores.saw = { [weak self] hosts, attention in
            self?.saw(hosts: hosts, attention: attention, for: id)
        }
        return stores
    }

    /// What the account on screen lists, remembered for when it is not.
    public func saw(hosts: Int, attention: Int, for id: AccountId) {
        guard let index = accounts.firstIndex(where: { $0.id == id }),
              accounts[index].hosts != hosts || accounts[index].attention != attention
        else { return }
        accounts[index].hosts = hosts
        accounts[index].attention = attention
        persist()
    }

    /// A late answer is kept only for the account still on screen.
    public func accept<Value>(_ value: Value, for account: AccountId) -> Value? {
        guard account == selected else {
            dropped += 1
            return nil
        }
        return value
    }
}
