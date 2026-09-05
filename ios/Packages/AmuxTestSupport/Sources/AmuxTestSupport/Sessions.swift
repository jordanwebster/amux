import AmuxCore
import Foundation

/// Session states, in each layer's own vocabulary. The two providers ask for
/// permission in different words and offer different choices, and the fixtures
/// keep them apart rather than inventing a shared one.
public enum Sessions {
    public static func claude(
        gate: ClaudePtySendGate = .ready,
        phase: String = "idle",
        stream: StreamPhase? = .live,
        asks: [Ask] = [],
        provider: ProviderFacts = claudeProvider,
        settingsGate: SettingsGate = .ptySettingsUnavailable,
        queue: QueuedMessage? = nil,
        family: [FamilyMember] = [],
        agent: AgentId = Scenario.focus
    ) -> SessionSnapshot {
        SessionSnapshot(
            agent: agent,
            gate: .claudePty(gate),
            phase: .claudePty(.object(["phase": .string(phase), "tag": .string("inferred")])),
            stream: stream,
            asks: asks,
            facts: .claudePty(.object([
                "layer": .string("claude_pty"),
                "session": .object(["permission_mode": .string("default"), "ai_title": .null,
                                    "agent_name": .null]),
                "accepted_plans": .array([]),
                "echoes": .array([]),
            ])),
            provider: provider,
            settingsGate: settingsGate,
            queue: queue,
            family: family)
    }

    public static func codex(
        gate: CodexSendGate = .ready,
        phase: String = "idle",
        asks: [Ask] = [],
        provider: ProviderFacts = codexProvider,
        settingsGate: SettingsGate = .ready,
        agent: AgentId = Scenario.agentId("spec-suite")
    ) -> SessionSnapshot {
        SessionSnapshot(
            agent: agent,
            gate: .codex(gate),
            phase: .codex(.object(["phase": .string(phase)])),
            stream: .live,
            asks: asks,
            facts: .codex(.object(["layer": .string("codex"), "active_turn_id": .null])),
            provider: provider,
            settingsGate: settingsGate,
            queue: nil,
            family: [])
    }

    /// An agent this build cannot read. It states that outright rather than
    /// presenting an empty conversation as an idle one.
    public static func unreadable(agent: AgentId = Scenario.agentId("legacy-port")) -> SessionSnapshot {
        SessionSnapshot(
            agent: agent,
            gate: .unavailable,
            phase: .unavailable,
            stream: nil,
            asks: [],
            facts: .claudeSdk(supported: false),
            provider: ProviderFacts(),
            settingsGate: .unavailable,
            queue: nil,
            family: [])
    }

    // MARK: - Provider facts

    public static let claudeProvider = ProviderFacts(
        model: "opus-4.6",
        models: [
            ModelInfo(id: "opus-4.6", name: "opus 4.6", efforts: [], defaultEffort: nil),
            ModelInfo(id: "sonnet-5", name: "sonnet 5", efforts: [], defaultEffort: nil),
            ModelInfo(id: "haiku-4.5", name: "haiku 4.5", efforts: [], defaultEffort: nil),
        ],
        commands: [
            ProviderCommand(name: "handoff", source: .string("claude"), terminalOnly: false),
            ProviderCommand(name: "code-review", source: .string("claude"), terminalOnly: false),
            ProviderCommand(name: "tasks", source: .string("claude"), terminalOnly: false),
            ProviderCommand(name: "compact", source: .string("claude"), terminalOnly: false),
            ProviderCommand(name: "doctor", source: .string("claude"), terminalOnly: true),
        ],
        permission: .object(["provider": .string("claude"), "mode": .string("default")]))

    public static let codexProvider = ProviderFacts(
        model: "gpt-5.2",
        effort: "medium",
        models: [
            ModelInfo(id: "gpt-5.2", name: "gpt-5.2", efforts: ["low", "medium", "high"],
                      defaultEffort: "medium"),
            ModelInfo(id: "gpt-5.2-mini", name: "gpt-5.2-mini", efforts: ["low", "medium", "high"],
                      defaultEffort: "low"),
        ],
        efforts: ["low", "medium", "high"],
        permission: .object([
            "provider": .string("codex"),
            "approval": .string("on-request"),
            "sandbox": .string("workspace-write"),
        ]))

    /// The task list the provider keeps, folded by the core rather than
    /// counted on the phone.
    public static let todos = TaskList(
        done: 3, total: 7,
        current: "Update the three spec tests that assert on the old strings",
        items: [
            TaskItem(text: "Find every call site that maps a status to a string", state: .completed),
            TaskItem(text: "Collapse the match in pairing.rs onto one arm", state: .completed),
            TaskItem(text: "Delete the three unused error constants", state: .completed),
            TaskItem(text: "Update the three spec tests that assert on the old strings",
                     state: .inProgress),
            TaskItem(text: "Run the spec suite", state: .pending),
            TaskItem(text: "Check nothing in docs asserts on the old copy", state: .pending),
            TaskItem(text: "Write the changelog line", state: .pending),
        ])

    // MARK: - Asks

    /// Claude asking to run a command.
    public static let claudePermission = Ask(layer: .claudePty, body: .object([
        "id": .int(1), "seq": .int(9),
        "tool_use_id": .string("toolu_9"),
        "session_ask_id": .string("ask-1"),
        "kind": .object([
            "ask": .string("permission"),
            "tool_name": .string("Bash"),
            "invocation": .object([
                "tool": .string("bash"),
                "command": .string("cargo test --workspace --test spec"),
                "description": .string("Watch the three tests fail before calling it done"),
            ]),
            "suggestions": .array([]),
        ]),
        "state": .object(["state": .string("pending")]),
        "document": .null,
    ]))

    /// Claude asking a question with options.
    public static let claudeQuestion = Ask(layer: .claudePty, body: .object([
        "id": .int(2), "seq": .int(11),
        "tool_use_id": .string("toolu_11"),
        "session_ask_id": .string("ask-2"),
        "kind": .object([
            "ask": .string("question"),
            "questions": .array([.object([
                "header": .string("Ownership"),
                "question": .string("Which crate should own the redaction table?"),
                "multi_select": .bool(false),
                "options": .array([
                    .object(["label": .string("amux-core"), "description": .null]),
                    .object(["label": .string("amux-ui"), "description": .null]),
                    .object(["label": .string("a new crate"), "description": .null]),
                ]),
            ])]),
        ]),
        "state": .object(["state": .string("pending")]),
        "document": .null,
    ]))

    /// A plan to approve. It is a permission whose payload carries the plan,
    /// not a third kind of ask.
    public static let claudePlan = Ask(layer: .claudePty, body: .object([
        "id": .int(3), "seq": .int(13),
        "tool_use_id": .string("toolu_13"),
        "session_ask_id": .string("ask-3"),
        "kind": .object([
            "ask": .string("permission"),
            "tool_name": .string("ExitPlanMode"),
            "invocation": .object([
                "tool": .string("plan"),
                "plan": .string(planMarkdown),
                "plan_file_path": .null,
            ]),
            "suggestions": .array([]),
        ]),
        "state": .object(["state": .string("pending")]),
        "document": .null,
    ]))

    /// The same request in Codex's words, with Codex's own choices.
    public static let codexPermission = Ask(layer: .codex, body: .object([
        "seq": .int(9),
        "request_id": .string("req-9"),
        "context": .object([
            "ask": .string("command"),
            "item_id": .string("item-9"),
            "command": .string("cargo test --workspace --test spec"),
            "cwd": .string("~/src/amux"),
            "reason": .string("Runs outside the workspace sandbox"),
            "proposed_execpolicy_amendment": .null,
            "proposed_network_policy_amendments": .array([]),
        ]),
        "actions": .array([
            .object(["wire": .string("approve"),
                     "meaning": .object(["meaning": .string("scalar"), "decision": .string("approve")])]),
            .object(["wire": .string("approve_for_session"),
                     "meaning": .object(["meaning": .string("scalar"),
                                         "decision": .string("approve_for_session")])]),
            .object(["wire": .string("deny"),
                     "meaning": .object(["meaning": .string("scalar"), "decision": .string("deny")])]),
        ]),
    ]))

    /// A message held until the turn ends.
    public static let heldMessage = QueuedMessage(
        draft: .object([
            "text": .string("Also update the changelog line once the suite is green."),
            "attachments": .array([]),
        ]),
        heldAt: Scenario.now.addingTimeInterval(-45),
        delivery: .object(["delivery": .string("held")]))

    /// The agents this one started, and the one that is stuck.
    public static let family: [FamilyMember] = [
        FamilyMember(agent: Scenario.agentId("spec-fixer"), depth: 1, needs: .permission),
        FamilyMember(agent: Scenario.agentId("docs-sweep"), depth: 1, needs: nil),
    ]

    public static let planMarkdown = """
        The client maps gRPC statuses onto distinct strings in three places. The protocol \
        refuses to distinguish them, so the client must not either.

        ## Approach

        1. Read every call site that maps a gRPC status to a string — 6 files
        2. Replace the match in `amux-ui/src/pairing.rs` with one arm
        3. Delete the three error constants nothing else reads
        4. Update the two spec tests that assert on the old strings

        ## What I will not do

        - Touch the daemon-side mapping
        - Change the retry budget, which looks wrong but is a separate change
        """
}
