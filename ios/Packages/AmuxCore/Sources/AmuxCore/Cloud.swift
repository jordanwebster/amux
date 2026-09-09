import Foundation

/// Everything this app asks of the cloud.
///
/// One protocol, so a screen never sees HTTP and a test never needs a server:
/// the production adapter and the scripted double are the same shape, and a
/// screen cannot tell which one it is holding.
public protocol CloudService: Sendable {
    /// Which account service this is, as an origin.
    ///
    /// The runtime needs it to judge a pairing invitation: a machine's
    /// invitation names the service that machine's account is on, and this
    /// phone pairs only with machines on the same one. It belongs here
    /// because this is the object that actually reached that service.
    var service: URL { get }
    func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement
    func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken
    /// Hands over a purchase the App Store signed, so the subscription it paid
    /// for becomes this account's.
    ///
    /// The app carries the signed transaction and nothing else: what the cloud
    /// then does with it — which billing system it is reconciled against, how
    /// a renewal is watched — is the cloud's business, and an app that knew
    /// would be a second place for that to change.
    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError)
    func requestDeletion(_ id: AccountId, confirmedEmail: String) async throws(CloudError) -> DeletionOutcome
    func uploadReport(_ id: AccountId, bundle: ReportBundle) async throws(CloudError) -> ReportReceipt
}

public extension CloudService {
    /// A double that says nothing about where it is stands for the real one.
    var service: URL { CloudEndpoint.production.base }
}

/// Sign-in happens on the web, in a browser the app does not own and cannot
/// read. This is the app's whole part in it: hand over a URL and be told what
/// came back.
public protocol WebAuthPresenter: Sendable {
    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL
}

public enum CloudError: Error, Sendable, Equatable {
    case cancelled
    case unauthenticated
    case network(String)
    case refused(String)
    case timeout
}

public struct AccountId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let value: String
    public init(_ value: String) { self.value = value }
    public init(from decoder: any Decoder) throws {
        value = try decoder.singleValueContainer().decode(String.self)
    }
    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(value)
    }
    public var description: String { value }
}

public struct SignedInAccount: Sendable, Equatable, Codable {
    public var id: AccountId
    public var email: String
    public var displayName: String?

    public init(id: AccountId, email: String, displayName: String? = nil) {
        self.id = id
        self.email = email
        self.displayName = displayName
    }
}

public struct AccountFacts: Sendable, Equatable, Codable {
    public var id: AccountId
    public var email: String
    public var displayName: String?
    public var entitlement: Entitlement

    public init(id: AccountId, email: String, displayName: String? = nil, entitlement: Entitlement) {
        self.id = id
        self.email = email
        self.displayName = displayName
        self.entitlement = entitlement
    }
}

/// What this account is allowed to do, and where that came from. A lapsed
/// entitlement says when it ended rather than pretending it never existed.
///
/// The cloud answers one question — may this account act — and this is that
/// answer. It is never inferred from whether a billing record exists: an
/// account can be entitled without anybody ever having paid, and reading the
/// absence of a purchase as the absence of access is the mistake this shape
/// exists to prevent.
public enum Entitlement: Sendable, Equatable, Codable {
    case none
    case active(grant: Grant, renews: Date?)
    case lapsed(grant: Grant, endedAt: Date)
}

/// Why an account has what it has.
///
/// Two cases, not an optional purchase, because an entitlement that was given
/// rather than sold is a state every screen has to say something about — and a
/// screen that treated it as a missing purchase would offer to manage a
/// subscription that does not exist.
public enum Grant: Sendable, Equatable, Codable {
    /// Bought, in the place it was bought.
    case purchased(EntitlementSource)
    /// Given: complimentary, an employee, a beta tester. There is no store
    /// behind it, nothing is being billed, and there is nothing to cancel.
    case granted

    /// Where it was bought, for the screens that only have something to say
    /// about a purchase. Nothing when it was not one.
    public var purchase: EntitlementSource? {
        switch self {
        case .purchased(let source): source
        case .granted: nil
        }
    }
}

public enum EntitlementSource: String, Sendable, Equatable, Codable {
    case appStore
    case web
}

/// A relay credential, and the relay it is good at. The bridge asks for one
/// when it needs it and the app answers; nothing caches it beyond its expiry.
///
/// The address travels with the credential because it is the account service
/// that decides which relay an account reaches — an app holding a relay
/// address of its own would keep dialling one machine after the service had
/// moved the account to another, and the credential names a port and an
/// audience the relay compares with its own configuration.
public struct ConnectToken: Sendable, Equatable, Codable {
    public var bearer: String
    public var host: String
    public var port: Int
    public var expiresAt: Date?

    public init(bearer: String, host: String, port: Int, expiresAt: Date? = nil) {
        self.bearer = bearer
        self.host = host
        self.port = port
        self.expiresAt = expiresAt
    }

    /// Where the relay is, as the runtime is told to reach it. Always TLS: the
    /// account service only ever names a relay on the public internet.
    public var relay: URL? { URL(string: "https://\(host):\(port)") }
}

/// Deletion can be refused while money is still moving, and the refusal has to
/// say where to go and stop it.
public enum DeletionOutcome: Sendable, Equatable, Codable {
    case deleted
    case blockedByRenewal(source: EntitlementSource, manageURL: URL)
}

/// A report as assembled for upload. Every part declares itself present or
/// states why it is missing, so a report with a hole in it is still readable
/// as a report rather than as a bug in the reporter.
public struct ReportBundle: Sendable, Equatable, Codable {
    /// Every part, `report.json` first. What somebody wrote about the report
    /// is inside that file rather than beside it: the header is what declares
    /// the note, the rectangles and which other parts are here at all, and a
    /// note carried separately would be a second place for it to live.
    public var parts: [ReportPart]

    public init(parts: [ReportPart]) {
        self.parts = parts
    }

    /// One part by the name it is filed under.
    public func part(_ name: String) -> ReportPart? {
        parts.first { $0.name == name }
    }
}

public struct ReportPart: Sendable, Equatable, Codable {
    public var name: String
    public var data: Data?
    public var absenceReason: String?

    public init(name: String, data: Data? = nil, absenceReason: String? = nil) {
        self.name = name
        self.data = data
        self.absenceReason = absenceReason
    }

    public var present: Bool { data != nil }
}

public struct ReportReceipt: Sendable, Equatable, Codable {
    public var id: String
    public var receivedAt: Date

    public init(id: String, receivedAt: Date) {
        self.id = id
        self.receivedAt = receivedAt
    }
}
