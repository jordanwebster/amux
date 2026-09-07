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
        case .feed, .session, .opResult, .diff, .tokenRequest, .invariant:
            break
        }
    }

    public func host(_ id: HostId) -> HostEntry? { hosts.first { $0.id == id } }

    /// A machine by id, paired or merely offered. The pairing screens ask this
    /// one, because the machine they are about to trust is by definition not
    /// in the fleet yet.
    public func known(_ id: HostId) -> HostEntry? {
        hosts.first { $0.id == id } ?? discovered.first { $0.id == id }
    }
}
