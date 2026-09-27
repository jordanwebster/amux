import AmuxValues
import Foundation

/// How this phone reaches a host, as the hosts screen groups them.
public enum HostReach: Sendable, Equatable {
    case onThisNetwork
    case throughTheRelay
    /// Listed, and not reachable from here.
    case away
    case offline

    public var name: String {
        switch self {
        case .onThisNetwork: "on-this-network"
        case .throughTheRelay: "through-the-relay"
        case .away: "away"
        case .offline: "offline"
        }
    }

    public static let all: [HostReach] = [.onThisNetwork, .throughTheRelay, .away, .offline]

    public var live: Bool {
        switch self {
        case .onThisNetwork, .throughTheRelay: true
        case .away, .offline: false
        }
    }
}

extension HostView: Identifiable {
    public var id: HostId? { HostId(bytes: hostId) }

    public var online: Bool { presence == .online }

    /// Where the runtime says this host is.
    ///
    /// A candidate is found on this network by definition. A trusted host is
    /// grouped by the route its live link runs over; one with no link is
    /// away when the runtime says it is and offline otherwise.
    public var reach: HostReach {
        if candidate { return .onThisNetwork }
        switch presence {
        case .online:
            return via == .direct ? .onThisNetwork : .throughTheRelay
        case .away:
            return .away
        case .offline, .unspecified:
            return .offline
        }
    }
}
