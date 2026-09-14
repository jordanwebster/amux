import Foundation
import Observation

/// The Hosts tab's state: the machines this phone can reach and what is known
/// about why it cannot reach the rest.
@MainActor
@Observable
public final class HostsStore {
    public private(set) var hosts: [HostEntry] = []
    /// Machines on the relay this phone has not paired with.
    ///
    /// Kept apart from `hosts` rather than mixed in with a flag, because they
    /// are not worse hosts: nothing of theirs is readable and nothing can be
    /// started on them. What they are for is pairing — a six-digit code is
    /// authenticated against one machine, and this is where the screen that
    /// takes the digits learns which.
    public private(set) var discovered: [HostEntry] = []
    public private(set) var connection = ConnectionUpdate(state: .connecting)
    /// Whether anybody is signed in, what the link is doing and what the
    /// account buys, as the core reports it.
    ///
    /// Read here rather than asked of the account service: this is the state
    /// the link is actually in, and a screen that decides what to offer from
    /// a separate question could offer something the link cannot do.
    public private(set) var cloud: CloudState = .signedOut
    /// This phone and the machines it holds keys for.
    ///
    /// Nothing until the runtime has read its own trust store, and never
    /// emptied by a failed read: a section that blanked itself would invite
    /// pairing again with everything still paired.
    public private(set) var roster: DeviceRoster?
    /// Whether the system is letting this app look at the network this phone
    /// is on.
    ///
    /// A person who refused is not a person on an empty network, and the two
    /// look identical from here: no machines either way. Only this tells them
    /// apart, so only this can decide whether the screen says there is nothing
    /// nearby or says why it cannot look.
    public private(set) var localNetwork: LocalNetworkPermission = .unknown

    /// Whether the paired devices are being read rather than counted.
    ///
    /// A screen state rather than a route, because the list is the same
    /// machines the screen behind it is already about — going somewhere else
    /// to read a fingerprint would lose them.
    public var readingDevices = false

    /// The moment this phone saw a machine stop being reachable.
    ///
    /// Observed, never inferred. Nothing in the inventory says when a host
    /// went away — presence is a boolean derived from routing — so the only
    /// honest answer is the moment this phone watched it change, and a host
    /// that was already offline the first time the phone heard of it has no
    /// answer at all.
    private var departures: [HostId: Date] = [:]
    private let clock: @MainActor () -> Date

    public init(clock: @escaping @MainActor () -> Date = { Date() }) {
        self.clock = clock
    }

    /// What the clock this store reads says now. A screen that puts an age on
    /// a row asks the store rather than the system, so a pinned clock pins the
    /// whole picture.
    public var now: Date { clock() }

    public var online: [HostEntry] { hosts.filter(\.online) }
    public var offline: [HostEntry] { hosts.filter { !$0.online } }

    /// Where this phone can reach a machine from, which is the only division
    /// of the list worth drawing.
    ///
    /// Not a degree of goodness. A machine on the same network and one on the
    /// far side of the relay are both usable and feel nothing alike; a machine
    /// the relay can see but this account may not tunnel to is not a slow
    /// version of either, because nothing started on it will run; and a
    /// machine no route reaches is a fourth thing again. Naming them apart is
    /// what lets the screen say something true about each without putting a
    /// caveat on every row.
    public enum Reach: Sendable, Equatable {
        case onThisNetwork
        case throughTheRelay
        /// The relay can see it; this phone may not tunnel to it.
        case away
        case offline

        /// The group's own word, for an identifier a capture and a driver can
        /// both name it by.
        public var name: String {
            switch self {
            case .onThisNetwork: "on-this-network"
            case .throughTheRelay: "through-the-relay"
            case .away: "away"
            case .offline: "offline"
            }
        }

        /// The four, in the order a screen lists them: nearest first, then
        /// what is reachable further away, then what is only visible, then
        /// what is not there at all.
        public static let all: [Reach] = [.onThisNetwork, .throughTheRelay, .away, .offline]
    }

    /// Which of the four a machine is in, paired or merely offered.
    ///
    /// The tier is asked of the link rather than of the account service: what
    /// decides whether a tunnel opens is the credential the relay holds, and a
    /// screen that grouped by a separately fetched entitlement could promise a
    /// machine the link will refuse. A host that says it is not signed in is
    /// never "away": the relay is not seeing it, so nothing about it is a
    /// question of money.
    public func reach(of host: HostEntry) -> Reach {
        switch host.via {
        case .direct: .onThisNetwork
        case .relay where cloud.tier == .free && host.signedIn != false: .away
        // A phone holds no SSH links; the arm is here so a machine is never
        // dropped from a screen that claims to list them all.
        case .relay, .ssh: .throughTheRelay
        case .offline: .offline
        }
    }

    public func hosts(_ reach: Reach) -> [HostEntry] {
        hosts.filter { self.reach(of: $0) == reach }
    }

    /// The machines this phone has not paired with, in the same four groups.
    /// A browser answers with machines on this network and the relay answers
    /// with the rest, so an offer belongs where a paired machine on the same
    /// route belongs.
    public func candidates(_ reach: Reach) -> [HostEntry] {
        discovered.filter { self.reach(of: $0) == reach }
    }

    /// Machines this phone can see advertising themselves on this network and
    /// cannot open a link to.
    ///
    /// Two facts at once, and both are known: the browser resolved the
    /// advertisement, and no route stands. It is worth saying because the
    /// cause is almost never the machine — a network that passes Bonjour and
    /// blocks the transport looks exactly like this — and a machine that is
    /// simply switched off looks nothing like it.
    public var foundButUnreachable: Set<HostId> {
        let seen = Set(discovered.map(\.id))
        return Set(hosts.filter { reach(of: $0) == .offline && seen.contains($0.id) }.map(\.id))
    }

    /// When this phone saw that host go, or nothing where it never saw it.
    public func wentOffline(_ id: HostId) -> Date? { departures[id] }

    public func apply(_ event: Event) {
        switch event {
        case .fleet(let fleet):
            let known = Dictionary(uniqueKeysWithValues: hosts.map { ($0.id, $0.online) })
            let entries = fleet.hosts.map(\.entry)
            for entry in entries {
                if entry.online {
                    departures[entry.id] = nil
                } else if known[entry.id] == true {
                    departures[entry.id] = clock()
                }
            }
            departures = departures.filter { id, _ in entries.contains { $0.id == id } }
            // Online first, then by name: a machine you can use outranks one
            // you cannot, and beyond that the order is the user's alphabet.
            hosts = entries.sorted {
                ($0.online ? 0 : 1, $0.name.lowercased()) < ($1.online ? 0 : 1, $1.name.lowercased())
            }
        case .discovered(let offers):
            discovered = offers.sorted { $0.name.lowercased() < $1.name.lowercased() }
        case .connection(let update):
            connection = update
        case .devices(let roster):
            self.roster = roster
        case .cloudState(let state):
            cloud = state
        case .feed, .session, .opResult, .diff, .tokenRequest, .invariant, .attention:
            break
        }
    }

    /// The machines this phone trusts, or nothing where the trust store has
    /// not been read yet. Not derived from the fleet: a machine that is away
    /// is still trusted, and the fingerprint a person compares before revoking
    /// one is not something the inventory carries.
    public var devices: [PairedDevice] { roster?.devices ?? [] }

    /// What the system said about looking at this network.
    public func sawLocalNetwork(_ permission: LocalNetworkPermission) {
        localNetwork = permission
    }

    /// Open and close the paired devices.
    public func readDevices() { readingDevices = true }
    public func stopReadingDevices() { readingDevices = false }

    public func host(_ id: HostId) -> HostEntry? { hosts.first { $0.id == id } }

    /// A machine by id, paired or merely offered. The pairing screens ask this
    /// one, because the machine they are about to trust is by definition not
    /// in the fleet yet.
    public func known(_ id: HostId) -> HostEntry? {
        hosts.first { $0.id == id } ?? discovered.first { $0.id == id }
    }
}
