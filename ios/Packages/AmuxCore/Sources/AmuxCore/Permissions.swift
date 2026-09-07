import Foundation

/// What an agent is allowed to do without asking, in its provider's own words.
///
/// The two providers do not have the same idea and are not flattened into one
/// invented set. Claude runs under a **mode** and reports which. Codex runs
/// under an approval policy and a sandbox, and the pair of them is a **preset**
/// — so a preset names its two axes underneath rather than hiding them, because
/// what a sandbox permits is the thing being chosen.
public enum ProviderPermission: Equatable, Sendable {
    /// This layer does not report one, which is not the same as permissive.
    case unavailable
    case claude(mode: String?)
    case codex(approval: String?, sandbox: String?)

    /// Read off the provider's facts, which carry it as the core wrote it.
    public init(_ facts: JSONValue) {
        guard let provider = facts["provider"]?.stringValue else {
            self = .unavailable
            return
        }
        switch provider {
        case "claude": self = .claude(mode: facts["mode"]?.stringValue)
        case "codex":
            self = .codex(
                approval: Self.word(facts["approval"]), sandbox: Self.word(facts["sandbox"]))
        default: self = .unavailable
        }
    }

    /// A policy the core wrote as a bare word, or as an object naming itself.
    /// Codex spells some of both, and neither spelling is the phone's to fix.
    private static func word(_ value: JSONValue?) -> String? {
        if let word = value?.stringValue { return word }
        guard case .object(let fields)? = value else { return nil }
        return fields.keys.sorted().first
    }

    /// Every choice this provider offers, in its own order, and which one the
    /// agent is under now.
    public var choices: [PermissionChoice] {
        switch self {
        case .unavailable: []
        case .claude(let mode): PermissionChoice.claude(current: mode)
        case .codex(let approval, let sandbox):
            PermissionChoice.codex(approval: approval, sandbox: sandbox)
        }
    }

    /// What the plus says beside the word Permissions: the name of what the
    /// agent is under, or nothing where the layer has not said.
    public var current: String? { choices.first(where: \.selected)?.name }

    /// Which provider's vocabulary this is, for a sheet that has to say so.
    public var provider: String? {
        switch self {
        case .unavailable: nil
        case .claude: "Claude"
        case .codex: "Codex"
        }
    }
}

/// One thing an agent can be set to run under.
public struct PermissionChoice: Equatable, Sendable, Identifiable {
    /// What the core is told when this is picked. The provider's own spelling.
    public let id: String
    /// What it reads.
    public let name: String
    /// For Codex, the two axes named under the preset. Claude's modes are one
    /// axis and have nothing to say underneath.
    public let detail: String?
    /// Whether the agent is under this one now.
    public let selected: Bool
    /// The one that stops asking. It is the single exception to the sheet
    /// being achromatic: colour in this app means something needs you, and
    /// spending it on a permanent label would be spending it on nothing —
    /// but being in the mode that will not ask again is not a state anybody
    /// should be in without seeing it.
    public let stopsAsking: Bool

    /// Claude's five, named as Claude names them.
    ///
    /// The core reports the mode an agent is under and not the list, because
    /// the list is closed and is the provider's rather than the session's. A
    /// mode reported that is not one of the five is shown as it was reported
    /// and marked current: a client that quietly dropped a mode it had not
    /// heard of would say an agent was somewhere it is not.
    static func claude(current: String?) -> [PermissionChoice] {
        let known = [
            ("default", "Ask me", false),
            ("acceptEdits", "Accept edits", false),
            ("plan", "Plan mode", false),
            ("auto", "Auto", false),
            ("bypassPermissions", "Bypass permissions", true),
        ]
        var choices = known.map {
            PermissionChoice(
                id: $0.0, name: $0.1, detail: nil, selected: $0.0 == current, stopsAsking: $0.2)
        }
        if let current, !known.contains(where: { $0.0 == current }) {
            choices.append(PermissionChoice(
                id: current, name: current, detail: nil, selected: true, stopsAsking: false))
        }
        return choices
    }

    /// Codex's three presets, with the approval policy and the sandbox named
    /// under each.
    static func codex(approval: String?, sandbox: String?) -> [PermissionChoice] {
        let presets = [
            ("read-only", "Read Only", "untrusted", "read-only", false),
            ("auto", "Auto", "on-request", "workspace-write", false),
            ("full-access", "Full Access", "never", "danger-full-access", true),
        ]
        var choices = presets.map { preset in
            PermissionChoice(
                id: preset.0, name: preset.1,
                detail: "Approval \(spelled(preset.2)) \u{00B7} Sandbox \(spelled(preset.3))",
                selected: preset.2 == approval && preset.3 == sandbox,
                stopsAsking: preset.4)
        }
        // A pair the presets do not cover is a real configuration and is said
        // so, rather than being rounded to the nearest preset.
        if !choices.contains(where: \.selected), approval != nil || sandbox != nil {
            choices.append(PermissionChoice(
                id: "custom", name: "Custom",
                detail: "Approval \(spelled(approval)) \u{00B7} Sandbox \(spelled(sandbox))",
                selected: true, stopsAsking: sandbox == "danger-full-access"))
        }
        return choices
    }

    /// A policy as words rather than as the wire's hyphenated spelling.
    private static func spelled(_ policy: String?) -> String {
        guard let policy else { return "unreported" }
        return policy.replacingOccurrences(of: "-", with: " ")
    }
}
