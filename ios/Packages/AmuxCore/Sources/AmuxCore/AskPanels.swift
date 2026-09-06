import Foundation

/// What an agent is waiting to be told, read off the ask the core sent.
///
/// The two providers ask in different words and offer different choices, and
/// nothing here flattens them into one: Claude asks for permission, asks a
/// question or offers a plan, and Codex asks for a decision from the list of
/// decisions Codex itself named. A panel drawn from a shared invention would
/// put words in a provider's mouth and offer buttons the far side would
/// refuse.
///
/// An ask this build cannot read stays as ``Kind/unreadable``, which is drawn,
/// because a screen that silently omits the one thing an agent is waiting for
/// is worse than one that admits it cannot read it.
public struct AskPanel: Identifiable, Equatable, Sendable {
    public let id: String
    /// What an answer to this ask is addressed to.
    public let address: Address
    public let kind: Kind

    public init(id: String, address: Address, kind: Kind) {
        self.id = id
        self.address = address
        self.kind = kind
    }

    /// The handle an answer carries. Each layer names its asks its own way and
    /// an answer is only addressable by that name.
    public enum Address: Equatable, Sendable {
        /// Claude's per-window ask number.
        case claude(ask: Int)
        /// Codex's opaque request identifier, carried back exactly as it came.
        case codex(request: JSONValue)
    }

    public enum Kind: Equatable, Sendable {
        /// Permission for one tool use.
        case permission(Permission)
        /// A plan to judge. Claude sends it as a permission whose payload is
        /// the plan, and it stays that ask here: what changes is the panel.
        case plan(Plan)
        /// One or more questions, each with its own options.
        case question([Question])
        /// Codex asking for one of its own decisions.
        case approval(Approval)
        /// An ask shape this build has no panel for.
        case unreadable(label: String)
    }

    // MARK: - Permission

    public struct Permission: Equatable, Sendable {
        /// What it wants, in one line: "Wants to run a command".
        public let headline: String
        /// The thing itself, verbatim — the command, the path. Never
        /// paraphrased: a person approving a command is approving these
        /// characters.
        public let subject: String
        /// Whether the subject is something a machine will read, and so is set
        /// in mono.
        public let literal: Bool
        /// Why it wants to, in the agent's own words. Absent where the agent
        /// did not say.
        public let purpose: String?
        /// The standing grant the host offered, when it offered one.
        public let scope: Scope?
        /// Set when this build must not offer any answer at all, and why.
        public let unanswerable: String?

        public init(
            headline: String, subject: String, literal: Bool, purpose: String?,
            scope: Scope?, unanswerable: String?
        ) {
            self.headline = headline
            self.subject = subject
            self.literal = literal
            self.purpose = purpose
            self.scope = scope
            self.unanswerable = unanswerable
        }
    }

    /// A grant that outlives this one answer, described by whatever the host
    /// actually offered rather than by what the button would like to say.
    public struct Scope: Equatable, Sendable {
        /// "Always allow access", or the host's own wording for its suggestion.
        public let title: String
        /// Where it applies, when the suggestion names somewhere.
        public let directory: String?
        /// Which of the host's suggestions this is.
        public let suggestion: Int

        public init(title: String, directory: String?, suggestion: Int) {
            self.title = title
            self.directory = directory
            self.suggestion = suggestion
        }
    }

    // MARK: - Plan

    public struct Plan: Equatable, Sendable {
        /// Free-form markdown, as the agent wrote it.
        public let markdown: String
        /// Where the agent also wrote it down, when it did.
        public let path: String?

        public init(markdown: String, path: String?) {
            self.markdown = markdown
            self.path = path
        }
    }

    // MARK: - Question

    public struct Question: Equatable, Sendable, Identifiable {
        public let id: Int
        public let header: String?
        public let prompt: String
        /// Whether several answers may be chosen at once. A question that
        /// takes several answers cannot be answered by one tap, so it collects
        /// and then sends.
        public let multiSelect: Bool
        public let options: [Option]

        public init(
            id: Int, header: String?, prompt: String, multiSelect: Bool, options: [Option]
        ) {
            self.id = id
            self.header = header
            self.prompt = prompt
            self.multiSelect = multiSelect
            self.options = options
        }

        public struct Option: Equatable, Sendable, Identifiable {
            public let id: Int
            public let label: String
            public let description: String?

            public init(id: Int, label: String, description: String?) {
                self.id = id
                self.label = label
                self.description = description
            }
        }
    }

    // MARK: - Codex

    public struct Approval: Equatable, Sendable {
        public let headline: String
        /// The command, the files, the tool — whatever this approval is about.
        public let subject: String?
        /// Where it would happen.
        public let place: String?
        /// Why Codex is asking rather than just doing it.
        public let reason: String?
        /// Codex's own choices, in the order Codex offered them.
        public let choices: [Choice]

        public init(
            headline: String, subject: String?, place: String?, reason: String?,
            choices: [Choice]
        ) {
            self.headline = headline
            self.subject = subject
            self.place = place
            self.reason = reason
            self.choices = choices
        }
    }

    /// One of Codex's decisions. A choice this build cannot carry is still
    /// listed and cannot be pressed: hiding it would misrepresent what Codex
    /// offered, and offering it would send something the backend refuses.
    public struct Choice: Equatable, Sendable, Identifiable {
        public let id: Int
        public let label: String
        /// The decision word this sends, or nil where this build cannot send
        /// it.
        public let decision: String?

        public init(id: Int, label: String, decision: String?) {
            self.id = id
            self.label = label
            self.decision = decision
        }
    }
}

/// What a person decided, in the shape the layer that asked will take.
public enum AskDecision: Equatable, Sendable {
    case allowOnce
    case allowScoped(suggestion: Int)
    case deny(feedback: String?)
    /// Approve a plan, with edits still asked about one at a time.
    case approvePlan
    case sendPlanBack(feedback: String)
    /// One reply per question, in the order the questions were asked.
    case answered([AskDecision.Reply])
    /// One of Codex's own decisions, by its wire word.
    case decided(String)

    public struct Reply: Equatable, Sendable {
        public let selected: [Int]
        public let other: String?

        public init(selected: [Int], other: String? = nil) {
            self.selected = selected
            self.other = other
        }
    }
}

/// What has been chosen on a question panel so far.
///
/// The layer refuses a response with a question missing, so a panel with more
/// than one question — or one that takes several answers — has to collect
/// before it sends. The rule about when a tap is already the whole answer
/// lives here rather than in the view, because it is a fact about what the
/// layer accepts.
public struct QuestionDraft: Equatable, Sendable {
    private var chosen: [Int: Set<Int>] = [:]

    public init() {}

    /// Whether one tap finishes the whole thing. One question that takes one
    /// answer is answered by tapping it: a confirmation step after a choice
    /// that is already a tap says nothing.
    public static func immediate(_ questions: [AskPanel.Question]) -> Bool {
        questions.count == 1 && questions[0].multiSelect == false
    }

    public func picked(_ option: Int, of question: Int) -> Bool {
        chosen[question]?.contains(option) ?? false
    }

    /// Take or drop one option. A question that takes one answer replaces
    /// what was chosen; one that takes several accumulates.
    public mutating func toggle(_ option: Int, of question: AskPanel.Question) {
        var picked = chosen[question.id] ?? []
        guard question.multiSelect else {
            chosen[question.id] = picked.contains(option) ? [] : [option]
            return
        }
        if picked.contains(option) { picked.remove(option) } else { picked.insert(option) }
        chosen[question.id] = picked
    }

    /// Every question has an answer, which is what the layer requires before
    /// it will take any of them.
    public func isComplete(for questions: [AskPanel.Question]) -> Bool {
        questions.allSatisfy { !(chosen[$0.id] ?? []).isEmpty }
    }

    public func replies(for questions: [AskPanel.Question]) -> [AskDecision.Reply] {
        questions.map { AskDecision.Reply(selected: (chosen[$0.id] ?? []).sorted()) }
    }
}

extension AskPanel {
    /// The shared command that carries this decision, in the core's own
    /// vocabulary. Nil where the decision does not fit the ask, which is a
    /// programming mistake rather than something a person can do.
    public func command(_ decision: AskDecision, agent: AgentId) -> JSONValue? {
        switch address {
        case .claude(let ask):
            guard let answer = Self.claudeAnswer(decision) else { return nil }
            return .object([
                "command": .string("claude"),
                "claude_command": .string("answer_ask"),
                "agent": .string(agent.description),
                "ask": .int(ask),
                "answer": answer,
            ])
        case .codex(let request):
            guard case .decided(let word) = decision else { return nil }
            return .object([
                "command": .string("codex"),
                "codex_command": .string("answer"),
                "agent": .string(agent.description),
                "request_id": request,
                "decision": .string(word),
            ])
        }
    }

    private static func claudeAnswer(_ decision: AskDecision) -> JSONValue? {
        switch decision {
        case .allowOnce:
            .object(["answer": .string("permission"), "permission": .string("allow_once")])
        case .allowScoped(let suggestion):
            .object([
                "answer": .string("permission"), "permission": .string("allow_scoped"),
                "suggestion": .int(suggestion),
            ])
        case .deny(let feedback):
            .object([
                "answer": .string("permission"), "permission": .string("deny"),
                "feedback": feedback.map(JSONValue.string) ?? .null,
            ])
        case .approvePlan:
            .object(["answer": .string("plan"), "plan": .string("approve_manual")])
        case .sendPlanBack(let feedback):
            .object([
                "answer": .string("plan"), "plan": .string("request_changes"),
                "feedback": .string(feedback),
            ])
        case .answered(let replies):
            .object([
                "answer": .string("question"),
                "answers": .array(replies.map { reply in
                    .object([
                        "selected": .array(reply.selected.map(JSONValue.int)),
                        "other": reply.other.map(JSONValue.string) ?? .null,
                    ])
                }),
            ])
        case .decided:
            nil
        }
    }
}

extension Ask {
    /// The panel this ask is drawn as, or nil where the ask has already been
    /// answered and is only waiting for the layer to say so.
    ///
    /// An optimistically answered ask draws nothing: the person has decided,
    /// and putting the same question back in front of them while the answer
    /// travels would invite them to answer it twice.
    public var panel: AskPanel? {
        switch layer {
        case .claudePty: claudePanel
        case .codex: codexPanel
        }
    }

    /// An ask is drawn while it is waiting and again when the answer never
    /// left: the core resurfaces a send that failed rather than leaving a
    /// spinner, and a person whose tap raced the session has to be able to
    /// make it again. Only an answer in flight draws nothing.
    private var claudePanel: AskPanel? {
        guard ["pending", "send_failed"].contains(body["state"]?["state"]?.stringValue ?? "")
        else { return nil }
        guard let ask = body["id"]?.intValue else { return nil }
        let id = "claude:\(ask)"
        let kind = body["kind"]
        switch kind?["ask"]?.stringValue {
        case "permission":
            let invocation = kind?["invocation"]
            if invocation?["tool"]?.stringValue == "plan" {
                return AskPanel(id: id, address: .claude(ask: ask), kind: .plan(AskPanel.Plan(
                    markdown: invocation?["plan"]?.stringValue ?? "",
                    path: invocation?["plan_file_path"]?.stringValue)))
            }
            return AskPanel(
                id: id, address: .claude(ask: ask),
                kind: .permission(Self.permission(
                    tool: kind?["tool_name"]?.stringValue, invocation: invocation,
                    suggestions: kind?["suggestions"]?.arrayValue ?? [])))
        case "question":
            let questions = (kind?["questions"]?.arrayValue ?? []).enumerated().map {
                index, question in
                AskPanel.Question(
                    id: index,
                    header: question["header"]?.stringValue,
                    prompt: question["question"]?.stringValue ?? "",
                    multiSelect: question["multi_select"]?.boolValue ?? false,
                    options: (question["options"]?.arrayValue ?? []).enumerated().map {
                        AskPanel.Question.Option(
                            id: $0.offset, label: $0.element["label"]?.stringValue ?? "",
                            description: $0.element["description"]?.stringValue)
                    })
            }
            return AskPanel(id: id, address: .claude(ask: ask), kind: .question(questions))
        case let other:
            return AskPanel(
                id: id, address: .claude(ask: ask),
                kind: .unreadable(label: other ?? "an ask with no kind"))
        }
    }

    /// What Claude wants, said in the shape of the tool it wants to use.
    ///
    /// The verbatim subject is the tool's own argument — the command, the path
    /// — because that is what a person is being asked to approve. A headline
    /// that summarised it would be the phone's opinion of a thing only the
    /// characters themselves state.
    private static func permission(
        tool: String?, invocation: JSONValue?, suggestions: [JSONValue]
    ) -> AskPanel.Permission {
        let name = tool ?? "a tool"
        let headline: String
        let subject: String
        var literal = true
        switch invocation?["tool"]?.stringValue {
        case "bash":
            headline = "Wants to run a command"
            subject = invocation?["command"]?.stringValue ?? name
        case "edit":
            headline = "Wants to edit a file"
            subject = invocation?["file_path"]?.stringValue ?? name
        case "write":
            headline = "Wants to write a file"
            subject = invocation?["file_path"]?.stringValue ?? name
        case "read":
            headline = "Wants to read a file"
            subject = invocation?["file_path"]?.stringValue ?? name
        case "query":
            headline = "Wants to search"
            subject = invocation?["text"]?.stringValue ?? name
        case "amux_send":
            headline = "Wants to message another agent"
            subject = invocation?["to"]?.stringValue ?? name
        case "task":
            headline = "Wants to start another agent"
            subject = invocation?["description"]?.stringValue ?? name
            literal = false
        default:
            headline = "Wants to use \(name)"
            subject = name
        }
        return AskPanel.Permission(
            headline: headline, subject: subject, literal: literal,
            purpose: invocation?["description"]?.stringValue,
            scope: scope(suggestions),
            unanswerable: Self.unanswerable(suggestions))
    }

    /// The standing grant to offer, from the host's own suggestion.
    ///
    /// Claude generates its permission menu from these suggestions, so what
    /// this row says and what pressing it grants are the same fact. A row that
    /// promised to always allow the command while sending a directory grant
    /// would be a lie about what the button does.
    private static func scope(_ suggestions: [JSONValue]) -> AskPanel.Scope? {
        guard suggestions.count == 1, let suggestion = suggestions.first else { return nil }
        let directories = (suggestion["directories"]?.arrayValue ?? []).compactMap(\.stringValue)
        if suggestion["kind"]?.stringValue == "add_directories", !directories.isEmpty {
            return AskPanel.Scope(
                title: "Always allow access",
                directory: directories.joined(separator: ", "), suggestion: 0)
        }
        if suggestion["destination"]?.stringValue == "session" {
            return AskPanel.Scope(
                title: "Always allow for this session", directory: nil, suggestion: 0)
        }
        return AskPanel.Scope(title: "Apply the suggested rule", directory: nil, suggestion: 0)
    }

    /// Why nothing may be answered here.
    ///
    /// Claude's permission menu is generated from the host's suggestions, and
    /// only the one-suggestion shape has been checked against a real Claude.
    /// On any other shape the core refuses every answer, so the panel must not
    /// offer one: a button that is always refused is worse than a sentence
    /// saying where to go instead.
    private static func unanswerable(_ suggestions: [JSONValue]) -> String? {
        guard suggestions.count != 1 else { return nil }
        return "This build cannot answer this menu. Attach to the session to answer it there."
    }

    private var codexPanel: AskPanel? {
        guard let request = body["request_id"] else { return nil }
        let context = body["context"]
        let id = "codex:\(context?["item_id"]?.stringValue ?? String(body["seq"]?.intValue ?? 0))"
        let choices = (body["actions"]?.arrayValue ?? []).enumerated().map { index, action in
            AskPanel.Choice(
                id: index, label: Self.choiceLabel(action["meaning"]),
                decision: action["meaning"]?["meaning"]?.stringValue == "scalar"
                    ? action["meaning"]?["decision"]?.stringValue : nil)
        }
        let approval: AskPanel.Approval
        switch context?["ask"]?.stringValue {
        case "command":
            approval = AskPanel.Approval(
                headline: "Wants to run a command",
                subject: context?["command"]?.stringValue,
                place: context?["cwd"]?.stringValue,
                reason: context?["reason"]?.stringValue, choices: choices)
        case "file_change":
            let changed = (context?["changes"]?.arrayValue ?? []).count
            approval = AskPanel.Approval(
                headline: "Wants to change files",
                subject: changed == 1 ? "1 file" : "\(changed) files", place: nil,
                reason: context?["reason"]?.stringValue, choices: choices)
        case "permissions":
            approval = AskPanel.Approval(
                headline: "Wants more permission", subject: nil, place: nil,
                reason: context?["reason"]?.stringValue, choices: choices)
        case "dynamic_tool":
            approval = AskPanel.Approval(
                headline: "Wants to use a tool",
                subject: context?["tool"]?.stringValue, place: nil, reason: nil,
                choices: choices)
        case let other:
            return AskPanel(
                id: id, address: .codex(request: request),
                kind: .unreadable(label: other ?? "an approval with no kind"))
        }
        return AskPanel(id: id, address: .codex(request: request), kind: .approval(approval))
    }

    /// Codex's own word for a choice, spelled the way a person reads rather
    /// than the way the wire does.
    private static func choiceLabel(_ meaning: JSONValue?) -> String {
        switch meaning?["meaning"]?.stringValue {
        case "scalar":
            switch meaning?["decision"]?.stringValue {
            case "accept": "Accept"
            case "acceptForSession": "Accept for Session"
            case "decline": "Decline"
            case "cancel": "Cancel"
            case let other: other ?? "A decision with no name"
            }
        case "accept_with_execpolicy_amendment":
            meaning?["matches_proposal"]?.boolValue == true
                ? "Accept and Allow Similar" : "Accept with a Rule Change"
        case "apply_network_policy_amendment":
            "Change the Network Policy"
        case "empty_object":
            "A choice with nothing in it"
        case "unknown_object":
            meaning?["kind"]?.stringValue ?? "A choice this build cannot read"
        case "unknown_scalar":
            meaning?["detail"]?.stringValue ?? "A choice this build cannot read"
        case let other:
            other ?? "A choice with no meaning"
        }
    }
}

extension Array where Element == Ask {
    /// The one panel to draw. Asks queue in arrival order and the head of the
    /// queue is what is in front of the person; drawing the rest as well would
    /// ask them to answer questions the agent is not waiting on yet.
    public var panel: AskPanel? {
        lazy.compactMap(\.panel).first
    }
}
