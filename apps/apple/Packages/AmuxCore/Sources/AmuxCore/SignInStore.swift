import Foundation
import Observation

/// The one sign-in this phone has in flight, and how it went.
///
/// It is not part of an account's stores, because there is no account yet:
/// signing in is what makes one. It sits beside the registry a finished
/// sign-in is added to.
@MainActor
@Observable
public final class SignInStore {
    public enum Phase: Sendable, Equatable {
        /// Nothing attempted, or an attempt somebody came back from without
        /// finishing.
        case ready
        /// The browser is up. Everything from here happens on amux.sh.
        case handingOff
        /// It came back refused, in the cloud's own words.
        case failed(String)
        case signedIn(SignedInAccount)
        /// Asked to sign back into one account, and somebody else came back —
        /// the browser was signed in as another account, or the person chose
        /// one. Nothing is added until they say which they meant.
        case mismatched(wanted: SignedInAccount, got: SignedInAccount)
    }

    public private(set) var phase: Phase = .ready
    /// Which account this sign-in is for. Set when the page is opened, from
    /// whatever opened it, and read when the browser is.
    public private(set) var intent: SignInIntent
    /// Where this screen says it is sending you. Read from the endpoint the
    /// hand-off actually opens, so the promise and the URL are one fact.
    public let host: String

    public init(
        host: String = CloudEndpoint.production.host, phase: Phase = .ready,
        intent: SignInIntent = .adding
    ) {
        self.host = host
        self.phase = phase
        self.intent = intent
    }

    public var working: Bool { phase == .handingOff }

    /// Signs in, and says what came back.
    ///
    /// Cancelling is not a failure and leaves nothing on the screen to dismiss:
    /// coming back from the browser without finishing is a person changing
    /// their mind, and an error banner over it would be the app arguing. Every
    /// other refusal is said, because there is nothing the person can do about
    /// it until they know what it was.
    @discardableResult
    public func signIn(
        with cloud: any CloudService,
        presenting: any WebAuthPresenter,
        into registry: AccountRegistry? = nil
    ) async -> SignedInAccount? {
        guard phase != .handingOff else { return nil }
        phase = .handingOff
        do {
            let account = try await cloud.signIn(intent, presenting: presenting)
            // Somebody other than the account this was for. Adding them now
            // would put an account on the phone nobody asked for, under a row
            // the person pressed for a different one.
            if case .returning(let wanted) = intent, wanted.id != account.id {
                phase = .mismatched(wanted: wanted, got: account)
                return nil
            }
            phase = .signedIn(account)
            // What the account is allowed to do decides which gate the home
            // screen draws, so it is asked for here rather than left for the
            // first screen that wonders. A cloud that will not say is not a
            // failed sign-in: the account exists and the gate stays closed
            // until it answers.
            let entitlement = try? await cloud.entitlement(account.id)
            registry?.add(account, entitlement: entitlement ?? .none)
            return account
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    static func phase(after error: CloudError) -> Phase {
        switch error {
        case .cancelled: .ready
        case .unauthenticated: .failed("amux.sh did not recognise this sign-in")
        case .refused(let reason), .keychain(let reason, _): .failed(reason)
        case .network(let what): .failed(what)
        case .timeout: .failed("amux.sh did not answer")
        }
    }

    /// Opens the page for one sign-in, asking afresh.
    ///
    /// What came back last time was about whoever signed in then, and leaving
    /// it on screen would offer somebody Done for an account they are not
    /// signing in as. A browser still up is the exception: that attempt is
    /// this page's and is still running.
    public func begin(_ intent: SignInIntent) {
        guard !working else { return }
        self.intent = intent
        phase = .ready
    }

    /// Puts the screen back where it started, for somebody who wants to try
    /// again rather than read what went wrong a second time.
    public func again() {
        phase = .ready
    }

    /// Keeps the account that came back instead of the one that was asked for.
    @discardableResult
    public func keep(with cloud: any CloudService, into registry: AccountRegistry?) async
        -> SignedInAccount?
    {
        guard case .mismatched(_, let got) = phase else { return nil }
        phase = .signedIn(got)
        let entitlement = try? await cloud.entitlement(got.id)
        registry?.add(got, entitlement: entitlement ?? .none)
        return got
    }

    /// Turns down the account that came back.
    ///
    /// The sign-in still left a session behind for it, and a session this
    /// phone keeps for an account it does not list is one nobody can see or
    /// sign out of — so it is let go. Unless that account is already signed
    /// in here: then the session is the one it was using, now fresher, and
    /// letting go of it would sign somebody out who asked for nothing.
    public func discard(with cloud: any CloudService, from registry: AccountRegistry?) async {
        guard case .mismatched(_, let got) = phase else { return }
        phase = .ready
        let inUse = registry?.accounts.contains { $0.id == got.id && $0.signedIn } ?? false
        if !inUse { try? await cloud.forgetSession(got.id) }
    }
}
