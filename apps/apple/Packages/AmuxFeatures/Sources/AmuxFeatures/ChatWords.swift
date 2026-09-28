import AmuxCore
import Foundation

/// How the phone words the chat's typed facts. The rows, the card and the
/// strip arrive as values; every sentence a person reads about them is
/// written here, so each can be read and tested apart from any view.
public enum ChatWords {
    // MARK: - Numbers

    /// "1m 42s", "8s", "4.2s", "850ms".
    public static func duration(_ ms: Int64) -> String {
        let ms = max(0, ms)
        if ms < 1_000 { return "\(ms)ms" }
        let secs = ms / 1_000
        if secs < 60 {
            if ms < 10_000, ms % 1_000 != 0 {
                return String(format: "%.1fs", Double(ms) / 1_000)
            }
            return "\(secs)s"
        }
        let mins = secs / 60
        if mins < 60 { return "\(mins)m \(secs % 60)s" }
        return "\(mins / 60)h \(mins % 60)m"
    }

    /// "148k", "1.2M", "812".
    public static func tokens(_ count: UInt64) -> String {
        switch count {
        case ..<1_000: "\(count)"
        case ..<1_000_000: "\(count / 1_000)k"
        default: String(format: "%.1fM", Double(count) / 1_000_000)
        }
    }

    /// "12 KB", "3.4 MB", "900 B".
    public static func bytes(_ size: UInt64) -> String {
        switch size {
        case ..<1_024: "\(size) B"
        case ..<1_048_576: "\(size / 1_024) KB"
        default: String(format: "%.1f MB", Double(size) / 1_048_576)
        }
    }

    static func plural(_ count: UInt32, _ one: String, _ many: String) -> String {
        count == 1 ? one : many
    }

    // MARK: - Rows

    /// "4 reads · 2 searches", with "+" when the run continues below what
    /// this phone holds.
    public static func run(_ run: RunInfo) -> String {
        let plus = run.openBelow ? "+" : ""
        var parts: [String] = []
        if run.reads > 0 {
            parts.append("\(run.reads)\(plus) " + plural(run.reads, "read", "reads"))
        }
        if run.searches > 0 {
            parts.append("\(run.searches)\(plus) " + plural(run.searches, "search", "searches"))
        }
        let other = run.len > run.reads + run.searches ? run.len - run.reads - run.searches : 0
        if other > 0 || parts.isEmpty { parts.append("\(other)\(plus) more") }
        return parts.joined(separator: " · ")
    }

    public static func explore(_ verb: ExploreVerb) -> String {
        switch verb {
        case .read: String(localized: "Read")
        case .search: String(localized: "Searched")
        case .list: String(localized: "Listed")
        case .fetch: String(localized: "Fetched")
        case .webSearch: String(localized: "Searched the web")
        }
    }

    /// A call's state beside it, when the verb does not already say it.
    public static func state(_ state: ToolStateView) -> String? {
        switch state {
        case .pending: String(localized: "waiting")
        case .running: String(localized: "running")
        case .succeeded: nil
        case .failed: String(localized: "failed")
        case .denied: String(localized: "denied")
        case .cancelled: String(localized: "cancelled")
        }
    }

    /// A call's verb says what happened to it: waiting for the person, under
    /// way, refused, cancelled or done.
    public static func verb(
        _ state: ToolStateView, _ row: Row, wants: String, doing: String, done: String
    ) -> String {
        let denied = state == .denied || row.decision?.outcome == .denied
        if denied { return String(localized: "Denied") }
        switch state {
        case .pending: return wants
        // An open ask points at the call: it runs only if the person allows it.
        case .running where row.attention && row.decision == nil: return wants
        case .running: return doing
        case .cancelled: return String(localized: "Cancelled")
        case .succeeded, .failed, .denied: return done
        }
    }

    /// A permission decision as meta: allowed or denied, its scope, the
    /// note, and where it was answered. The outcome word is left off when
    /// the verb already says it.
    public static func decision(_ decision: Decision, verbSaysIt: Bool = false) -> String {
        var parts: [String] = []
        if !verbSaysIt {
            let outcome = switch decision.outcome {
            case .allowed: String(localized: "allowed")
            case .denied: String(localized: "denied")
            case .autoApproved: String(localized: "auto-approved")
            case .dismissed: String(localized: "dismissed")
            }
            parts.append(outcome)
        }
        if let scope = decision.scope { parts.append(scope) }
        if let note = decision.note { parts.append("“\(firstLine(note))”") }
        if decision.elsewhere { parts.append(String(localized: "in the terminal")) }
        return parts.joined(separator: " · ")
    }

    /// The row's meta with its decision after it; the call's own state word
    /// for the same thing is dropped.
    public static func meta(_ meta: [String], _ row: Row, verb: String = "") -> String {
        guard let decision = row.decision else {
            return meta.filter { !$0.isEmpty }.joined(separator: " · ")
        }
        let dropped: Set<String> = [
            String(localized: "denied"), String(localized: "cancelled"),
            String(localized: "waiting"),
        ]
        var kept = meta.filter { !$0.isEmpty && !dropped.contains($0) }
        let decided = Self.decision(
            decision,
            verbSaysIt: verb == String(localized: "Denied") && decision.outcome == .denied)
        if !decided.isEmpty { kept.append(decided) }
        return kept.joined(separator: " · ")
    }

    public static func answer(_ answer: AnswerView) -> String {
        if answer.hidden { return String(localized: "answered (hidden)") }
        var picks = answer.picked
        if let other = answer.other { picks.append("“\(other)”") }
        return picks.joined(separator: ", ")
    }

    public static func resolution(_ resolution: Resolution, answered: String) -> String {
        switch resolution {
        case .open: String(localized: "Asking")
        case .answered: answered
        case .declined: String(localized: "Declined")
        case .cancelled: String(localized: "Cancelled")
        case .dismissed: String(localized: "Dismissed")
        }
    }

    public static func boundary(_ kind: BoundaryKind, cause: String) -> String {
        let word = switch kind {
        case .started: String(localized: "Started")
        case .cleared: String(localized: "Cleared")
        case .compacted: String(localized: "Compacted")
        case .resumed: String(localized: "Resumed")
        case .forked: String(localized: "Forked")
        case .restarted: String(localized: "Restarted")
        case .exited: String(localized: "Ended")
        case .daemonLost: String(localized: "Lost its daemon")
        case .unspecified: String(localized: "Session")
        }
        return cause.isEmpty ? word : "\(word) · \(cause)"
    }

    /// "Compacted · 148k → 22k".
    public static func compaction(before: UInt64?, after: UInt64?, automatic: Bool) -> String {
        var words = String(localized: "Compacted")
        if let before, let after { words += " · \(tokens(before)) → \(tokens(after))" }
        if automatic { words += " · " + String(localized: "automatic") }
        return words
    }

    /// "Worked 1m 42s · $0.42".
    public static func turnEnd(durationMs: Int64?, costUsd: Double?, failed: Bool) -> String {
        var words = [durationMs.map { String(localized: "Worked \(duration($0))") }
            ?? String(localized: "Turn ended")]
        if let costUsd { words.append(String(format: "$%.2f", costUsd)) }
        if failed { words.append(String(localized: "failed")) }
        return words.joined(separator: " · ")
    }

    public static func thinking(open: Bool, durationMs: Int64?) -> String {
        if open { return String(localized: "Thinking") }
        guard let durationMs else { return String(localized: "Thought") }
        return String(localized: "Thought for \(duration(durationMs))")
    }

    /// "40 tools · 48s".
    public static func subagent(toolCount: UInt32, durationMs: Int64?) -> String {
        var parts = ["\(toolCount) " + plural(toolCount, "tool", "tools")]
        if let durationMs { parts.append(duration(durationMs)) }
        return parts.joined(separator: " · ")
    }

    /// An attachment as its chip names it, from the reference alone.
    public static func chip(_ view: AttachmentView) -> String {
        switch view {
        case .image(let blob): "\(blob.name) · \(bytes(blob.size))"
        case .file(let blob): "\(blob.name) · \(bytes(blob.size))"
        case .text(let name, let lines): String(localized: "\(name) · \(lines) lines")
        case .review(let comments, _):
            comments == 1
                ? String(localized: "Review · 1 comment")
                : String(localized: "Review · \(comments) comments")
        case .empty: String(localized: "Attachment")
        }
    }

    public static func chipGlyph(_ view: AttachmentView) -> String {
        switch view {
        case .image: "photo"
        case .file: "doc"
        case .text: "doc.plaintext"
        case .review: "text.bubble"
        case .empty: "paperclip"
        }
    }

    public static func firstLine(_ text: String) -> String {
        String(text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first ?? "")
    }

    public static func text(of segments: [Segment]) -> String {
        segments.map { segment in
            switch segment {
            case .text(let text): text
            case .attachment(let view): "[\(chip(view))]"
            }
        }.joined()
    }

    // MARK: - The activity line

    /// "Running cargo test · 12s". `subject` names the call a Running line
    /// points at, when the chat holds it.
    public static func activity(_ kind: ActivityKind, elapsedMs: Int64, subject: String?) -> String {
        let elapsed = duration(elapsedMs / 1_000 * 1_000)
        switch kind {
        case .working: return String(localized: "Working · \(elapsed)")
        case .thinking: return String(localized: "Thinking · \(elapsed)")
        case .running:
            guard let subject, !subject.isEmpty else {
                return String(localized: "Running · \(elapsed)")
            }
            return String(localized: "Running \(subject) · \(elapsed)")
        case .subagents(let count):
            return count == 1
                ? String(localized: "1 subagent working · \(elapsed)")
                : String(localized: "\(count) subagents working · \(elapsed)")
        case .compacting: return String(localized: "Compacting · \(elapsed)")
        case .retrying(let attempt, let most, _):
            return most > 0
                ? String(localized: "Retrying · attempt \(attempt) of \(most)")
                : String(localized: "Retrying · attempt \(attempt)")
        }
    }

    /// What a row names when the activity line points at it.
    public static func subject(of row: Row) -> String? {
        switch row.kind {
        case .command(let command, _, _, _, _, _): firstLine(command)
        case .explore(_, let subject, _): subject
        case .toolCall(let server, let tool, _, _, _): server.isEmpty ? tool : "\(server) · \(tool)"
        case .fileChange(let files, _): files.first?.path
        case .subagent(let description, _, _, _, _, _): firstLine(description)
        default: nil
        }
    }

    // MARK: - The composer

    /// What the empty field says: who a message goes to, or why sending
    /// waits while the draft is kept.
    public static func placeholder(
        _ composer: Composer?, agent: String, host: String, away: Away?, working: Bool
    ) -> String {
        switch composer {
        case .send?, nil:
            return working
                ? String(localized: "Queue a message")
                : String(localized: "Message \(agent)")
        case .resume?:
            return String(localized: "\(agent) has exited · a message resumes it")
        case .disabled(.catchingUp)?:
            return String(localized: "Catching up · your draft is kept")
        case .disabled(.reconnecting)?:
            return String(localized: "Reconnecting · your draft is kept")
        case .disabled(.detached)?:
            switch away {
            case .revoked?:
                return String(localized: "\(host) no longer trusts this phone · your draft is kept")
            case .signedOut?:
                return String(localized: "This phone is signed out · your draft is kept")
            default:
                return String(localized: "\(host) is out of reach · your draft is kept")
            }
        }
    }

    public static func queued(_ row: QueuedRow) -> String {
        if row.steered { return String(localized: "steered") }
        if let agent = row.fromAgent { return String(localized: "queued from \(agent)") }
        return String(localized: "queued")
    }

    public static func outbox(_ state: OutboxState) -> String {
        switch state {
        case .sending: String(localized: "sending")
        case .notConfirmed: String(localized: "not confirmed")
        case .rejected(let reason): String(localized: "not sent · \(reason)")
        }
    }

    // MARK: - Review

    public static func files(_ count: Int) -> String {
        count == 1 ? String(localized: "1 file") : String(localized: "\(count) files")
    }

    public static func comments(_ count: Int) -> String {
        count == 1 ? String(localized: "1 comment") : String(localized: "\(count) comments")
    }

    /// "4 files, 18 added, 28 removed": the counts as a sentence to hear.
    public static func changes(files: Int, added: UInt32, removed: UInt32) -> String {
        String(localized: "\(Self.files(files)), \(added) added, \(removed) removed")
    }

    public static func attachReview(_ count: Int) -> String {
        count == 1
            ? String(localized: "Attach Review · 1 comment")
            : String(localized: "Attach Review · \(count) comments")
    }

    /// "Added line 12, Refused(String),": a diff line as VoiceOver reads it.
    public static func spoken(_ line: DiffLine) -> String {
        let text = line.text.trimmingCharacters(in: .whitespaces)
        switch line.kind {
        case .added: return String(localized: "Added line \(line.newLine ?? 0), \(text)")
        case .removed: return String(localized: "Removed line \(line.oldLine ?? 0), \(text)")
        case .context: return String(localized: "Line \(line.newLine ?? line.oldLine ?? 0), \(text)")
        }
    }

    public static func commentOn(lines: Int) -> String {
        lines == 1 ? String(localized: "Comment on 1 line") : String(localized: "Comment on \(lines) lines")
    }

    // MARK: - The strip

    /// The facts strip's parts, in order; each only while it is true.
    public static func strip(_ strip: Strip) -> [(text: String, warn: Bool)] {
        var parts: [(String, Bool)] = []
        if let tasks = strip.tasks {
            var words = String(localized: "Tasks \(tasks.done)/\(tasks.total)")
            if !tasks.current.isEmpty { words += " · \(tasks.current)" }
            parts.append((words, false))
        }
        if let context = strip.context, context.inStrip, let percent = context.percent {
            parts.append((String(localized: "\(percent)% context"), true))
        }
        if let background = strip.background {
            parts.append((String(localized: "\(background) in background"), false))
        }
        if let usage = strip.usage, !usage.blocked {
            let near = usage.windows
                .map { "\($0.name) \(Int($0.usedPercent.rounded()))%" }
                .joined(separator: " · ")
            parts.append((
                near.isEmpty
                    ? String(localized: "Near the usage limit")
                    : String(localized: "Near the usage limit · \(near)"),
                true))
        }
        for server in strip.failedServers {
            parts.append((
                server.needsAuth
                    ? String(localized: "\(server.name) needs sign-in")
                    : String(localized: "\(server.name) failed to start"),
                true))
        }
        return parts
    }

    // MARK: - Settings

    /// The model chip's two lines: the model and the mode by the names the
    /// settings card gives them, the effort beside the mode. Nil when
    /// nothing is reported.
    public static func chip(_ strip: Strip, _ settings: SettingsView?) -> (model: String, detail: String)? {
        let current = settings?.modes.first { $0.current }
        let mode = current.map { self.mode($0.value) } ?? strip.mode
        let detail = [strip.effort, mode].compactMap { $0 }.filter { !$0.isEmpty }
            .joined(separator: " · ")
        let model = settings?.models.first { $0.current }.map(self.model) ?? strip.model ?? ""
        if model.isEmpty && detail.isEmpty { return nil }
        return (model, detail)
    }

    /// A model by the name its agent offers it under, else the id the agent
    /// reports.
    public static func model(_ choice: ModelChoice) -> String {
        choice.displayName.isEmpty ? choice.value : choice.displayName
    }

    /// A permission mode by the name a person reads.
    public static func mode(_ value: ModeValue) -> String {
        switch value {
        case .claude(let mode):
            switch mode {
            case "default": String(localized: "Default")
            case "acceptEdits": String(localized: "Accept edits")
            case "plan": String(localized: "Plan")
            case "auto": String(localized: "Auto")
            case "bypassPermissions": String(localized: "Bypass permissions")
            default: mode
            }
        case .codex(let approval, let sandbox, let preset):
            switch preset {
            case "read-only"?: String(localized: "Read only")
            case "auto"?: String(localized: "Auto")
            case "full-access"?: String(localized: "Full access")
            default: "\(approval) · \(sandbox)"
            }
        }
    }

    /// What the agent does under a mode without asking first.
    public static func modeDetail(_ value: ModeValue) -> String {
        switch value {
        case .claude(let mode):
            switch mode {
            case "default": String(localized: "Asks before edits and commands")
            case "acceptEdits": String(localized: "Edits files without asking, asks before commands")
            case "plan": String(localized: "Plans without changing anything")
            case "auto": String(localized: "Decides for itself when to ask")
            case "bypassPermissions": String(localized: "Never asks")
            default: String(localized: "Reported by the agent")
            }
        case .codex(_, _, let preset):
            switch preset {
            case "read-only"?: String(localized: "Reads files, asks before any change")
            case "auto"?: String(localized: "Works in its folder, asks to go further")
            case "full-access"?: String(localized: "Never asks, with full access")
            default: String(localized: "Reported by the agent")
            }
        }
    }

    /// The permissions card's heading, by the agent's kind.
    public static func permissionsHeading(_ kind: Kind?) -> String {
        kind == .codex ? String(localized: "CODEX PERMISSIONS") : String(localized: "CLAUDE PERMISSIONS")
    }

    /// A command as VoiceOver speaks it: its name, then where it comes from.
    public static func command(_ command: CommandView) -> String {
        command.source.isEmpty
            ? command.name
            : String(localized: "\(command.name), from \(command.source)")
    }

    /// The plus menu's permissions row: the current mode after the name.
    public static func permissionsItem(_ settings: SettingsView) -> String {
        guard let current = settings.modes.first(where: { $0.current }) else {
            return String(localized: "Permissions")
        }
        return String(localized: "Permissions · \(mode(current.value))")
    }

    public static func signIn(_ view: SignInView) -> String {
        let head = switch view.state {
        case .expired: String(localized: "Sign-in expired")
        case .failed: String(localized: "Sign-in failed")
        default: String(localized: "Signed out")
        }
        return view.account.isEmpty ? head : "\(head) · \(view.account)"
    }

    // MARK: - The card

    /// What the card says it wants.
    public static func headline(_ card: AskCard) -> String {
        switch card.body {
        case .command: return String(localized: "Wants to run a command")
        case .edit(_, let files, _, _, _, _) where files > 1:
            return String(localized: "Wants to edit \(files) files")
        case .edit: return String(localized: "Wants to edit a file")
        case .tool(let server, let tool, _):
            return server.isEmpty
                ? String(localized: "Wants to use \(tool)")
                : String(localized: "Wants to use \(server) · \(tool)")
        case .question(let questions) where questions.count > 1:
            return String(localized: "\(questions.count) questions")
        case .question: return String(localized: "Question")
        case .plan: return String(localized: "Plan")
        case .form(let server, _, _): return String(localized: "\(server) needs details")
        case .link(let server, _, _): return String(localized: "\(server) wants you to open a link")
        case .access: return String(localized: "Wants more access")
        case .unanswerable: return String(localized: "Can’t answer this here")
        }
    }

    static func scope(_ scope: Scope) -> String {
        switch scope {
        case .session: String(localized: "for this session")
        case .project: String(localized: "in this project")
        case .projectShared: String(localized: "in this project, for everyone")
        case .user: String(localized: "in every project")
        case .other(let other): other
        }
    }

    /// A choice in words: what happens, never rule syntax.
    public static func choice(_ choice: Choice) -> String {
        let label: String = switch choice.outcome {
        case .allowOnce: String(localized: "Allow once")
        case .allowAlways(let subjects, let directories, let mode, let scope, let label):
            if !subjects.isEmpty {
                String(localized: "Always allow \(subjects.joined(separator: ", ")) \(Self.scope(scope))")
            } else if !directories.isEmpty {
                String(localized: "Allow access to \(directories.joined(separator: ", ")) \(Self.scope(scope))")
            } else if !mode.isEmpty {
                String(localized: "Switch to \(mode) mode")
            } else if !label.isEmpty {
                label
            } else {
                String(localized: "Always allow \(Self.scope(scope))")
            }
        case .allowForSession: String(localized: "Allow for this session")
        case .allowSimilar(let prefix):
            String(localized: "Allow commands starting with \(prefix.joined(separator: " "))")
        case .allowNetwork(let hosts):
            hosts.isEmpty
                ? String(localized: "Allow network access")
                : String(localized: "Allow network access to \(hosts.joined(separator: ", "))")
        case .deny(let stops): stops ? String(localized: "Deny and stop") : String(localized: "Deny")
        case .denyAndStop: String(localized: "Deny and stop")
        case .approvePlan: String(localized: "Approve")
        case .sendBack: String(localized: "Send back")
        case .submit: String(localized: "Submit")
        case .decline: String(localized: "Decline")
        case .openLink: String(localized: "I’m done")
        case .grantForTurn: String(localized: "Grant")
        case .grantForSession: String(localized: "Grant")
        }
        return choice.takesNote ? label + "…" : label
    }

    /// "1 of 3" when asks are queued.
    public static func position(_ card: AskCard) -> String? {
        card.count > 1 ? String(localized: "\(card.position) of \(card.count)") : nil
    }
}
