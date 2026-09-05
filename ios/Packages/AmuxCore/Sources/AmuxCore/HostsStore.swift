import Foundation
import Observation

/// The Hosts tab's state: the machines this phone can reach and what is known
/// about why it cannot reach the rest.
@MainActor
@Observable
public final class HostsStore {
    public private(set) var hosts: [HostEntry] = []
    public private(set) var connection = ConnectionUpdate(state: .connecting)

    public init() {}

    public var online: [HostEntry] { hosts.filter(\.online) }
    public var offline: [HostEntry] { hosts.filter { !$0.online } }

    public func apply(_ event: Event) {
        switch event {
        case .fleet(let fleet):
            // Online first, then by name: a machine you can use outranks one
            // you cannot, and beyond that the order is the user's alphabet.
            hosts = fleet.hosts.map(\.entry).sorted {
                ($0.online ? 0 : 1, $0.name.lowercased()) < ($1.online ? 0 : 1, $1.name.lowercased())
            }
        case .connection(let update):
            connection = update
        case .feed, .session, .opResult, .diff, .tokenRequest, .invariant:
            break
        }
    }

    public func host(_ id: HostId) -> HostEntry? { hosts.first { $0.id == id } }
}
