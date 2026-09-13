import Foundation

/// What is true about a turn right now, gathered for the strip above the
/// composer.
///
/// Every field is something a host said. Nothing here is counted, timed or
/// inferred on the phone: the task list and its arithmetic are the provider's
/// own, folded by the shared library; the children are the core's ranking of
/// the family; the held message is the one the core is holding. A fact that
/// nobody has stated is absent, and a strip with no facts in it is not drawn
/// at all — an empty band along the bottom of every quiet conversation would
/// cost the feed a row of screen to say nothing.
public struct ConversationFacts: Equatable, Sendable {
    /// The provider's list, where it keeps one.
    public let tasks: TaskList?
    /// How many agents this one started, and whether any of them has stopped
    /// and cannot go on without somebody.
    public let children: Children?
    /// The message waiting for the turn to end.
    public let queued: QueuedMessage?

    public struct Children: Equatable, Sendable {
        public let count: Int
        /// Set when one of them is waiting on a person. It is the only thing
        /// in the strip allowed to be coloured, for the same reason it is the
        /// only coloured thing on the home: the accent is this app's one word
        /// for "something is waiting for you".
        public let needs: Why?

        public init(count: Int, needs: Why?) {
            self.count = count
            self.needs = needs
        }
    }

    public init(tasks: TaskList?, children: Children?, queued: QueuedMessage?) {
        self.tasks = tasks
        self.children = children
        self.queued = queued
    }

    /// Nothing is true, so there is no strip.
    ///
    /// Started agents count here because the strip is their way into the
    /// expanded list. Without it a child waiting for an answer would be a fact
    /// in the store with no reachable control on screen.
    public var isEmpty: Bool { tasks == nil && children == nil && queued == nil }

    /// Whether there is a list to grow into. A strip with one line of task in
    /// it and nothing behind that line has nothing to open.
    public var opens: Bool {
        !(tasks?.items.isEmpty ?? true) || children != nil
    }

    /// What the count reads: "3/7". The provider's own two numbers, in the
    /// order it reports them, and never recomputed from the items — a list
    /// whose items disagree with its own count is the host's disagreement to
    /// resolve, not something to paper over here.
    public var progress: String? {
        guard let tasks else { return nil }
        return "\(tasks.done)/\(tasks.total)"
    }

    /// The task being worked on. A list whose current task the provider never
    /// named still has a count worth reading, so this may be absent while
    /// ``progress`` is not.
    public var current: String? { tasks?.current }
}

extension ConversationFacts {
    /// The facts a conversation can state about its own turn.
    ///
    /// The children are counted from the same roster the expanded strip draws,
    /// so its summary and Started rows can never disagree.
    @MainActor
    public init(_ store: ConversationStore) {
        let roster = store.children()
        self.init(
            tasks: store.provider.todos,
            children: roster.isEmpty
                ? nil
                : Children(
                    count: roster.count,
                    needs: roster.compactMap(\.needs).first),
            queued: store.queued)
    }
}
