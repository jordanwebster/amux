import Foundation
import Observation

/// Starting an agent: which machine, which directory, which layer.
///
/// One attempt at a time, like pairing. A phone is not a place where two
/// agents are being started at once, and holding only one is what lets the
/// screen answer "did the machine refuse this" without carrying an identifier
/// around.
///
/// Nothing here reaches a machine. It records what somebody has chosen and
/// what the machine answered; whoever owns the connection carries the two
/// requests out.
@MainActor
@Observable
public final class NewAgentStore {
    /// Which layer the new agent runs under.
    ///
    /// Two, and no driver among them: this app starts Claude through the SDK
    /// and there is no third thing to choose. An agent already running under
    /// the terminal driver is still opened under it — what is missing is a way
    /// to start another one.
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
    }

    /// How the directories a machine offers stand.
    public enum Listing: Equatable, Sendable {
        /// No machine has been asked yet.
        case none
        case asking
        case ready
        /// The machine could not be asked, or would not answer. Not an error
        /// to recover from: a typed path still works, and that is what the
        /// screen offers instead.
        case unavailable
    }

    /// How many directories a machine is asked for at once.
    ///
    /// The machine's own cap, so one request fetches everything a machine of
    /// ordinary size has and the search box can filter what is already here
    /// rather than going back for every keystroke. A machine with more than
    /// this many is asked again with the query, which is the only case where
    /// the local list could be hiding the answer.
    public static let limit = 200

    public private(set) var machine: HostId?
    public private(set) var listing = Listing.none
    /// Directories an agent has run in on this machine, most recent first.
    public private(set) var recent: [Project] = []
    /// Repositories under the machine's roots. A directory that is already in
    /// `recent` is not repeated here.
    public private(set) var repositories: [Project] = []
    /// Where the machine looked. Shown when it found nothing, because "no
    /// repositories" is only useful beside where it searched.
    public private(set) var roots: [String] = []
    /// The machine gave everything it had up to the limit and may have more.
    public private(set) var capped = false
    /// Where the agent will start. Empty until something has been chosen.
    public private(set) var directory = ""
    /// What is being searched for among the machine's repositories.
    public var query = ""
    /// A path somebody typed, for a directory the machine did not list.
    public var typed = ""
    /// Whether the directory chooser is open.
    ///
    /// A state of this screen rather than a page of its own: what is being
    /// chosen is one field of the thing being built, and going somewhere else
    /// to choose it would take the machine, the layer and the button that
    /// starts it off the screen.
    public var browsing = false
    public private(set) var provider = Provider.claude
    /// The model Codex is asked for, or nothing to leave it to the machine.
    /// Claude carries no model: what a create request says about Claude is the
    /// driver and nothing else.
    public private(set) var model: String?
    /// The Codex models this account has been told about, gathered from the
    /// sessions it is already running.
    ///
    /// There is no way to ask a machine what models a layer offers before that
    /// layer is running — the list arrives with a session — so the only honest
    /// list is the one the running sessions reported. An account with no Codex
    /// agent yet has none, and the card says the machine's default instead of
    /// offering a list made up here.
    public private(set) var codexModels: [ModelInfo] = []
    /// A request is with the machine.
    public private(set) var starting = false
    /// What the machine said when it would not start the agent. One sentence,
    /// in the machine's own words, because a path it rejected is a fact only
    /// it holds.
    public private(set) var failure: String?
    /// The agent the machine started.
    public private(set) var created: Agent?

    private var awaitingListing: OpId?
    private var awaitingCreate: OpId?

    public init() {}

    /// Starts over, on whichever machine the screen opened on.
    public func open(on host: HostId?) {
        machine = nil
        listing = .none
        recent = []
        repositories = []
        roots = []
        capped = false
        directory = ""
        query = ""
        typed = ""
        browsing = false
        provider = .claude
        model = nil
        starting = false
        failure = nil
        created = nil
        awaitingListing = nil
        awaitingCreate = nil
        if let host { machine = host }
    }

    /// Points at a machine. Everything the last machine offered goes with it:
    /// a directory on one machine names nothing on another.
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
    }

    /// A request for this machine's directories is on its way.
    public func asking(_ op: OpId?) {
        awaitingListing = op
        // Nothing to ask with — no account, no runtime — is the same as a
        // machine that would not answer: a typed path is what is left.
        listing = op == nil ? .unavailable : .asking
    }

    /// Where the agent will start.
    public func choose(directory path: String) {
        directory = path
        failure = nil
    }

    /// Which layer. Changing it drops a model chosen for the other one.
    public func choose(provider chosen: Provider) {
        guard provider != chosen else { return }
        provider = chosen
        model = nil
        failure = nil
    }

    /// Which model Codex is asked for, or nothing for the machine's default.
    public func choose(model chosen: String?) {
        model = chosen
    }

    /// A create is with the machine.
    public func starts(_ op: OpId?) {
        awaitingCreate = op
        starting = op != nil
        failure = op == nil ? "This phone has no connection to start an agent on." : nil
    }

    /// The repositories the machine listed, narrowed by what is being searched
    /// for. Recent directories are matched too — somebody typing the name of a
    /// project they worked in yesterday means that one.
    public var found: [Project] {
        let wanted = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !wanted.isEmpty else { return repositories }
        return (recent + repositories).filter {
            $0.name.lowercased().contains(wanted) || $0.path.lowercased().contains(wanted)
        }
    }

    /// Whether the machine should be asked again with what is being typed.
    ///
    /// Only where its first answer stopped at the limit. Below that the local
    /// list is everything the machine has, and filtering it is both faster and
    /// exactly as complete.
    public var searchesTheMachine: Bool {
        capped && !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The path as it now stands: whatever was chosen, or a typed one while
    /// the chooser is open and something has been typed.
    public var typedPath: String {
        typed.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// What this agent will be called. The directory's own last component,
    /// which is what a person calls the thing they are working on; the machine
    /// keeps the name and the fleet redraws from what it answers.
    public var name: String {
        let trimmed = directory.hasSuffix("/") ? String(directory.dropLast()) : directory
        let last = trimmed.split(separator: "/").last.map(String.init)
        return last.flatMap { $0.isEmpty ? nil : $0 } ?? "agent"
    }

    /// Whether there is enough to start: a machine and a directory.
    public var ready: Bool { machine != nil && !directory.isEmpty && !starting }

    /// What the create request says about the layer.
    public var kind: NewAgentKind {
        switch provider {
        case .claude: .claude
        case .codex: .codex(model: model)
        }
    }

    public func apply(_ event: Event) {
        switch event {
        // The models a layer offers arrive with a running session and nowhere
        // else, so they are gathered as they pass rather than asked for.
        case .session(let session):
            if case .codex = session.facts, !session.provider.models.isEmpty {
                codexModels = session.provider.models
            }
        case .opResult(let result):
            if result.op == awaitingListing { listed(result.outcome) }
            if result.op == awaitingCreate { started(result.outcome) }
        case .feed, .fleet, .discovered, .connection, .diff, .tokenRequest, .invariant, .devices,
             .attention:
            break
        }
    }

    private func listed(_ outcome: OpOutcome) {
        awaitingListing = nil
        switch outcome {
        case .repositories(let host, let recent, let repositories, let roots):
            // An answer about a machine nobody is looking at any more is not
            // this screen's answer.
            guard host == machine else { return }
            self.recent = recent
            self.repositories = repositories
            self.roots = roots
            capped = recent.count + repositories.count >= Self.limit
            listing = .ready
            // The directory somebody used here last is the one they are most
            // likely to want, so it is standing in the field rather than
            // waiting to be found. Anything already chosen stays.
            if directory.isEmpty, let first = recent.first ?? repositories.first {
                directory = first.path
            }
        default:
            listing = .unavailable
        }
    }

    private func started(_ outcome: OpOutcome) {
        awaitingCreate = nil
        starting = false
        switch outcome {
        case .agentCreated(let agent):
            created = agent
        case .failed(let refusal):
            // The machine's own sentence where it wrote one, and the error it
            // named where it did not. A path a machine rejected is a fact only
            // that machine holds, so nothing here is restated in this app's
            // words.
            failure = refusal.message ?? refusal.error
        default:
            // Anything else is an answer to a create that is not a create:
            // said as a refusal rather than passed over, because the one thing
            // that must never happen here is a screen that looks as though it
            // started something it did not.
            self.failure = "The machine did not start the agent."
        }
    }
}
