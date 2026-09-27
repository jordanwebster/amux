import AmuxValues
import Foundation

/// A machine, as every host names it: a UUID, carried across the bridge as
/// its sixteen bytes.
public struct HostId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let uuid: UUID

    public init(_ uuid: UUID) { self.uuid = uuid }

    public init?(_ text: String) {
        guard let uuid = UUID(uuidString: text) else { return nil }
        self.uuid = uuid
    }

    /// The bytes the bridge carries; nil for anything that is not sixteen.
    public init?(bytes: [UInt8]) {
        guard bytes.count == 16 else { return nil }
        uuid = UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
    }

    public var bytes: [UInt8] {
        withUnsafeBytes(of: uuid.uuid) { Array($0) }
    }

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let uuid = UUID(uuidString: text) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "not a UUID: \(text)"))
        }
        self.uuid = uuid
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(description)
    }

    public var description: String { uuid.uuidString.lowercased() }
}

extension AgentKey: CustomStringConvertible {
    public init(host: HostId, agent: UUID) {
        self.init(host: host.bytes, agent: withUnsafeBytes(of: agent.uuid) { Array($0) })
    }

    /// The agent's host.
    public var hostId: HostId? { HostId(bytes: host) }

    /// The agent's own id, as a UUID where it is one.
    public var agentUUID: UUID? { HostId(bytes: agent)?.uuid }

    public var description: String {
        agentUUID?.uuidString.lowercased() ?? agent.map { String(format: "%02x", $0) }.joined()
    }
}
