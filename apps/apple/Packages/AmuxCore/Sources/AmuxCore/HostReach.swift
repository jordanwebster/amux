import Foundation

/// Where this phone can reach a machine from, which is the only division of
/// the machines worth drawing.
///
/// Not a degree of goodness. A machine on the same network and one on the far
/// side of the relay are both usable and feel nothing alike; a machine the
/// relay can see but this account may not tunnel to is not a slow version of
/// either, because nothing started on it will run; and a machine no route
/// reaches is a fourth thing again. Naming them apart is what lets a screen
/// say something true about each without putting a caveat on every row.
public enum HostReach: Sendable, Equatable {
    case onThisNetwork
    case throughTheRelay
    /// The relay can see it; this phone may not tunnel to it.
    case away
    case offline

    /// The group's own word, for an identifier a capture and a driver can both
    /// name it by.
    public var name: String {
        switch self {
        case .onThisNetwork: "on-this-network"
        case .throughTheRelay: "through-the-relay"
        case .away: "away"
        case .offline: "offline"
        }
    }

    /// The four, in the order a screen lists them: nearest first, then what is
    /// reachable further away, then what is only visible, then what is not
    /// there at all.
    public static let all: [HostReach] = [.onThisNetwork, .throughTheRelay, .away, .offline]

    /// Whether anything started on that machine would run. An away machine and
    /// an offline one are both listed and neither is live.
    public var live: Bool {
        switch self {
        case .onThisNetwork, .throughTheRelay: true
        case .away, .offline: false
        }
    }
}

extension HostEntry {
    /// Which of the four this machine is in, given what the relay will carry
    /// for this account.
    ///
    /// The tier is the link's own, never the account service's: what decides
    /// whether a tunnel opens is the credential the relay holds, and a screen
    /// that grouped by a separately fetched entitlement could promise a
    /// machine the link will refuse. A machine that says it is not signed in
    /// is never "away" — the relay is not seeing it, so nothing about it is a
    /// question of money. It is the same rule `amux peer list` prints.
    public func reach(tier: Tier?) -> HostReach {
        switch via {
        case .direct: .onThisNetwork
        case .relay where tier == .free && signedIn != false: .away
        // A phone holds no SSH links; the arm is here so a machine is never
        // dropped from a screen that claims to list them all.
        case .relay, .ssh: .throughTheRelay
        case .offline: .offline
        }
    }

    /// Whether this machine says it has no account at all.
    ///
    /// The difference between a machine nobody can reach because the
    /// subscription does not carry it and one nobody can reach because it
    /// never signed in — and only one of those is worth asking anybody for
    /// money about.
    public var neverSignedIn: Bool { signedIn == false }
}
