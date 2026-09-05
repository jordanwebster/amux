import AmuxCore
import Foundation

/// What the cloud will answer, declared by the test rather than discovered at
/// runtime. Every outcome — including the failures and the waiting — is stated
/// here, so a screen that only appears while a request is in flight is a
/// screen a test can actually reach.
public struct ScriptedCloudState: Codable, Sendable, Equatable {
    public var signIn: SignInOutcome
    public var entitlement: Entitlement
    /// The relay credential to hand back, or nothing to refuse.
    public var token: String?
    public var deletion: DeletionOutcome
    public var upload: UploadOutcome
    /// How long every answer takes. Zero is instant.
    public var latency: Duration

    public init(
        signIn: SignInOutcome = .succeeds(Self.ada),
        entitlement: Entitlement = .active(source: .web, renews: nil),
        token: String? = "scripted-connect-token",
        deletion: DeletionOutcome = .deleted,
        upload: UploadOutcome = .accepted(id: "report-1"),
        latency: Duration = .zero
    ) {
        self.signIn = signIn
        self.entitlement = entitlement
        self.token = token
        self.deletion = deletion
        self.upload = upload
        self.latency = latency
    }

    public static let ada = SignedInAccount(
        id: AccountId("ada"), email: "ada@example.com", displayName: "Ada")

    /// Signed out, with nothing bought.
    public static var firstRun: ScriptedCloudState {
        ScriptedCloudState(signIn: .cancelled, entitlement: .none, token: nil)
    }

    /// Signed in, nothing bought.
    public static var unsubscribed: ScriptedCloudState {
        ScriptedCloudState(entitlement: .none)
    }

    public enum SignInOutcome: Codable, Sendable, Equatable {
        case succeeds(SignedInAccount)
        case cancelled
        case refused(String)
        case offline
    }

    public enum UploadOutcome: Codable, Sendable, Equatable {
        case accepted(id: String)
        case refused(String)
        case offline
    }
}

/// One call a screen made, in the order it made it.
public enum CloudCall: Sendable, Equatable {
    case signIn
    case account(AccountId)
    case entitlement(AccountId)
    case connectToken(AccountId)
    case requestDeletion(AccountId, confirmedEmail: String)
    case uploadReport(AccountId, parts: [String])
}

/// The cloud, scripted. It answers exactly what the state says and records
/// what it was asked, so a test can assert that a screen asked for a token
/// once rather than on every frame.
public final class ScriptedCloudService: CloudService, @unchecked Sendable {
    private let lock = NSLock()
    private var state: ScriptedCloudState
    private var recorded: [CloudCall] = []

    public init(state: ScriptedCloudState = ScriptedCloudState()) {
        self.state = state
    }

    public var calls: [CloudCall] { lock.withLock { recorded } }

    public var scripted: ScriptedCloudState {
        get { lock.withLock { state } }
        set { lock.withLock { state = newValue } }
    }

    public func reset() {
        lock.withLock { recorded = [] }
    }

    private func record(_ call: CloudCall) -> ScriptedCloudState {
        lock.withLock {
            recorded.append(call)
            return state
        }
    }

    private func wait(_ state: ScriptedCloudState) async {
        guard state.latency > .zero else { return }
        try? await Task.sleep(for: state.latency)
    }

    public func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        let state = record(.signIn)
        await wait(state)
        switch state.signIn {
        case .succeeds(let account):
            // The app's whole part in signing in: hand over a URL and be told
            // what came back. It never sees a password.
            _ = try? await presenting.present(
                URL(string: "https://amux.sh/sign-in")!, callbackScheme: "amux")
            return account
        case .cancelled: throw CloudError.cancelled
        case .refused(let reason): throw CloudError.refused(reason)
        case .offline: throw CloudError.network("offline")
        }
    }

    public func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
        let state = record(.account(id))
        await wait(state)
        guard case .succeeds(let account) = state.signIn else { throw CloudError.unauthenticated }
        return AccountFacts(
            id: account.id, email: account.email, displayName: account.displayName,
            entitlement: state.entitlement)
    }

    public func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
        let state = record(.entitlement(id))
        await wait(state)
        return state.entitlement
    }

    public func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken {
        let state = record(.connectToken(id))
        await wait(state)
        guard let token = state.token else { throw CloudError.unauthenticated }
        return ConnectToken(bearer: token, expiresAt: Scenario.now.addingTimeInterval(3600))
    }

    public func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        let state = record(.requestDeletion(id, confirmedEmail: confirmedEmail))
        await wait(state)
        return state.deletion
    }

    public func uploadReport(
        _ id: AccountId, bundle: ReportBundle
    ) async throws(CloudError) -> ReportReceipt {
        let state = record(.uploadReport(id, parts: bundle.parts.map(\.name)))
        await wait(state)
        switch state.upload {
        case .accepted(let receipt):
            return ReportReceipt(id: receipt, receivedAt: Scenario.now)
        case .refused(let reason): throw CloudError.refused(reason)
        case .offline: throw CloudError.network("offline")
        }
    }
}

/// A sign-in presenter that answers with the callback the cloud would have
/// sent, without opening a browser.
public struct ScriptedWebAuth: WebAuthPresenter {
    public var callback: URL
    public var outcome: Outcome

    public enum Outcome: Sendable, Equatable {
        case returns
        case cancelled
    }

    public init(callback: URL = URL(string: "amux://callback?code=scripted")!,
                outcome: Outcome = .returns) {
        self.callback = callback
        self.outcome = outcome
    }

    public func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        switch outcome {
        case .returns: return callback
        case .cancelled: throw CloudError.cancelled
        }
    }
}
