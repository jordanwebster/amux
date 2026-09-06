import Foundation

/// What the transcript draws, read off the rows the core sends.
///
/// The three layers each have their own row vocabulary and keep it: nothing
/// here re-decodes one layer as another. What they share is what a reader
/// sees — somebody asked, something was said, something was read, changed,
/// run or refused — so the projection names those and each layer's reader
/// maps its own rows onto them. A row this build cannot read stays as
/// ``TranscriptRow/Kind/unreadable``, which is drawn, because a transcript
/// that silently loses rows is worse than one that admits it saw something
/// it could not name.
public struct TranscriptRow: Identifiable, Equatable, Sendable {
    public let id: String
    public let layer: FeedEntry.Layer
    public let kind: Kind

    public init(id: String, layer: FeedEntry.Layer, kind: Kind) {
        self.id = id
        self.layer = layer
        self.kind = kind
    }

    public enum Kind: Equatable, Sendable {
        /// What the person asked for. Drawn as a surface, not a rail row.
        case prompt(text: String)
        /// What the agent said, in markdown, still being written or done.
        case prose(markdown: String, open: Bool)
        /// `~ thought for 8s`, and whether the content was withheld.
        case thinking(seconds: Int?, redacted: Bool)
        /// A run of reads and searches, folded to its counts and the last
        /// thing it touched. The rows themselves are kept so the fold opens.
        case exploration(reads: Int, searches: Int, last: String, inside: [Detail])
        /// A file that changed, as its path and the arithmetic.
        case edit(path: String, added: Int, removed: Int)
        /// A file written whole, with what the layer said about it.
        case wrote(path: String, meta: String?)
        /// A command, its meta on the trailing edge, and its output if any.
        case ran(command: String, meta: String?, output: Output?)
        /// A tool this build has no special line for: its name and one fact.
        case tool(name: String, detail: String?, meta: String?)
        /// Refused. The glyph carries the accent; the words stay ink.
        case denied(label: String, reason: String?)
        /// Ran and failed.
        case failed(label: String, message: String?)
        /// The person cut it off.
        case interrupted(toolUse: Bool)
        /// The provider itself errored.
        case providerError(message: String)
        /// A subagent started or finished under this one.
        case subagent(name: String, kind: String?, state: String?)
        /// A plan that was judged, with the document that was judged kept
        /// beside the verdict.
        case planVerdict(PlanVerdict)
        /// A message between two amux agents, collapsed to one line until
        /// it is opened.
        case agentMessage(from: String, text: String, outbound: Bool, note: String?)
        /// A session that ended.
        case exit(text: String)
        /// The rule that closes a turn.
        case turnEnd(meta: String?)
        /// The rule that marks history being compacted away.
        case compaction(before: UInt64?, after: UInt64?)
        /// A row shape this build does not know, named by whatever the row
        /// did say about itself.
        case unreadable(label: String)
    }

    /// What was decided about a plan, and the plan it was decided about.
    ///
    /// The document travels with the verdict rather than being fetched when
    /// somebody reopens the decision. A plan lives in the agent's own words at
    /// the moment it was offered; the file it was also written to can be
    /// edited afterwards, and re-reading it later would show a reader
    /// something other than what they approved.
    public struct PlanVerdict: Equatable, Sendable {
        public enum Decision: Equatable, Sendable {
            case approved
            /// Sent back to be reworked. `reason` is the layer's own typed
            /// denial word where it stated one.
            case sentBack(reason: String?)
        }

        public let decision: Decision
        /// The plan as the agent wrote it. Absent where the layer offered a
        /// plan it did not carry, which is drawn as a verdict with nothing to
        /// reopen rather than as a blank document.
        public let markdown: String?
        /// Where the agent also wrote it down, when it did.
        public let path: String?

        public init(decision: Decision, markdown: String?, path: String?) {
            self.decision = decision
            self.markdown = markdown
            self.path = path
        }
    }

    /// One line inside a folded run.
    public struct Detail: Equatable, Sendable, Identifiable {
        public let id: String
        public let verb: String
        public let subject: String

        public init(id: String, verb: String, subject: String) {
            self.id = id
            self.verb = verb
            self.subject = subject
        }
    }

    /// Command output, kept to its head with the rest counted rather than
    /// dropped.
    public struct Output: Equatable, Sendable {
        public let head: String
        /// Lines the head does not show. Zero when the head is all of it.
        public let hidden: Int

        public init(head: String, hidden: Int) {
            self.head = head
            self.hidden = hidden
        }
    }

    /// Whether this row hangs off the rail or interrupts it.
    ///
    /// The rail is the spine of a turn's work. A prompt, the agent's prose and
    /// the rules that close a turn are not work, so they break the rail and
    /// run the full width; everything else hangs off it.
    public var onRail: Bool {
        switch kind {
        case .prompt, .prose, .turnEnd, .compaction: false
        default: true
        }
    }
}

extension Array where Element == FeedEntry {
    /// The rows as the transcript draws them, runs already folded.
    ///
    /// Folding happens here rather than in the view because a run's counts are
    /// a fact about the feed, and a view that counted its own children would
    /// be reading layout to decide meaning.
    public func transcriptRows() -> [TranscriptRow] {
        var rows: [TranscriptRow] = []
        var run: [(id: String, layer: FeedEntry.Layer, search: Bool, subject: String)] = []

        func flushRun() {
            guard let first = run.first else { return }
            let reads = run.filter { !$0.search }.count
            let searches = run.filter(\.search).count
            let details = run.map {
                TranscriptRow.Detail(
                    id: $0.id, verb: $0.search ? "Searched" : "Read", subject: $0.subject)
            }
            rows.append(TranscriptRow(
                id: first.id, layer: first.layer,
                kind: .exploration(
                    reads: reads, searches: searches,
                    last: run[run.count - 1].subject, inside: details)))
            run.removeAll(keepingCapacity: true)
        }

        for entry in self {
            if let look = entry.exploration {
                if !run.isEmpty && !look.groups { flushRun() }
                run.append((entry.id, entry.layer, look.search, look.subject))
                continue
            }
            flushRun()
            rows.append(TranscriptRow(id: entry.id, layer: entry.layer, kind: entry.kind))
        }
        flushRun()
        return rows
    }
}

extension FeedEntry {
    /// This row read as a read or a search, when that is what it is, and
    /// whether the core said it continues the run before it.
    ///
    /// Whether two looks belong to one run is `group_with_previous`, a fact
    /// the core states; a client that decided grouping for itself would fold
    /// two runs the core deliberately kept apart. Codex has no exploration
    /// vocabulary of its own — every look is work — so it folds nothing.
    var exploration: (search: Bool, subject: String, groups: Bool)? {
        switch layer {
        case .claudePty, .claudeSdk:
            let tool = layer == .claudeSdk
                ? (row["kind"]?["kind"]?.stringValue == "tool" ? row["kind"]?["entry"] : nil)
                : (row["kind"]?["entry"]?.stringValue == "tool" ? row["kind"] : nil)
            guard let tool, let invocation = tool["invocation"] else { return nil }
            let groups = tool["group_with_previous"]?.boolValue ?? false
            switch invocation["tool"]?.stringValue {
            case "read": return (false, invocation["file_path"]?.stringValue ?? "a file", groups)
            case "query": return (true, invocation["text"]?.stringValue ?? "the tree", groups)
            default: return nil
            }
        case .codex:
            return nil
        }
    }

    /// This row's kind, read in its own layer's vocabulary.
    var kind: TranscriptRow.Kind {
        switch layer {
        case .claudePty: claudePtyKind
        case .claudeSdk: claudeSdkKind
        case .codex: codexKind
        }
    }
}

// MARK: - Claude, over a terminal

extension FeedEntry {
    private var body: JSONValue? { row["kind"] }

    fileprivate var claudePtyKind: TranscriptRow.Kind {
        guard let body else { return .unreadable(label: "a row with no kind") }
        switch body["entry"]?.stringValue {
        case "prompt":
            return .prompt(text: body["text"]?.stringValue ?? "")
        case "message":
            return .prose(
                markdown: body["segments"]?.arrayValue?.compactMap(\.stringValue)
                    .joined(separator: "\n\n") ?? "",
                open: body["finality"]?["finality"]?.stringValue == "open")
        case "thinking":
            return .thinking(
                seconds: (body["duration_ms"]?.intValue).map { max(1, $0 / 1000) },
                redacted: body["redacted"]?.boolValue ?? false)
        case "turn":
            return .turnEnd(meta: Self.duration(body["duration"]))
        case "compaction":
            return .compaction(
                before: body["pre_tokens"]?.intValue.map(UInt64.init),
                after: body["post_tokens"]?.intValue.map(UInt64.init))
        case "compact_summary":
            return .prose(markdown: body["text"]?.stringValue ?? "", open: false)
        case "tool":
            return Self.claudeTool(body)
        case "task_notification":
            return .subagent(
                name: body["text"]?.stringValue ?? "a subagent", kind: nil, state: "finished")
        case "agent_message":
            return Self.carried(body)
        case "interruption":
            return .interrupted(toolUse: body["kind"]?.stringValue == "tool_use")
        case "api_error":
            return .providerError(
                message: body["text"]?.stringValue
                    ?? body["error"]?.stringValue ?? "the provider returned an error")
        case "unrecognized":
            return .unreadable(label: Self.unreadableLabel(body))
        case let other:
            return .unreadable(label: other ?? "a row with no kind")
        }
    }

    /// One Claude tool use, as the line a reader wants: what it did, to what,
    /// and what came back.
    private static func claudeTool(_ body: JSONValue) -> TranscriptRow.Kind {
        let invocation = body["invocation"]
        let outcome = body["outcome"]
        let name = body["name"]?.stringValue ?? "a tool"
        let label = toolLabel(invocation, name: name)

        // A judged plan is a decision somebody made, not a tool that ran, and
        // it is read before the ordinary outcomes so that approving one does
        // not arrive in the feed as a nameless tool with no output.
        if invocation?["tool"]?.stringValue == "plan",
           let verdict = planVerdict(invocation, outcome) {
            return .planVerdict(verdict)
        }

        switch outcome?["outcome"]?.stringValue {
        case "denied":
            return .denied(label: label, reason: deniedReason(outcome?["kind"]?.stringValue))
        case "failed":
            return .failed(label: label, message: outcome?["message"]?.stringValue)
        case "pending", .none:
            return running(invocation, name: name, label: label)
        default:
            break
        }

        let facts = outcome?["facts"]
        switch facts?["facts"]?.stringValue {
        case "edit":
            return .edit(
                path: facts?["file_path"]?.stringValue ?? label,
                added: facts?["added"]?.intValue ?? 0,
                removed: facts?["removed"]?.intValue ?? 0)
        case "task_completed":
            return .subagent(
                name: facts?["agent_id"]?.stringValue ?? label,
                kind: invocation?["subagent_type"]?.stringValue,
                state: "finished")
        case "task_launched":
            return .subagent(
                name: facts?["agent_id"]?.stringValue ?? label,
                kind: invocation?["subagent_type"]?.stringValue,
                state: nil)
        default:
            break
        }

        let output = facts.flatMap(outputHead)
        switch invocation?["tool"]?.stringValue {
        case "bash":
            return .ran(
                command: invocation?["command"]?.stringValue ?? label,
                meta: invocation?["description"]?.stringValue, output: output)
        case "write":
            return .wrote(path: label, meta: output?.head)
        case "task":
            return .subagent(
                name: label, kind: invocation?["subagent_type"]?.stringValue, state: nil)
        case "amux_send":
            return .agentMessage(
                from: invocation?["to"]?.stringValue ?? "another agent",
                text: invocation?["text"]?.stringValue ?? "", outbound: true, note: nil)
        default:
            return .tool(name: name, detail: label, meta: output?.head)
        }
    }

    /// What was decided about a plan, or nothing while it is still being
    /// decided.
    ///
    /// Claude records the decision on the ExitPlanMode tool use itself: a
    /// result means it was approved, a typed denial means it was sent back to
    /// be reworked. A plan nobody has answered yet is not a verdict — the ask
    /// is still on the screen — and a tool that failed is a failure rather
    /// than a judgement, so neither is claimed as one.
    private static func planVerdict(
        _ invocation: JSONValue?, _ outcome: JSONValue?
    ) -> TranscriptRow.PlanVerdict? {
        let markdown = invocation?["plan"]?.stringValue
        // The layer states the sidecar path on the outcome once it has one and
        // on the invocation before that; either is the same file.
        let path = outcome?["facts"]?["plan_file_path"]?.stringValue
            ?? invocation?["plan_file_path"]?.stringValue
        switch outcome?["outcome"]?.stringValue {
        case "success":
            return TranscriptRow.PlanVerdict(
                decision: .approved, markdown: markdown, path: path)
        case "denied":
            return TranscriptRow.PlanVerdict(
                decision: .sentBack(reason: deniedReason(outcome?["kind"]?.stringValue)),
                markdown: markdown, path: path)
        default:
            return nil
        }
    }

    private static func running(
        _ invocation: JSONValue?, name: String, label: String
    ) -> TranscriptRow.Kind {
        switch invocation?["tool"]?.stringValue {
        case "bash":
            return .ran(
                command: invocation?["command"]?.stringValue ?? label,
                meta: "running", output: nil)
        default:
            return .tool(name: name, detail: label, meta: "running")
        }
    }

    /// The one fact a tool line is about, per family.
    private static func toolLabel(_ invocation: JSONValue?, name: String) -> String {
        switch invocation?["tool"]?.stringValue {
        case "edit", "write", "read":
            return invocation?["file_path"]?.stringValue ?? name
        case "bash":
            return invocation?["command"]?.stringValue ?? name
        case "query":
            return invocation?["text"]?.stringValue ?? name
        case "task":
            return invocation?["description"]?.stringValue ?? name
        case "amux_send":
            return invocation?["to"]?.stringValue ?? name
        default:
            return name
        }
    }

    /// A row amux itself carried into this transcript.
    ///
    /// Three of the carrier's kinds are not messages at all: a sender that
    /// finished, a sender whose session ended, and a kind this build does not
    /// know. The first two are one line about somebody else and have no body to
    /// open, so they are drawn as events rather than as a collapsed quote with
    /// nothing inside it.
    private static func carried(_ body: JSONValue) -> TranscriptRow.Kind {
        let from = body["from"]?.stringValue ?? "another agent"
        // The envelope kind is a tagged object, not a bare string: the core
        // states it as `{"message_kind": …}` and a kind it does not know
        // carries the label the carrier wrote beside it.
        let envelope = body["kind"]
        switch envelope?["message_kind"]?.stringValue {
        case "exited":
            return .exit(text: "\(from) ended its session")
        case "completed":
            return .exit(text: "\(from) finished its turn")
        case "message", "unstated", .none:
            return .agentMessage(
                from: from, text: body["text"]?.stringValue ?? "", outbound: false, note: nil)
        case "other":
            return .agentMessage(
                from: from, text: body["text"]?.stringValue ?? "", outbound: false,
                note: envelope?["label"]?.stringValue ?? "an unknown kind")
        case .some(let other):
            return .agentMessage(
                from: from, text: body["text"]?.stringValue ?? "", outbound: false, note: other)
        }
    }

    /// A denial's typed kind said in words. Never sniffed from an error
    /// string: the core states the kind or states nothing.
    private static func deniedReason(_ kind: String?) -> String? {
        switch kind {
        case "user_reject": "You said no"
        case "user_abort": "You interrupted it"
        case "permission_denied": "Outside what it may touch"
        case .some(let other): other.replacingOccurrences(of: "_", with: " ")
        case .none: nil
        }
    }

    private static func outputHead(_ facts: JSONValue) -> TranscriptRow.Output? {
        guard facts["facts"]?.stringValue == "output",
              let head = facts["head"]?.stringValue, !head.isEmpty else { return nil }
        let lines = head.split(separator: "\n", omittingEmptySubsequences: false)
        let shown = lines.prefix(2).joined(separator: "\n")
        let truncated = facts["truncated"]?.boolValue ?? false
        // A truncated head cannot say how many lines it is missing, so it says
        // "more" without a number rather than inventing one; the count is only
        // claimed where every line is actually in hand.
        return TranscriptRow.Output(
            head: shown, hidden: truncated ? -1 : max(0, lines.count - 2))
    }

    private static func duration(_ value: JSONValue?) -> String? {
        guard let ms = value?["ms"]?.intValue else { return nil }
        return ms >= 60_000 ? "\(ms / 60_000)m \((ms % 60_000) / 1000)s" : "\(ms / 1000)s"
    }

    private static func unreadableLabel(_ body: JSONValue) -> String {
        [body["row_type"]?.stringValue, body["detail"]?.stringValue, body["label"]?.stringValue]
            .compactMap { $0 }.first ?? "a row this build cannot read"
    }
}

// MARK: - Claude, over the SDK

extension FeedEntry {
    /// The SDK's rows, in the SDK's own shape.
    ///
    /// The layer tags its kind on the row and carries the entry beside it,
    /// where the terminal layer inlines it; nothing here re-reads an SDK row
    /// as a terminal one, because the two vocabularies only look alike.
    fileprivate var claudeSdkKind: TranscriptRow.Kind {
        guard let body = row["kind"], let inner = body["entry"] else {
            return .unreadable(label: "an SDK row with no entry")
        }
        switch body["kind"]?.stringValue {
        case "prompt":
            return .prompt(text: inner["text"]?.stringValue ?? "")
        case "message":
            return .prose(
                markdown: inner["text"]?.stringValue ?? "",
                open: inner["finality"]?.stringValue == "streaming")
        case "thinking":
            return .thinking(seconds: nil, redacted: inner["redacted"]?.boolValue ?? false)
        case "tool":
            return Self.sdkTool(inner)
        case "task":
            return .subagent(
                name: inner["description"]?.stringValue ?? "a subagent",
                kind: inner["subagent_type"]?.stringValue,
                state: inner["state"]?.stringValue)
        case "turn":
            let outcome = inner["outcome"]?.stringValue
            if inner["is_error"]?.boolValue == true {
                return .providerError(message: inner["errors"]?.arrayValue?
                    .compactMap(\.stringValue).first ?? outcome ?? "the turn failed")
            }
            return .turnEnd(meta: (inner["duration_ms"]?.intValue).map { "\($0 / 1000)s" })
        case "compaction":
            return .compaction(
                before: inner["pre_tokens"]?.intValue.map(UInt64.init),
                after: inner["post_tokens"]?.intValue.map(UInt64.init))
        case "agent_message":
            return Self.carried(inner)
        case "status":
            return .tool(
                name: inner["status"]?.stringValue ?? "status", detail: nil, meta: nil)
        case "boundary":
            return Self.sdkBoundary(inner)
        case "unrecognized":
            return .unreadable(label: Self.unreadableLabel(inner))
        case let other:
            return .unreadable(label: other ?? "an SDK row with no kind")
        }
    }

    private static func sdkTool(_ entry: JSONValue) -> TranscriptRow.Kind {
        let invocation = entry["invocation"]
        let name = entry["name"]?.stringValue ?? "a tool"
        let label = toolLabel(invocation, name: name)
        let result = entry["result"]

        if result?["is_error"]?.boolValue == true {
            return .failed(label: label, message: result?["text"]?.stringValue)
        }
        if let edit = result?["edit"] {
            return .edit(
                path: edit["file_path"]?.stringValue ?? label,
                added: edit["added"]?.intValue ?? 0,
                removed: edit["removed"]?.intValue ?? 0)
        }
        guard result != nil else {
            return running(invocation, name: name, label: label)
        }
        let text = result?["text"]?.stringValue ?? ""
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let output = text.isEmpty
            ? nil
            : TranscriptRow.Output(
                head: lines.prefix(2).joined(separator: "\n"),
                hidden: max(0, lines.count - 2))
        switch invocation?["tool"]?.stringValue {
        case "bash":
            return .ran(
                command: invocation?["command"]?.stringValue ?? label,
                meta: invocation?["description"]?.stringValue, output: output)
        case "write":
            return .wrote(path: label, meta: nil)
        case "amux_send":
            return .agentMessage(
                from: invocation?["to"]?.stringValue ?? "another agent",
                text: invocation?["text"]?.stringValue ?? "", outbound: true, note: nil)
        default:
            return .tool(name: name, detail: label, meta: output?.head)
        }
    }

    private static func sdkBoundary(_ entry: JSONValue) -> TranscriptRow.Kind {
        switch entry["boundary"]?.stringValue {
        case "ready":
            return .exit(text: entry["resumed"]?.boolValue == true
                ? "Resumed the session" : "Started a session")
        case "gap":
            return .unreadable(label: "a gap in the history")
        case "conversation_reset":
            return .exit(text: "The conversation was reset")
        case let other:
            return .unreadable(label: other ?? "a boundary with no kind")
        }
    }
}

// MARK: - Codex

extension FeedEntry {
    fileprivate var codexKind: TranscriptRow.Kind {
        guard let body = row["kind"] else { return .unreadable(label: "a row with no kind") }
        switch body["entry"]?.stringValue {
        case "prompt":
            return .prompt(text: body["content"]?.arrayValue?
                .compactMap { $0["value"]?.stringValue }.joined() ?? "")
        case "message":
            return .prose(
                markdown: body["text"]?.stringValue ?? "",
                open: body["finality"]?.stringValue == "open")
        case "reasoning":
            return .prose(
                markdown: body["summary"]?.arrayValue?.compactMap(\.stringValue)
                    .joined(separator: "\n\n") ?? body["text"]?.stringValue ?? "",
                open: body["finality"]?.stringValue == "open")
        case "work":
            return Self.codexWork(body)
        case "mcp_startup":
            return .tool(name: "Started", detail: "its tool servers", meta: nil)
        case "agent_message":
            return Self.carried(body)
        case "turn":
            switch body["status"]?["status"]?.stringValue {
            case "interrupted": return .interrupted(toolUse: false)
            case "failed":
                return .providerError(
                    message: body["status"]?["message"]?.stringValue ?? "the turn failed")
            default:
                return .turnEnd(meta: (body["token_usage"]?["total_tokens"]?.intValue)
                    .map { "\($0) tokens" })
            }
        case "boundary":
            switch body["boundary"]?.stringValue {
            case "compacted": return .compaction(before: nil, after: nil)
            case "resumed": return .exit(text: "Resumed the session")
            case "ready": return .exit(text: "Started a session")
            case "gap":
                return .unreadable(
                    label: body["reason"]?.stringValue ?? "a gap in the history")
            case let other: return .unreadable(label: other ?? "a boundary with no kind")
            }
        case "error":
            return .providerError(
                message: body["message"]?.stringValue ?? "the provider returned an error")
        case "unrecognized":
            return .unreadable(label: Self.unreadableLabel(body))
        case let other:
            return .unreadable(label: other ?? "a row with no kind")
        }
    }

    /// Codex says everything it does as work, so what kind of work it is
    /// decides which line this becomes.
    private static func codexWork(_ body: JSONValue) -> TranscriptRow.Kind {
        let work = body["kind"]
        let state = body["state"]?["state"]?.stringValue
        let output = codexOutput(body)

        if state == "denied" {
            return .denied(label: codexWorkLabel(work), reason: nil)
        }
        if state == "blocked_unsupported" {
            return .unreadable(label: codexWorkLabel(work))
        }
        if state == "done", body["state"]?["outcome"]?.stringValue == "failed" {
            return .failed(label: codexWorkLabel(work), message: output?.head)
        }

        switch work?["work"]?.stringValue {
        case "command":
            return .ran(
                command: work?["command"]?.stringValue ?? "a command",
                meta: state == "running" ? "running"
                    : (work?["exit_code"]?.intValue).map { "exit \($0)" },
                output: output)
        case "file_change":
            let changes = work?["changes"]?.arrayValue ?? []
            let path = changes.first?["path"]?.stringValue ?? "a file"
            return .wrote(
                path: path, meta: changes.count > 1 ? "and \(changes.count - 1) more" : nil)
        case "amux_send":
            return .agentMessage(
                from: work?["to"]?.stringValue ?? "another agent",
                text: work?["text"]?.stringValue ?? "", outbound: true, note: nil)
        case "web_search":
            return .tool(name: "Searched", detail: work?["query"]?.stringValue, meta: nil)
        case "plan":
            return .prose(markdown: work?["text"]?.stringValue ?? "", open: false)
        default:
            return .tool(
                name: codexWorkName(work), detail: codexWorkLabel(work), meta: output?.head)
        }
    }

    private static func codexWorkName(_ work: JSONValue?) -> String {
        switch work?["work"]?.stringValue {
        case "mcp_tool": work?["tool"]?.stringValue ?? "a server tool"
        case "amux_tool": work?["tool"]?.stringValue ?? "an amux tool"
        case "dynamic_tool": work?["tool"]?.stringValue ?? "a tool"
        case "unsupported_user_input": "A question this build cannot answer"
        case let other: other ?? "a tool"
        }
    }

    private static func codexWorkLabel(_ work: JSONValue?) -> String {
        switch work?["work"]?.stringValue {
        case "command": work?["command"]?.stringValue ?? "a command"
        case "file_change": work?["changes"]?.arrayValue?.first?["path"]?.stringValue ?? "a file"
        case "mcp_tool": work?["server"]?.stringValue ?? "a server"
        case "web_search": work?["query"]?.stringValue ?? "the web"
        case let other: other ?? "work"
        }
    }

    private static func codexOutput(_ body: JSONValue) -> TranscriptRow.Output? {
        let text = [body["stdout_head"]?.stringValue, body["stderr_head"]?.stringValue]
            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "\n")
        guard !text.isEmpty else { return nil }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        return TranscriptRow.Output(
            head: lines.prefix(2).joined(separator: "\n"),
            hidden: body["output_truncated"]?.boolValue == true ? -1 : max(0, lines.count - 2))
    }
}
