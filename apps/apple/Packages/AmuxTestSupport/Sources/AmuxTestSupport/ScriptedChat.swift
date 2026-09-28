import AmuxCore
import Foundation

/// A chat with no runtime behind it: rows, a frame, a card and a strip set
/// by whoever builds it. The component catalogue draws the real chat views
/// from one, and a test drives a model through it.
public final class ScriptedChat: ChatSource, @unchecked Sendable {
    private let lock = NSLock()
    private var ordered: [Row]
    private var current: ChatFrame
    private var card: AskCard?
    private var facts: Strip
    private var offered: SettingsView?
    private var images: [[UInt8]: Data]
    private var pending = ChatChanges(keys: [], reloaded: false, session: false)
    /// What was sent, in order, for a test to read back.
    public private(set) var sent: [Draft] = []
    /// The settings picks sent, in order.
    public private(set) var changed: [SettingChange] = []
    /// What asking for older rows comes to.
    public var paged: PageOutcome = .arrived(0)
    /// What asking for the working-tree diff comes to; nil answers that the
    /// machine could not be asked.
    public var working: FrozenReview?

    public init(
        rows: [Row], frame: ChatFrame, card: AskCard? = nil, strip: Strip = ScriptedChat.strip(),
        settings: SettingsView? = nil, images: [[UInt8]: Data] = [:]
    ) {
        ordered = rows
        current = frame
        self.card = card
        facts = strip
        offered = settings
        self.images = images
    }

    // MARK: - Building values

    public static let agent = AgentKey(
        host: Array(repeating: 1, count: 16), agent: Array(repeating: 2, count: 16))

    public static func frame(
        name: String = "refactor-auth", kind: Kind = .claudeSdk, phase: PhaseView = .idle,
        mode: Composer = .send, activity: Activity? = nil, caughtUp: Bool = true,
        hasOlder: Bool = false, queue: [QueuedRow] = [], outbox: [OutboxRow] = [],
        waiting: Waiting? = nil
    ) -> ChatFrame {
        ChatFrame(
            agent: agent, name: name, kind: kind, phase: phase,
            composer: ComposerView(mode: mode, activity: activity), connection: .live,
            caughtUp: caughtUp, hasOlder: hasOlder, queue: queue, outbox: outbox, askInput: nil,
            ended: nil, waiting: waiting)
    }

    public static func strip(
        tasks: TasksView? = nil, context: ContextView? = nil, model: String? = nil,
        effort: String? = nil, mode: String? = nil, usage: UsageView? = nil,
        failedServers: [ServerView] = [], signIn: SignInView? = nil, background: UInt32? = nil
    ) -> Strip {
        Strip(
            failedServers: failedServers, background: background, context: context,
            effort: effort, mode: mode, model: model, signIn: signIn, tasks: tasks, usage: usage,
            workingOn: nil)
    }

    public static func row(
        _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
        decision: Decision? = nil, run: RunInfo? = nil, parent: String? = nil,
        collapsed: Bool = false
    ) -> Row {
        Row(
            id: id, order: order, atMs: 1_790_000_000_000 + Int64(order) * 1_000, kind: kind,
            collapsed: collapsed || parent != nil, attention: attention, decision: decision,
            parent: parent, run: run)
    }

    // MARK: - Changing it

    /// Appends rows at the newest edge and says so on the next take.
    public func append(_ rows: [Row]) {
        lock.withLock {
            ordered.append(contentsOf: rows)
            pending.keys.append(contentsOf: rows.map(\.id))
        }
    }

    /// Replaces rows in place and says so on the next take.
    public func revise(_ rows: [Row]) {
        lock.withLock {
            for row in rows {
                if let index = ordered.firstIndex(where: { $0.id == row.id }) { ordered[index] = row }
            }
            pending.keys.append(contentsOf: rows.map(\.id))
        }
    }

    public func show(
        frame: ChatFrame? = nil, card: AskCard?? = nil, strip: Strip? = nil,
        settings: SettingsView? = nil
    ) {
        lock.withLock {
            if let frame { current = frame }
            if let card { self.card = card }
            if let strip { facts = strip }
            if let settings { offered = settings }
            pending.session = true
        }
    }

    // MARK: - ChatSource

    public func keys() -> [String] { lock.withLock { ordered.map(\.id) } }

    public func keys(above newest: String) -> [String]? {
        lock.withLock {
            guard let index = ordered.firstIndex(where: { $0.id == newest }) else { return nil }
            return ordered[(index + 1)...].map(\.id)
        }
    }

    public func keys(below oldest: String) -> [String]? {
        lock.withLock {
            guard let index = ordered.firstIndex(where: { $0.id == oldest }) else { return nil }
            return ordered[..<index].map(\.id)
        }
    }

    public func rows(for keys: [String], options: RowOptions?) -> [Row] {
        lock.withLock {
            let wanted = Set(keys)
            return ordered.filter { wanted.contains($0.id) }
        }
    }

    public func askCard() -> AskCard? { lock.withLock { card } }
    public func strip() -> Strip? { lock.withLock { facts } }
    public func settings() -> SettingsView? { lock.withLock { offered } }
    public func frame() -> ChatFrame? { lock.withLock { current } }

    public func takeChanges() -> ChatChanges {
        lock.withLock {
            defer { pending = ChatChanges(keys: [], reloaded: false, session: false) }
            return pending
        }
    }

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        lock.withLock { sent.append(draft) }
        return .success(SendOutcome(inputId: [1], state: .sent))
    }

    public func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    public func answer(_ ask: String, picks: [Pick], note: String?) async -> ActOutcome? { .done }
    public func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome? { .done }
    public func withdraw(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    public func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    public func discard(_ input: [UInt8]) {}

    /// The words of the queued or outbox row with this id; a scripted chat
    /// holds no attachments behind its rows.
    public func draft(of input: [UInt8]) -> Draft? {
        let frame = lock.withLock { current }
        let text = frame.queue.first { $0.inputId == input }?.text
            ?? frame.outbox.first { $0.inputId == input }?.text
        return text.map { segments in
            let words = segments.compactMap { segment -> String? in
                if case .text(let text) = segment { text } else { nil }
            }
            return Draft(text: words.joined(), attachments: nil)
        }
    }
    public func interrupt() async -> ActOutcome? { .done }

    public func change(_ setting: SettingChange) async -> ActOutcome? {
        lock.withLock { changed.append(setting) }
        return .done
    }
    public func resume(with draft: Draft) async -> ActOutcome? { .done }
    public func pageOlder(_ rows: UInt32) async -> PageOutcome? { lock.withLock { paged } }

    public func putBlob(
        _ data: Data, name: String, mime: String
    ) async -> Result<BlobRef, RuntimeFailure> {
        let blob = BlobRef(hash: Array(name.utf8), name: name, mime: mime, size: UInt64(data.count))
        lock.withLock { images[blob.hash] = data }
        return .success(blob)
    }

    public func blob(_ hash: [UInt8]) -> Data? { lock.withLock { images[hash] } }

    public func review() async -> Result<FrozenReview, RuntimeFailure> {
        guard let working = lock.withLock({ working }) else {
            return .failure(RuntimeFailure("the machine could not be asked"))
        }
        return .success(working)
    }
}
