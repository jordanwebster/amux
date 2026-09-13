import Foundation

/// Identity of one agent, as the shared core spells it.
public struct AgentId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let uuid: UUID
    public init(_ uuid: UUID) { self.uuid = uuid }
    public init?(_ text: String) {
        guard let uuid = UUID(uuidString: text) else { return nil }
        self.uuid = uuid
    }
    public init(from decoder: any Decoder) throws {
        self.uuid = try decodeUUID(decoder)
    }
    public func encode(to encoder: any Encoder) throws {
        try encodeUUID(uuid, to: encoder)
    }
    public var description: String { uuid.uuidString.lowercased() }
}

/// Identity of one host.
public struct HostId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let uuid: UUID
    public init(_ uuid: UUID) { self.uuid = uuid }
    public init?(_ text: String) {
        guard let uuid = UUID(uuidString: text) else { return nil }
        self.uuid = uuid
    }
    public init(from decoder: any Decoder) throws {
        self.uuid = try decodeUUID(decoder)
    }
    public func encode(to encoder: any Encoder) throws {
        try encodeUUID(uuid, to: encoder)
    }
    public var description: String { uuid.uuidString.lowercased() }
}

/// Identity of one dispatched operation. The bridge answers a dispatch with
/// this before any result arrives, so a caller can await its own outcome.
public struct OpId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let uuid: UUID
    public init(_ uuid: UUID) { self.uuid = uuid }
    public init?(_ text: String) {
        guard let uuid = UUID(uuidString: text) else { return nil }
        self.uuid = uuid
    }
    public init(from decoder: any Decoder) throws {
        self.uuid = try decodeUUID(decoder)
    }
    public func encode(to encoder: any Encoder) throws {
        try encodeUUID(uuid, to: encoder)
    }
    public var description: String { uuid.uuidString.lowercased() }
}

private func decodeUUID(_ decoder: any Decoder) throws -> UUID {
    let text = try decoder.singleValueContainer().decode(String.self)
    guard let uuid = UUID(uuidString: text) else {
        throw DecodingError.dataCorrupted(
            .init(codingPath: decoder.codingPath, debugDescription: "not a UUID: \(text)"))
    }
    return uuid
}

/// The core writes identifiers lowercase; Swift's own spelling is uppercase.
/// Writing them back the way they arrived keeps a round trip byte-identical.
private func encodeUUID(_ uuid: UUID, to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(uuid.uuidString.lowercased())
}

/// What names a stored artifact — a frozen patch, an attachment.
///
/// It is a content digest rather than a UUID ("sha256:…"), so it is carried as
/// the string the core wrote and never reformatted: two spellings of the same
/// digest would name two different artifacts to anyone comparing them.
public struct ArtifactId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let digest: String

    public init(_ digest: String) { self.digest = digest }

    public init(from decoder: any Decoder) throws {
        self.digest = try decoder.singleValueContainer().decode(String.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(digest)
    }

    public var description: String { digest }
}
