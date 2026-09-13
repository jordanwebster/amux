import Foundation

/// One agent this conversation started, and where answering it happens.
///
/// Two different things are called a child here and the strip must not pretend
/// they are one. An agent amux started is an agent in its own right: it has a
/// conversation, it can be waiting on somebody, and going to it is going
/// somewhere. Work the provider runs inside this session — Claude's own
/// subagents — has no session, no address and nothing to open; it is a thing
/// this transcript reports, not a place.
public struct ChildRow: Identifiable, Equatable, Sendable {
    /// Stable across arrivals so a strip does not reshuffle when a child
    /// finishes: an agent is its identity, provider-internal work is its name.
    public let id: String
    public let name: String
    /// What it is waiting for, when it is waiting for somebody. Only an agent
    /// can be: the providers report their own subagents as work in progress
    /// and never as something asking for the person, and an ask raised inside
    /// one surfaces on this session rather than under the subagent's name.
    public let needs: Why?
    /// The last thing the layer said about it, in the layer's own word —
    /// "finished", or nothing while it is still going.
    public let state: String?
    public let place: Place

    public enum Place: Equatable, Sendable {
        /// An agent with a conversation of its own, reached by identity.
        case agent(AgentId)
        /// Work running inside this session, with nowhere to go.
        case insideThisSession
    }

    public init(id: String, name: String, needs: Why?, state: String?, place: Place) {
        self.id = id
        self.name = name
        self.needs = needs
        self.state = state
        self.place = place
    }

    /// The conversation to open, or nothing where there is none.
    public var openable: AgentId? {
        guard case .agent(let agent) = place else { return nil }
        return agent
    }

    /// Why this one cannot be opened, said to a person rather than implied by
    /// a control that does nothing.
    ///
    /// A row that simply refused the tap would read as the app being broken.
    /// The sentence is here rather than in the view because it is a fact about
    /// where the work runs, and the same fact is what a journey asserts on.
    public var unopenable: String? {
        switch place {
        case .agent: nil
        case .insideThisSession:
            "This one runs inside the session and has no conversation of its own."
        }
    }
}

extension ChildRow {
    /// Everything this agent started, wherever it runs.
    ///
    /// The agents come first and in the order the core ranked them, which
    /// already puts whoever is waiting nearest the top; provider-internal work
    /// follows in the order the transcript reported it. A subagent that
    /// started and then finished is one child, not two, so the rows fold by
    /// name and the last thing said about it wins.
    public static func roster(
        family: [FamilyMember], rows: [TranscriptRow],
        named: (AgentId) -> String = { $0.description }
    ) -> [ChildRow] {
        var roster = family.map { member in
            ChildRow(
                id: member.agent.description, name: named(member.agent),
                needs: member.needs, state: nil, place: .agent(member.agent))
        }
        var inside: [String: Int] = [:]
        for row in rows {
            guard case .subagent(let name, let kind, let state) = row.kind else { continue }
            let label = [name, kind].compactMap { $0 }.joined(separator: " \u{00B7} ")
            let child = ChildRow(
                id: label, name: label, needs: nil, state: state,
                place: .insideThisSession)
            if let at = inside[label] {
                roster[at] = child
            } else {
                inside[label] = roster.count
                roster.append(child)
            }
        }
        return roster
    }
}

extension ConversationStore {
    /// The children this conversation would list, named by whoever knows their
    /// names.
    ///
    /// The fleet owns an agent's name and this store does not, so a caller
    /// that has the fleet hands the naming in; one that does not gets the
    /// identity, which is at least true.
    public func children(named: (AgentId) -> String = { $0.description }) -> [ChildRow] {
        ChildRow.roster(family: family, rows: entries.transcriptRows(), named: named)
    }
}
