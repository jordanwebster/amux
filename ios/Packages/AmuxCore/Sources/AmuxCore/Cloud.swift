import Foundation

/// Everything this app asks of the cloud.
///
/// One protocol, so a screen never sees HTTP and a test never needs a server:
/// the production adapter and the scripted double are the same shape, and a
/// screen cannot tell which one it is holding.
public protocol CloudService: Sendable {
    func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement
    func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken
    func requestDeletion(_ id: AccountId, confirmedEmail: String) async throws(CloudError) -> DeletionOutcome
    func uploadReport(_ id: AccountId, bundle: ReportBundle) async throws(CloudError) -> ReportReceipt
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
public enum Entitlement: Sendable, Equatable, Codable {
    case none
    case active(source: EntitlementSource, renews: Date?)
    case lapsed(source: EntitlementSource, endedAt: Date)
}

public enum EntitlementSource: String, Sendable, Equatable, Codable {
    case appStore
    case web
}

/// A relay credential. The bridge asks for one when it needs it and the app
/// answers; nothing caches it beyond its expiry.
public struct ConnectToken: Sendable, Equatable, Codable {
    public var bearer: String
    public var expiresAt: Date?

    public init(bearer: String, expiresAt: Date? = nil) {
        self.bearer = bearer
        self.expiresAt = expiresAt
    }
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
    public var note: String
    public var parts: [ReportPart]

    public init(note: String, parts: [ReportPart]) {
        self.note = note
        self.parts = parts
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
