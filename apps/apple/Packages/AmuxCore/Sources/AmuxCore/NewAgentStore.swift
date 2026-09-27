import AmuxValues
import Foundation
import Observation

/// Starting an agent: which host, where on it, what it is called and which
/// agent it is. One screen; every choice is held here and reset each time
/// the screen opens.
@MainActor
@Observable
public final class NewAgentStore {
    public enum Provider: String, Sendable, Equatable, CaseIterable, Identifiable {
        case claude
        case codex

        public var id: String { rawValue }

        public var title: String {
            switch self {
            case .claude: "Claude"
            case .codex: "Codex"
            }
        }

        /// Claude always starts on the SDK driver: the phone renders the chat
        /// vocabulary, and only the SDK driver carries every row of it.
        public var kind: Kind {
            switch self {
            case .claude: .claudeSdk
            case .codex: .codex
            }
        }
    }

    public enum Listing: Equatable, Sendable {
        case none
        case asking
        case ready
        /// The host could not be asked; only the fleet's own directories and
        /// a typed path are offered.
        case unavailable
    }

    /// How many directories a host is asked for; a list that comes back
    /// this long may have more, so a search asks the host again.
    public static let limit: UInt32 = 200

    public private(set) var machine: HostId?
    public private(set) var listing = Listing.none
    public private(set) var recent: [Directory] = []
    public private(set) var repositories: [Directory] = []
    public private(set) var roots: [String] = []
    public private(set) var capped = false
    public private(set) var directory = ""
    public var query = ""
    public var typed = ""
    public var browsing = false
    public private(set) var provider = Provider.claude
    public private(set) var typedName: String?
    public private(set) var starting = false
    public private(set) var failure: String?
    /// The agent the host started, once it confirms.
    public private(set) var created: AgentKey?

    /// Names already taken on each host, so a suggestion is new there.
    @ObservationIgnored private var taken: [HostId: Set<String>] = [:]
    /// Where the fleet's agents on each host work, newest first: what the
    /// chooser offers when the host lists nothing of its own.
    @ObservationIgnored private var worked: [HostId: [Directory]] = [:]
    @ObservationIgnored private var asked = 0

    public init() {}

    public func open(on host: HostId?) {
        machine = host
        listing = .none
        recent = []
        repositories = []
        roots = []
        capped = false
        directory = ""
        query = ""
        typed = ""
        typedName = nil
        browsing = false
        provider = .claude
        starting = false
        failure = nil
        created = nil
        asked += 1
    }

    /// Chooses a host. Everything learned about the last one goes.
    public func point(at host: HostId) {
        guard machine != host else { return }
        machine = host
        listing = .none
        recent = []
        repositories = []
        roots = []
        capped = false
        directory = ""
        query = ""
        failure = nil
        asked += 1
    }

    /// What the fleet lists, for names and directories per host.
    public func remember(_ rows: [AgentRow]) {
        var names: [HostId: Set<String>] = [:]
        var places: [HostId: [Directory]] = [:]
        for row in rows.sorted(by: { $0.lastActivity > $1.lastActivity }) {
            guard let host = row.hostId else { continue }
            names[host, default: []].insert(row.name)
            let path = row.workingDirectory
            guard !path.isEmpty, !(places[host] ?? []).contains(where: { $0.path == path })
            else { continue }
            let name = path.split(separator: "/").last.map(String.init) ?? path
            places[host, default: []].append(Directory(
                path: path, name: name, lastUsedMs: row.card.lastActivityMs))
        }
        taken = names
        worked = places
    }

    /// Marks a listing under way and answers its number.
    public func asking() -> Int {
        asked += 1
        listing = .asking
        return asked
    }

    public func listed(_ result: Result<Directories, RuntimeFailure>, asked number: Int) {
        guard number == asked else { return }
        let fallback = machine.flatMap { worked[$0] } ?? []
        switch result {
        case .success(let directories):
            recent = directories.recent.isEmpty ? fallback : directories.recent
            repositories = directories.repositories
            roots = directories.roots
            capped = UInt32(directories.recent.count + directories.repositories.count)
                >= Self.limit
            listing = .ready
        case .failure:
            recent = fallback
            repositories = []
            roots = []
            capped = false
            listing = .unavailable
        }
        if directory.isEmpty, let first = recent.first ?? repositories.first {
            directory = first.path
        }
    }

    public func choose(directory path: String) {
        directory = path
        failure = nil
    }

    public func choose(provider chosen: Provider) {
        guard provider != chosen else { return }
        provider = chosen
        failure = nil
    }

    /// What the chooser shows for the search typed so far.
    public var found: [Directory] {
        let wanted = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !wanted.isEmpty else { return repositories }
        return (recent + repositories).filter {
            $0.name.lowercased().contains(wanted) || $0.path.lowercased().contains(wanted)
        }
    }

    /// Whether a search has to ask the host, because the list it gave was cut.
    public var searchesTheMachine: Bool {
        capped && !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    public var typedPath: String {
        typed.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public var name: String { typedName ?? suggestedName }

    /// The directory's own name, made new on its host, or "agent".
    public var suggestedName: String {
        let trimmed = directory.hasSuffix("/") ? String(directory.dropLast()) : directory
        let last = trimmed.split(separator: "/").last.map(String.init)
        let base = last.flatMap { $0.isEmpty ? nil : $0 } ?? "agent"
        let names = machine.flatMap { taken[$0] } ?? []
        guard names.contains(base) else { return base }
        var number = 2
        while names.contains("\(base)-\(number)") { number += 1 }
        return "\(base)-\(number)"
    }

    public func choose(name written: String) {
        typedName = written
        failure = nil
    }

    public var chosenName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    public var ready: Bool {
        machine != nil && !directory.isEmpty && !chosenName.isEmpty && !starting
    }

    /// What starting asks the host for.
    public var request: NewAgent? {
        guard ready, let machine else { return nil }
        return NewAgent(
            hostId: machine.bytes, kind: provider.kind, cwd: directory, name: chosenName,
            model: nil)
    }

    public func starts() {
        starting = true
        failure = nil
    }

    public func started(_ result: Result<AgentKey, RuntimeFailure>) {
        starting = false
        switch result {
        case .success(let agent):
            created = agent
            if let machine { taken[machine, default: []].insert(chosenName) }
        case .failure(let refusal):
            failure = refusal.description
        }
    }
}

extension Directory: Identifiable {
    public var id: String { path }
}
