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
    }

    public private(set) var phase: Phase = .ready
    /// Where this screen says it is sending you. Read from the endpoint the
    /// hand-off actually opens, so the promise and the URL are one fact.
    public let host: String

    public init(host: String = CloudEndpoint.production.host, phase: Phase = .ready) {
        self.host = host
        self.phase = phase
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
            let account = try await cloud.signIn(presenting: presenting)
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

    /// Puts the screen back where it started, for somebody who wants to try
    /// again rather than read what went wrong a second time.
    public func again() {
        phase = .ready
    }
}
