import Foundation

/// What an agent is doing while a turn runs, as the composer says it.
///
/// Two facts, from two places, because they answer two different questions.
/// The name is the transcript's own last row — what the agent is doing this
/// second, in the words it used — and the elapsed time is how long the agent
/// has been on the piece of work it announced. Neither is measured on the
/// phone: a clock this app started would run on while a host was unreachable,
/// and a photograph of one would show a different number every time.
public struct ComposerActivity: Equatable, Sendable {
    /// "Thinking", "Running cargo test --workspace".
    public let name: String
    /// "8s", in the shortest true unit, or nothing where the machine has not
    /// said when this began.
    public let elapsed: String?

    public init(name: String, elapsed: String?) {
        self.name = name
        self.elapsed = elapsed
    }
}

/// Whether there is a composer at all, and which of its two states it is in.
///
/// The bottom of a conversation holds exactly one thing. An unanswered ask
/// holds it first, then a finished turn's offer, then whatever
/// ``ConversationFootState`` has to report; the composer is what is there when
/// none of those are. So this is `nil` for every gate that will not take a
/// message — an ended run, a session this build cannot read, a layer catching
/// up — and a box is drawn only where writing one is a thing a person can
/// actually do.
public enum ComposerState: Equatable, Sendable {
    /// The layer will take a message now.
    case writing
    /// A turn is running. What is written is held until it ends.
    case working(ComposerActivity)

    /// - Parameters:
    ///   - tail: the transcript's last row, which is what the agent is doing.
    ///   - elapsed: how long it has been doing it, from the fleet's own clock.
    public init?(gate: SendGate, tail: TranscriptRow?, elapsed: String?) {
        switch gate {
        case .claudePty(let gate):
            switch gate {
            case .ready: self = .writing
            case .working: self = .working(Self.activity(tail, elapsed))
            default: return nil
            }
        case .codex(let gate):
            switch gate {
            case .ready: self = .writing
            case .activeTurn: self = .working(Self.activity(tail, elapsed))
            default: return nil
            }
        case .unavailable:
            return nil
        }
    }

    /// What the empty field says. Naming the agent is the whole of it: a
    /// person writing to one of five agents has to be able to tell from the
    /// box which one is about to be interrupted.
    public func placeholder(agent name: String) -> String {
        switch self {
        case .writing: "Message \(name)"
        case .working: "Queue a message"
        }
    }

    public var activity: ComposerActivity? {
        guard case .working(let activity) = self else { return nil }
        return activity
    }

    /// A turn is running, so pressing the button holds the message rather
    /// than sending it, and the button with nothing to say stops the turn.
    public var busy: Bool { activity != nil }

    /// What the agent is doing, named by the row it is doing it in.
    ///
    /// The transcript is the only place that knows. A gate says a turn is
    /// running and a phase says "running"; neither says what is being run, and
    /// the row that is still open does.
    private static func activity(_ tail: TranscriptRow?, _ elapsed: String?) -> ComposerActivity {
        ComposerActivity(name: name(tail), elapsed: elapsed)
    }

    /// One word, because the row it came from is directly above the box and
    /// already spells the command, the path or the query in full. Repeating it
    /// here would put the same sentence on the screen twice and truncate it
    /// the second time, which is the version a person would try to read.
    private static func name(_ tail: TranscriptRow?) -> String {
        switch tail?.kind {
        case .thinking: "Thinking"
        case .prose(_, let open): open ? "Writing" : "Working"
        case .ran: "Running"
        case .exploration: "Reading"
        case .edit: "Editing"
        case .wrote: "Writing"
        case .tool(let name, _, _): name
        // A turn whose last row is the prompt that started it, or a row this
        // build has no word for. "Working" is the true thing to say, and the
        // elapsed time beside it is still the useful half.
        default: "Working"
        }
    }
}

extension SendGate {
    /// Stop what is running now, in the layer's own vocabulary.
    ///
    /// There is no shared interrupt: each layer stops a turn its own way, and
    /// the two are kept apart here for the same reason their refusals are.
    /// Nothing to send for a layer with no turn to stop.
    public func interrupt(_ agent: AgentId) -> JSONValue? {
        switch self {
        case .claudePty:
            .object([
                "command": .string("claude"),
                "claude_command": .string("interrupt"),
                "agent": .string(agent.description),
            ])
        case .codex:
            .object([
                "command": .string("codex"),
                "codex_command": .string("interrupt"),
                "agent": .string(agent.description),
            ])
        case .unavailable:
            nil
        }
    }
}
