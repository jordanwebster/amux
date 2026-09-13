import Foundation

/// The writes that are about the agent rather than to it: how it runs, what it
/// is called, and whether it goes on existing.
///
/// Spelled here beside the draft's own commands so there is one place the
/// phone's vocabulary can be held against the core's. Every one of these is a
/// shared `Command` exactly as the core serialises it — the tag it reads is
/// `command`, and a provider-native write carries its own tag underneath,
/// because the two providers do not agree about what a permission is and the
/// core keeps that asymmetry rather than inventing a common word for it.
public enum AgentWrite {
    /// The model this agent's next turn runs under.
    public static func model(_ model: String, of agent: AgentId) -> BridgeCommand {
        .shared(.object([
            "command": .string("set_model"),
            "agent": .string(agent.description),
            "model": .string(model),
        ]))
    }

    /// How hard it thinks, as one of the levels the layer reported.
    public static func effort(_ effort: String, of agent: AgentId) -> BridgeCommand {
        .shared(.object([
            "command": .string("set_effort"),
            "agent": .string(agent.description),
            "effort": .string(effort),
        ]))
    }

    /// Codex runs under two axes at once and the core takes them together, so
    /// picking a preset is one write and not two: an approval policy applied
    /// without its sandbox would leave the agent in a pair nobody chose.
    public static func preset(
        approval: String, sandbox: String, of agent: AgentId
    ) -> BridgeCommand {
        .shared(.object([
            "command": .string("set_preset"),
            "agent": .string(agent.description),
            "approval": .string(approval),
            "sandbox": .string(sandbox),
        ]))
    }

    /// Claude runs under a single named mode, which is its own vocabulary and
    /// travels as its own command rather than as a preset with one axis.
    public static func permissionMode(_ mode: String, of agent: AgentId) -> BridgeCommand {
        .shared(.object([
            "command": .string("claude_sdk"),
            "claude_sdk_command": .string("set_permission_mode"),
            "agent": .string(agent.description),
            "mode": .string(mode),
        ]))
    }

    /// What this agent is called, everywhere it is named.
    public static func rename(_ name: String, of agent: AgentId) -> BridgeCommand {
        .shared(.object([
            "command": .string("rename_agent"),
            "agent": .string(agent.description),
            "name": .string(name),
        ]))
    }

    /// The agent, gone. The host ends the session and drops the conversation
    /// on every device; the edits it made are left where they are.
    public static func delete(_ agent: AgentId) -> BridgeCommand {
        .shared(.object([
            "command": .string("delete_agent"),
            "agent": .string(agent.description),
        ]))
    }
}

extension ProviderPermission {
    /// The write that puts this agent under the choice with this identity, in
    /// whichever vocabulary its provider uses.
    ///
    /// Nothing where the layer reports no permissions at all, and nothing for
    /// a Codex configuration the presets do not name: "Custom" is the sheet
    /// reporting where the agent already is, not somewhere it can be sent.
    public func command(choosing choice: String, agent: AgentId) -> BridgeCommand? {
        switch self {
        case .unavailable:
            nil
        case .claude:
            AgentWrite.permissionMode(choice, of: agent)
        case .codex:
            PermissionChoice.codexAxes(choice).map {
                AgentWrite.preset(approval: $0.approval, sandbox: $0.sandbox, of: agent)
            }
        }
    }
}
