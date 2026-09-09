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
    /// What the account service does with a signed purchase handed to it.
    public var purchase: PurchaseRecording
    public var upload: UploadOutcome
    /// How long every answer takes. Zero is instant.
    public var latency: Duration

    public init(
        signIn: SignInOutcome = .succeeds(Self.ada),
        entitlement: Entitlement = .active(source: .web, renews: nil),
        token: String? = "scripted-connect-token",
        deletion: DeletionOutcome = .deleted,
        purchase: PurchaseRecording = .accepted,
        upload: UploadOutcome = .accepted(id: "report-1"),
        latency: Duration = .zero
    ) {
        self.signIn = signIn
        self.entitlement = entitlement
        self.token = token
        self.deletion = deletion
        self.purchase = purchase
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

    /// What becomes of a purchase the phone posts.
    ///
    /// Accepted by default, so every state written before there was such a
    /// thing still reaches the screen it was written for.
    public enum PurchaseRecording: Codable, Sendable, Equatable {
        case accepted
        /// The account service will not take this transaction, in its words.
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
    case recordPurchase(AccountId)
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
    private var uploads: [ReportBundle] = []
    private var purchases: [String] = []

    public init(state: ScriptedCloudState = ScriptedCloudState()) {
        self.state = state
    }

    public var calls: [CloudCall] { lock.withLock { recorded } }

    /// Every report bundle this was handed, in order, whether it then accepted
    /// it or refused it.
    ///
    /// Kept whole rather than as the names of its parts, because what a driver
    /// asks about a report afterwards is what it declared: which parts are
    /// there, and the reason beside each one that is not. A refused upload is
    /// here too — the retry has to be the same bundle, and only the bundles
    /// themselves can say whether it was.
    public var uploaded: [ReportBundle] { lock.withLock { uploads } }

    /// Every signed transaction this was handed, in order, whether it took it
    /// or refused it. A retry has to be the same transaction, and only these
    /// can say whether it was.
    public var recordedPurchases: [String] { lock.withLock { purchases } }

    public var scripted: ScriptedCloudState {
        get { lock.withLock { state } }
        set { lock.withLock { state = newValue } }
    }

    public func reset() {
        lock.withLock {
            recorded = []
            uploads = []
            purchases = []
        }
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

    public func recordPurchase(
        _ id: AccountId, signedTransaction: String
    ) async throws(CloudError) {
        let state = lock.withLock {
            recorded.append(.recordPurchase(id))
            purchases.append(signedTransaction)
            return self.state
        }
        await wait(state)
        switch state.purchase {
        case .accepted: return
        case .refused(let reason): throw CloudError.refused(reason)
        case .offline: throw CloudError.network("offline")
        }
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
        let state = lock.withLock {
            recorded.append(.uploadReport(id, parts: bundle.parts.map(\.name)))
            uploads.append(bundle)
            return self.state
        }
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

/// What the scripted account service will answer, in the door's own words.
///
/// The double's own Swift shape reaches a wire as Swift's synthesised encoding
/// of nested enums — `{"succeeds":{"_0":…}}` — which nothing outside this
/// language would write and nobody could read in a transcript of a failing
/// run. These are the same outcomes, spelled, and every field has the answer
/// most states want so a driver says only what it is changing.
public struct CloudScript: Codable, Sendable, Equatable {
    /// `succeeds`, `cancelled`, `refused` or `offline`.
    public var signIn = "succeeds"
    /// Who signing in comes back as. The identifier is the app's own and is
    /// what a relay credential is later asked for by name.
    public var account = "ada"
    public var email = "ada@example.com"
    public var displayName: String?
    /// Why the cloud refused, where it refused.
    public var reason = "amux.sh could not sign this account in"
    /// `none`, `active` or `lapsed`.
    public var entitlement = "active"
    /// Where the subscription was bought: `appStore` or `web`.
    public var source = "web"
    /// The relay credential to hand back. Nothing refuses to issue one, which
    /// is what an account with no subscription is answered with.
    public var token: String? = "scripted-connect-token"
    /// `deleted` or `blockedByRenewal`.
    public var deletion = "deleted"
    /// What the account service does with a signed purchase: `accepted`,
    /// `refused` or `network`. Accepted unless a driver says otherwise, so a
    /// journey that is not about paying never has to mention it.
    public var recordPurchase = "accepted"
    /// Why the account service would not take the purchase, where it would
    /// not. Its own field so a driver changes one outcome at a time.
    public var purchaseReason = "amux.sh could not accept that purchase"
    /// What becomes of a report handed over: `accepted`, `refused` or
    /// `offline`.
    public var upload = "accepted"
    /// Why the report was turned down, where it was. Its own field rather than
    /// `reason`, so a driver can leave a refused sign-in scripted and change
    /// only what happens to a report.
    public var uploadReason = "amux.sh could not take this report"
    /// What an accepted report is filed under, which is what the screen shows
    /// the person afterwards.
    public var receipt = "report-1"
    /// Where a blocked deletion says the billing can be stopped.
    public var manageURL = "https://apps.apple.com/account/subscriptions"
    /// How long every answer takes, so a screen that is only on show while a
    /// request is in flight can be reached.
    public var latencyMillis = 0

    public init() {}

    /// Everything unsaid keeps the answer above, so a driver changing one
    /// outcome writes one field.
    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        func said(_ key: CodingKeys, _ fallback: String) throws -> String {
            try fields.decodeIfPresent(String.self, forKey: key) ?? fallback
        }
        signIn = try said(.signIn, signIn)
        account = try said(.account, account)
        email = try said(.email, email)
        displayName = try fields.decodeIfPresent(String.self, forKey: .displayName)
        reason = try said(.reason, reason)
        entitlement = try said(.entitlement, entitlement)
        source = try said(.source, source)
        // Present and null is a cloud that will not issue a credential;
        // absent is a cloud nobody asked about.
        token = fields.contains(.token)
            ? try fields.decodeIfPresent(String.self, forKey: .token) : token
        deletion = try said(.deletion, deletion)
        recordPurchase = try said(.recordPurchase, recordPurchase)
        purchaseReason = try said(.purchaseReason, purchaseReason)
        manageURL = try said(.manageURL, manageURL)
        upload = try said(.upload, upload)
        uploadReason = try said(.uploadReason, uploadReason)
        receipt = try said(.receipt, receipt)
        latencyMillis = try fields.decodeIfPresent(Int.self, forKey: .latencyMillis)
            ?? latencyMillis
    }

    /// The state the double reads, which is the one thing this describes.
    public var state: ScriptedCloudState {
        ScriptedCloudState(
            signIn: outcome, entitlement: entitled, token: token, deletion: deleting,
            purchase: recording, upload: uploading, latency: .milliseconds(latencyMillis))
    }

    private var who: SignedInAccount {
        SignedInAccount(id: AccountId(account), email: email, displayName: displayName)
    }

    private var outcome: ScriptedCloudState.SignInOutcome {
        switch signIn {
        case "cancelled": .cancelled
        case "refused": .refused(reason)
        case "offline": .offline
        default: .succeeds(who)
        }
    }

    private var bought: EntitlementSource {
        source == "appStore" ? .appStore : .web
    }

    private var entitled: Entitlement {
        switch entitlement {
        case "none": .none
        // A subscription that ran out says when, because a screen that only
        // said "ended" would be telling somebody less than they knew.
        case "lapsed": .lapsed(source: bought, endedAt: Scenario.now.addingTimeInterval(-86_400))
        default: .active(source: bought, renews: nil)
        }
    }

    private var recording: ScriptedCloudState.PurchaseRecording {
        switch recordPurchase {
        case "refused": .refused(purchaseReason)
        case "network": .offline
        default: .accepted
        }
    }

    private var uploading: ScriptedCloudState.UploadOutcome {
        switch upload {
        case "refused": .refused(uploadReason)
        case "offline": .offline
        default: .accepted(id: receipt)
        }
    }

    private var deleting: DeletionOutcome {
        guard deletion == "blockedByRenewal", let url = URL(string: manageURL) else {
            return .deleted
        }
        return .blockedByRenewal(source: bought, manageURL: url)
    }
}
