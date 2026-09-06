import AmuxCore
import AmuxFeatures
import Foundation

/// Every named state this build can be put into.
///
/// One per screen the design describes, plus the states a screenshot of a good
/// morning never shows: a host lost mid-turn, an agent this build cannot read,
/// a send that was refused, an upload that failed, and the whole thing at an
/// accessibility type size.
public enum Fixtures {
    public static let all: [Fixture] = catalogue + states

    public static func named(_ id: String) -> Fixture? {
        all.first { $0.id == id }
    }

    /// One state this build can be asked for: a screen and what fills it.
    public struct Built: Hashable, Sendable {
        public let screen: Screen
        public let state: String

        public init(_ screen: Screen, _ state: String) {
            self.screen = screen
            self.state = state
        }
    }

    /// Whether this build draws that state.
    ///
    /// Built-ness belongs to the pair, not to the screen. The conversation
    /// screen draws a conversation, but the conversation whose host was lost
    /// mid-turn, the one stripped back to its rows and the one at an
    /// accessibility type size are separate states with separate baselines.
    /// Declared per screen, all of them became openable the moment the first
    /// one landed, and a check of "everything built so far still draws what it
    /// was locked as" started failing on work nobody had started.
    ///
    /// Asked before the state is looked up, so a state nobody has written yet
    /// answers "unimplemented" rather than "no state named": not having been
    /// written is exactly what being unbuilt means.
    public static func isBuilt(_ screen: Screen, state: String) -> Bool {
        built.contains(Built(screen, state))
    }

    /// Every state that is drawn and locked today. A state joins this list in
    /// the same commit that establishes its baseline.
    static let built: Set<Built> = [
        Built(.probe, "probe"),
        Built(.home, "home"),
        Built(.home, "home-accessibility"),
        Built(.homeQuiet, "home-quiet"),
        Built(.drawer, "drawer"),
        Built(.run, "run"),
        Built(.runLive, "run-live"),
        Built(.voices, "voices"),
        Built(.reviewCta, "review-cta"),
        Built(.firstRun, "first-run"),
        Built(.firstRunPaid, "first-run-paid"),
    ]

    /// The screens the design catalogue describes, in its own order.
    public static let catalogue: [Fixture] = [
        // The harness's own target, which is not a screen of the app.
        Fixture(id: "probe", screen: .probe),

        // 1 · Opening the app
        Fixture(id: "home", screen: .home) { bundle in
            States.open(bundle)
        },
        Fixture(id: "home-quiet", screen: .homeQuiet) { bundle in
            // Nothing blocked and nothing unread: the exceptions line appears
            // only because one machine is actually unreachable.
            States.open(bundle, agents: Scenario.settledAgents, unread: Scenario.allRead)
        },
        Fixture(id: "drawer", screen: .drawer) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },

        // 2 · A conversation
        Fixture(id: "run", screen: .run) { bundle in
            States.open(
                bundle, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .needsYou, phase: "idle",
                                         asks: [Sessions.claudePermission]))
        },
        Fixture(id: "run-live", screen: .runLive) { bundle in
            States.open(bundle, entries: Transcript.live,
                        session: Sessions.claude(gate: .working, phase: "running"))
        },
        Fixture(id: "voices", screen: .voices) { bundle in
            States.open(
                bundle, entries: Transcript.everyKind,
                session: Sessions.claude(family: Sessions.family))
        },
        Fixture(id: "review-cta", screen: .reviewCta) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.changes)
        },

        // 3 · When it needs you
        Fixture(id: "ask-permission", screen: .askPermission) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePermission]))
        },
        Fixture(id: "ask-question", screen: .askQuestion) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudeQuestion]))
        },
        Fixture(id: "plan", screen: .plan) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePlan]))
        },
        Fixture(id: "diff", screen: .diff) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.changes)
        },
        Fixture(id: "comment", screen: .comment) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.changes)
        },

        // 4 · Writing to it
        Fixture(id: "typing", screen: .typing) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "plus", screen: .plus) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "settings", screen: .settings) { bundle in
            // Model and effort on a Codex session, which is the layer this
            // build can actually change them on.
            States.open(
                bundle, entries: [], agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex())
        },
        Fixture(id: "slash-typing", screen: .slashTyping) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "working", screen: .working) { bundle in
            States.open(
                bundle, entries: Transcript.live,
                session: Sessions.claude(
                    gate: .working, phase: "running",
                    provider: ProviderFacts(
                        model: Sessions.claudeProvider.model,
                        models: Sessions.claudeProvider.models,
                        commands: Sessions.claudeProvider.commands,
                        permission: Sessions.claudeProvider.permission,
                        todos: Sessions.todos)))
        },
        Fixture(id: "queued", screen: .queued) { bundle in
            States.open(bundle, entries: Transcript.live,
                        session: Sessions.claude(gate: .working, phase: "running",
                                                 queue: Sessions.heldMessage))
        },
        Fixture(id: "overflow", screen: .overflow) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "agent-delete", screen: .agentDelete) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },

        // 5 · Machines and agents
        Fixture(id: "hosts", screen: .hosts) { bundle in
            States.open(bundle)
        },
        Fixture(id: "pin", screen: .pin) { bundle in
            States.open(bundle)
        },
        Fixture(id: "new-agent", screen: .newAgent) { bundle in
            States.open(bundle)
        },
        Fixture(id: "offline", screen: .offline) { bundle in
            // One machine unreachable: the agent on it is genuinely unknown,
            // which is not the same as idle and must never be drawn as idle.
            States.open(bundle, extra: [States.offline("air is not reachable")])
        },
        Fixture(id: "exited", screen: .exited) { bundle in
            var ended = Scenario.agents
            ended[0].phase = .exited(exitCode: 1)
            ended[0].attention = .unknown
            States.open(
                bundle, agents: ended, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .exited, stream: .closed(
                    reason: .object(["reason": .string("agent_exited")]))))
        },

        // 6 · You
        Fixture(id: "profiles", screen: .profiles) { bundle in
            States.open(bundle)
        },
        Fixture(id: "you", screen: .you) { bundle in
            States.open(bundle)
        },
        Fixture(id: "delete", screen: .delete, cloud: ScriptedCloudState(
            deletion: .blockedByRenewal(source: .appStore,
                                        manageURL: URL(string: "https://apps.apple.com/account/subscriptions")!))
        ) { bundle in
            States.open(bundle)
        },
        Fixture(id: "first-run", screen: .firstRun, cloud: .firstRun, accounts: []),
        Fixture(id: "sign-in", screen: .signIn, cloud: .firstRun, accounts: []),
        Fixture(id: "first-run-paid", screen: .firstRunPaid, cloud: .unsubscribed,
                accounts: [Fixture.unsubscribed]),
        Fixture(id: "paywall", screen: .paywall, cloud: .unsubscribed),

        // 7 · When it goes wrong
        Fixture(id: "shake", screen: .shake) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "dump", screen: .dump) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
    ]

    /// States a screenshot of a good morning never shows.
    public static let states: [Fixture] = [
        // Both permission vocabularies. Codex asks in its own words and offers
        // its own choices; flattening the two into one would put words in a
        // provider's mouth.
        Fixture(id: "ask-permission-codex", screen: .askPermission) { bundle in
            States.open(
                bundle, entries: [], agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex(gate: .needsYou, asks: [Sessions.codexPermission]))
        },
        Fixture(id: "settings-codex", screen: .settings) { bundle in
            States.open(bundle, agent: Scenario.agentId("spec-suite"), session: Sessions.codex())
        },
        // A link that landed on a confirmation. It names the host and never
        // pairs on arrival.
        Fixture(id: "pair-confirm", screen: .pin) { bundle in
            States.open(bundle, hosts: Scenario.hosts + [HostState(entry: Scenario.unpaired, epoch: 1)])
        },
        // The host went away mid-turn. The feed stays readable and says so.
        Fixture(id: "host-lost", screen: .run) { bundle in
            var lost = Scenario.hosts
            lost[0].entry.online = false
            lost[0].entry.lastDialError = "connection reset"
            States.open(
                bundle, hosts: lost, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .unknown, stream: .closed(
                    reason: .object(["reason": .string("host_unreachable")]))),
                extra: [States.offline("Studio is not reachable")])
        },
        // An agent this build cannot read. It is not offered to open.
        Fixture(id: "unreadable", screen: .run) { bundle in
            States.open(
                bundle, entries: [], agent: Scenario.agentId("legacy-port"),
                session: Sessions.unreadable())
        },
        // A send the layer refused, with the reason visible and no input
        // reaching the host.
        Fixture(id: "send-refused", screen: .run) { bundle in
            States.open(
                bundle, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .replaying, phase: "replaying", stream: .replaying),
                extra: [.opResult(OpResult(
                    op: OpId(UUID(uuidString: "00000000-0000-0000-0000-00000000FA11")!),
                    outcome: .failed(refusal)))])
        },
        // The report could not be sent. The draft is not lost.
        Fixture(id: "upload-failed", screen: .dump,
                cloud: ScriptedCloudState(upload: .offline)) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "sign-in-failed", screen: .signIn,
                cloud: ScriptedCloudState(signIn: .refused("that address is not recognised"),
                                          entitlement: .none, token: nil)),
        // Nothing yet: one action, and no list pretending to be loading.
        Fixture(id: "home-empty", screen: .home) { bundle in
            States.open(bundle, agents: [], hosts: [], unread: UnreadWeights())
        },
        // The cache before the network answers: rows are shown and marked
        // unconfirmed rather than replaced by a spinner.
        Fixture(id: "home-cached", screen: .home) { bundle in
            States.open(bundle, agents: Scenario.remembered, reconciled: false)
        },
        // The same screens for a reader who needs larger type. Nothing is
        // dropped at this size; it wraps.
        Fixture(id: "home-accessibility", screen: .home, typeSize: "accessibility3") { bundle in
            States.open(bundle)
        },
        Fixture(id: "run-accessibility", screen: .run, typeSize: "accessibility3") { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePermission]))
        },
    ]

    private static let refusal: OpFailure = {
        let json = Data("""
            {"error":"general","message":"the session is replaying history",\
            "auth_required":false,"subscription_required":false}
            """.utf8)
        // The refusal is the core's own sentence, decoded rather than retyped.
        return try! AmuxJSON.decoder.decode(OpFailure.self, from: json)
    }()
}
