import AmuxCore
import Foundation

/// States drawn from a real account store rather than from events a fixture
/// wrote by hand.
///
/// What a launch draws before anything has connected comes from the SQLite
/// store the account's runtime kept, read back through the reducer and
/// projection the running library uses. A fixture that invented those rows
/// would photograph what the fixture believes a remembered fleet looks like;
/// these write a store and read it back, so the photograph is of what the
/// store really gives a launch.
public enum RememberedStores {
    /// Writes a store remembering `remembered`, then reads back what a launch
    /// draws from it: the cached fleet, and the named conversation opened on
    /// its stored window when there is one.
    ///
    /// Installed by the driving build, which links the bridge's seeding entry
    /// points. Where nothing is installed these states draw an empty fleet.
    @MainActor public static var read: ((Remembered, AgentId?) -> [Event])?

    /// The account every store-backed state is written under.
    public static let account = AccountId("door")
}

/// What one account's store remembers.
public struct Remembered: Sendable {
    public var local: HostId
    public var hosts: [HostEntry]
    public var agents: [Agent]
    /// Agents their machine has since said are gone.
    public var removed: [AgentId]
    /// Each conversation's transcript rows, as the provider wrote them.
    public var chats: [AgentId: [String]]
    /// Rows a later connection delivered for a conversation in `chats` after
    /// its machine could no longer serve the rows in between.
    public var afterGap: [AgentId: [String]]
    /// What each agent's machine last published about it, stored as that
    /// machine's summary.
    public var standings: [AgentId: Standing]

    public init(
        local: HostId = Scenario.phone, hosts: [HostEntry], agents: [Agent],
        removed: [AgentId] = [], chats: [AgentId: [String]] = [:],
        afterGap: [AgentId: [String]] = [:], standings: [AgentId: Standing] = [:]
    ) {
        self.local = local
        self.hosts = hosts
        self.agents = agents
        self.removed = removed
        self.chats = chats
        self.afterGap = afterGap
        self.standings = standings
    }

    /// The standing a machine publishes for one agent.
    public struct Standing: Encodable, Sendable {
        public var attention: Attention
        public var phase: AgentPhase
        public var lastActivity: Date

        public init(_ card: AgentCard) {
            attention = card.attention
            phase = card.phase
            lastActivity = card.lastActivity
        }

        private enum CodingKeys: String, CodingKey {
            case attention, phase
            case lastActivity = "last_activity"
        }
    }

    /// The JSON the bridge's seeding entry point reads.
    public func json() throws -> Data {
        struct Fleet: Encodable {
            let local: HostId
            let hosts: [HostEntry]
            let agents: [Agent]
            let removed: [AgentId]
        }
        let fleet = try AmuxJSON.encoder.encode(
            Fleet(local: local, hosts: hosts, agents: agents, removed: removed))
        guard var object = try JSONSerialization.jsonObject(with: fleet) as? [String: Any] else {
            return fleet
        }
        if var records = object["agents"] as? [[String: Any]] {
            for index in records.indices {
                records[index]["last_activity"] = records[index]["created_at"]
            }
            object["agents"] = records
        }
        func rows(_ conversations: [AgentId: [String]]) throws -> [String: Any] {
            var object: [String: Any] = [:]
            for (agent, rows) in conversations {
                object[agent.description] = try rows.map {
                    try JSONSerialization.jsonObject(with: Data($0.utf8))
                }
            }
            return object
        }
        object["chats"] = try rows(chats)
        object["after_gap"] = try rows(afterGap)
        var standings: [String: Any] = [:]
        for (agent, standing) in self.standings {
            standings[agent.description] = try JSONSerialization.jsonObject(
                with: AmuxJSON.encoder.encode(standing))
        }
        object["standings"] = standings
        return try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }
}

extension Remembered {
    /// The morning's fleet as this phone last saw it, with air no longer
    /// answering: every agent is still listed, waiting for its machine, and
    /// each is drawn with the standing its machine last published and how
    /// long ago that was.
    public static var unreachable: Remembered {
        Remembered(
            hosts: Scenario.hosts.map(\.entry),
            agents: Scenario.agents.map(\.agent),
            standings: Dictionary(
                uniqueKeysWithValues: Scenario.agents.map { ($0.id, Standing($0)) }))
    }

    /// The same fleet after Studio said two of its agents were gone before the
    /// phone lost its connection: those two are not remembered at all.
    public static var removal: Remembered {
        var remembered = unreachable
        remembered.removed = [Scenario.agentId("docs-pass"), Scenario.agentId("ios-bridge")]
        return remembered
    }

    /// The fleet with Studio, where the conversation this app opens on runs,
    /// no longer answering.
    static var studioAway: Remembered {
        Remembered(
            hosts: Scenario.hosts.map { host in
                var entry = host.entry
                if entry.id == Scenario.studio { entry.online = false }
                return entry
            },
            agents: Scenario.agents.map(\.agent))
    }

    /// The fleet with the conversation this app opens on kept in the store.
    public static var chat: Remembered {
        var remembered = studioAway
        remembered.chats = [Scenario.focus: rememberedTranscript]
        return remembered
    }

    /// The same conversation after the phone reconnected to a machine that no
    /// longer held what was said while it was away: the store keeps what came
    /// before and after, with a break between them.
    public static var gap: Remembered {
        var remembered = chat
        remembered.afterGap = [Scenario.focus: transcriptAfterGap]
        return remembered
    }

    /// What the conversation went on to say once the phone could hear it
    /// again.
    static let transcriptAfterGap: [String] = [
        #"{"type":"user","uuid":"dddddddd-0000-4000-8000-000000000011","sessionId":"22222222-2222-4222-8222-222222222222","timestamp":"2025-12-01T09:40:00.000Z","message":{"role":"user","content":"Did collapsing them into one timer fix it?"},"origin":{"kind":"human"},"promptSource":"typed"}"#,
        #"{"type":"assistant","uuid":"dddddddd-0000-4000-8000-000000000012","sessionId":"22222222-2222-4222-8222-222222222222","timestamp":"2025-12-01T09:40:06.000Z","message":{"id":"message-11","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Yes. A dropped link now waits once, and the reconnect test passes."}]}}"#,
    ]

    /// A short Claude transcript: the session starting, one question and its
    /// answer.
    static let rememberedTranscript: [String] = [
        #"{"type":"amux.transcript_ready"}"#,
        #"{"type":"user","uuid":"dddddddd-0000-4000-8000-000000000001","sessionId":"22222222-2222-4222-8222-222222222222","timestamp":"2025-12-01T09:10:00.000Z","message":{"role":"user","content":"Why does the reconnect loop back off twice?"},"origin":{"kind":"human"},"promptSource":"typed"}"#,
        #"{"type":"assistant","uuid":"dddddddd-0000-4000-8000-000000000002","sessionId":"22222222-2222-4222-8222-222222222222","timestamp":"2025-12-01T09:10:05.000Z","message":{"id":"message-1","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Both the relay dialer and the session retry wait before trying again, so a dropped link waits for each in turn."}]}}"#,
    ]
}

extension States {
    /// Opens a state from what an account's store remembers.
    @MainActor
    public static func remembered(
        _ bundle: StoreBundle, _ remembered: Remembered, chat: AgentId? = nil
    ) {
        bundle.fleet.unread = Scenario.mostlyRead
        bundle.apply(RememberedStores.read?(remembered, chat) ?? [])
        bundle.fleet.refreshOrder(now: Scenario.now)
    }
}
