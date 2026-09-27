import AmuxValues
import Foundation
import Observation

/// What a chat screen reads and acts on: an open chat on the runtime, or a
/// scripted stand-in where there is no runtime to open one on.
public protocol ChatSource: AnyObject, Sendable {
    func keys() -> [String]
    func keys(above newest: String) -> [String]?
    func keys(below oldest: String) -> [String]?
    func rows(for keys: [String], options: RowOptions?) -> [Row]
    func askCard() -> AskCard?
    func strip() -> Strip?
    func settings() -> SettingsView?
    func frame() -> ChatFrame?
    func takeChanges() -> ChatChanges
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure>
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome?
    func answer(_ ask: String, picks: [Pick], note: String?) async -> ActOutcome?
    func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome?
    func withdraw(_ input: [UInt8]) async -> ActOutcome?
    func sendNow(_ input: [UInt8]) async -> ActOutcome?
    func resend(_ input: [UInt8]) async -> SendOutcome?
    func discard(_ input: [UInt8])
    func interrupt() async -> ActOutcome?
    func change(_ setting: SettingChange) async -> ActOutcome?
    func resume(with draft: Draft) async -> ActOutcome?
    func pageOlder(_ rows: UInt32) async -> PageOutcome?
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure>
    func blob(_ hash: [UInt8]) -> Data?
    func review() async -> Result<FrozenReview, RuntimeFailure>
}

/// An agent's uncommitted changes as the chat header counts them.
public struct WorkingChanges: Equatable, Sendable {
    public var review: FrozenReview
    public var files: Int
    public var added: UInt32
    public var removed: UInt32
}

/// One row of the list, held by its key. A cell observes only its own row,
/// so a change to one item redraws one cell.
@MainActor
@Observable
public final class RowCell: Identifiable {
    public let id: String
    public fileprivate(set) var row: Row?

    init(id: String, row: Row?) {
        self.id = id
        self.row = row
    }
}

/// Asking for older rows, as the top of the list says it.
public enum ChatPaging: Equatable, Sendable {
    case idle
    case fetching
    /// Older history is held only by the agent's machine, which cannot be
    /// reached; the next scroll to the top asks again.
    case unreachable
    case failed(String)
}

/// One open chat as the phone's list holds it.
///
/// The list is retained: it holds the sequence of row keys, which only ever
/// grows at its two edges, and each cell reads its row by key when it is
/// first drawn. An update names the keys it changed; only cells already
/// drawn are read again, and only those cells redraw. A Reset's swap is the
/// one time the whole sequence is read again.
@MainActor
@Observable
public final class ChatModel {
    /// A page of older rows, and the rows a chat opens with.
    public static let pageRows: UInt32 = 40
    /// The most a page asks for at a run that continues below the window.
    public static let largestPage: UInt32 = 1000
    /// How long an empty chat waits before saying it is loading, so a chat
    /// that fills at once never flashes the hint.
    public static let loadingHintDelay: Duration = .milliseconds(300)

    @ObservationIgnored public let source: ChatSource
    /// Row keys, oldest first.
    public private(set) var ids: [String] = []
    @ObservationIgnored private var held: Set<String> = []
    @ObservationIgnored private var cells: [String: RowCell] = [:]
    public private(set) var frame: ChatFrame?
    public private(set) var ask: AskCard?
    public private(set) var strip: Strip?
    /// What the agent offers to change, the current values marked.
    public private(set) var settings: SettingsView?
    /// Rows the reader opened: a run's members, a subagent's steps, or a
    /// row's detail.
    public private(set) var expanded: Set<String> = []
    public private(set) var paging: ChatPaging = .idle
    /// The chat is still empty a moment after opening.
    public private(set) var loadingHint = false
    /// The reader is at the newest row, so new rows are followed.
    public private(set) var following = true
    /// Rows arrived below a reader who scrolled up.
    public private(set) var newActivity = false
    /// Bumped whenever the list should go to its newest row: a swap, a send,
    /// or the reader asking.
    public private(set) var toNewest = 0
    public var draft = ""
    public private(set) var attachments: [DraftAttachment] = []
    /// Attachments still being stored.
    public private(set) var uploading = 0
    /// A send or resume on its way.
    public private(set) var sending = false
    /// What went wrong with the last thing the person did, until the next.
    public private(set) var notice: String?
    /// The agent's uncommitted changes, when it has any: what the header's
    /// changes chip counts and the review page opens on.
    public private(set) var changes: WorkingChanges?
    /// The review being written on this agent's changes, kept while the
    /// chat is open so leaving the page loses no comment.
    public private(set) var reviewing: ReviewModel?
    /// Dictation into the draft; the app's speech recogniser drives it.
    public var dictation = DictationState()
    /// The field's text when a paste last left it, and when.
    @ObservationIgnored private var pasteEcho: (text: String, at: ContinuousClock.Instant)?

    /// - Parameter loadingHintAfter: how long an empty chat waits before
    ///   saying it is loading; zero says so from the first frame.
    public init(source: ChatSource, loadingHintAfter: Duration = ChatModel.loadingHintDelay) {
        self.source = source
        ids = source.keys()
        held = Set(ids)
        readSession()
        _ = source.takeChanges()
        if ids.isEmpty { waitForRows(loadingHintAfter) }
    }

    // MARK: - Reading

    /// The runtime moved: extend the sequence at its edges, read the changed
    /// cells again, and read the frame, the card and the strip.
    public func woke() {
        let changes = source.takeChanges()
        if changes.reloaded {
            swap()
        } else if !changes.keys.isEmpty {
            if changes.keys.contains(where: { !held.contains($0) }) { extend() }
            refresh(changes.keys.filter { cells[$0] != nil })
        }
        if changes.session || changes.reloaded || !changes.keys.isEmpty { readSession() }
    }

    /// The cell for a key, its row read now if it never was.
    public func cell(for id: String) -> RowCell {
        if let cell = cells[id] { return cell }
        let cell = RowCell(id: id, row: source.rows(for: [id], options: options).first)
        cells[id] = cell
        return cell
    }

    /// Whether a row draws: collapsed rows do not, except a subagent's steps
    /// under a subagent the reader opened.
    public func shows(_ row: Row) -> Bool {
        !row.collapsed || row.parent.map(expanded.contains) == true
    }

    public var options: RowOptions {
        RowOptions(tools: .collapseRuns(expanded: expanded.sorted()))
    }

    /// Opens or closes a row: its run, its steps, or its detail. A run is
    /// open while any key the reader opened it by is still one of its
    /// members, which stays true as the run grows.
    public func toggle(_ id: String) {
        if expanded.contains(id) {
            expanded.remove(id)
        } else if let open = openedBy(cells[id]?.row?.run) {
            expanded.remove(open)
        } else {
            expanded.insert(id)
        }
        refresh(Array(cells.keys))
    }

    public func isExpanded(_ id: String) -> Bool {
        expanded.contains(id) || openedBy(cells[id]?.row?.run) != nil
    }

    /// The opened key that belongs to this run. A member's run attribute is
    /// read again whenever the run moves, so its newest key names the run.
    private func openedBy(_ run: RunInfo?) -> String? {
        guard let run else { return nil }
        return expanded.first { cells[$0]?.row?.run?.newest == run.newest }
    }

    /// An attachment's bytes where this phone holds them; asking starts the
    /// fetch, and the row changes when they land.
    public func bytes(of blob: BlobRef) -> Data? {
        source.blob(blob.hash)
    }

    private func extend() {
        guard let newest = ids.last, let oldest = ids.first else {
            swap()
            return
        }
        guard let newer = source.keys(above: newest), let older = source.keys(below: oldest)
        else {
            swap()
            return
        }
        if !older.isEmpty {
            ids.insert(contentsOf: older, at: 0)
            held.formUnion(older)
        }
        if !newer.isEmpty {
            ids.append(contentsOf: newer)
            held.formUnion(newer)
            arrived()
        }
    }

    /// A Reset's transcript was swapped in: every key may be new. Cells the
    /// new sequence keeps are read again in place.
    private func swap() {
        let fresh = source.keys()
        ids = fresh
        held = Set(fresh)
        cells = cells.filter { held.contains($0.key) }
        refresh(Array(cells.keys))
        arrived()
    }

    private func arrived() {
        loadingHint = false
        if following {
            toNewest += 1
        } else {
            newActivity = true
        }
    }

    private func refresh(_ keys: [String]) {
        guard !keys.isEmpty else { return }
        let rows = source.rows(for: keys, options: options)
        for row in rows { cells[row.id]?.row = row }
    }

    private func readSession() {
        let before = frame
        frame = source.frame()
        ask = source.askCard()
        strip = source.strip()
        settings = source.settings()
        if Self.changesMayHaveMoved(from: before, to: frame) { refreshChanges() }
    }

    /// The working tree is asked about once the chat is current, and again
    /// each time a turn ends, which is when an agent's edits settle.
    static func changesMayHaveMoved(from before: ChatFrame?, to after: ChatFrame?) -> Bool {
        guard let after, after.caughtUp else { return false }
        guard let before, before.caughtUp else { return true }
        return before.phase == .working && after.phase != .working
    }

    /// Asks the agent's machine for its working-tree diff again.
    public func refreshChanges() {
        Task { [weak self] in
            guard let self, case .success(let review) = await self.source.review() else { return }
            let doc = ReviewModel.document(review, comments: [])
            self.changes = doc.files.isEmpty
                ? nil
                : WorkingChanges(review: review, files: doc.files.count, added: doc.added, removed: doc.removed)
        }
    }

    private func waitForRows(_ delay: Duration) {
        if delay == .zero {
            loadingHint = true
            return
        }
        Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard let self else { return }
            self.loadingHint = self.ids.isEmpty
        }
    }

    // MARK: - Scrolling

    /// Where the reader is: at the newest row or above it.
    public func reading(atNewest: Bool) {
        following = atNewest
        if atNewest { newActivity = false }
    }

    /// Takes the reader to the newest row.
    public func jumpToNewest() {
        following = true
        newActivity = false
        toNewest += 1
    }

    /// The top of the list came into view: ask for older rows if there are
    /// any and nothing is already on its way.
    public func reachedTop() {
        guard frame?.hasOlder == true, paging != .fetching else { return }
        paging = .fetching
        let rows = pageSize
        Task {
            let outcome = await source.pageOlder(rows)
            switch outcome {
            case .arrived?: paging = .idle
            case .originUnreachable?: paging = .unreachable
            case .failed(let reason)?: paging = .failed(reason)
            case nil: paging = .failed("the chat is closed")
            }
            woke()
        }
    }

    /// A page, or at a collapsed run that continues below the window, the
    /// run's own length up to a cap, so a long run arrives in a few pages.
    public var pageSize: UInt32 {
        let top = ids.lazy.compactMap { self.cells[$0]?.row }.first { self.shows($0) }
        guard let run = top?.run, run.openBelow, run.isSummary, !isExpanded(run.newest)
        else { return Self.pageRows }
        return min(max(run.len, Self.pageRows), Self.largestPage)
    }

    // MARK: - Writing

    /// Sending waits for the rows to be current and the agent live; the draft
    /// never does.
    public var canSend: Bool {
        guard let frame, frame.caughtUp, !sending, uploading == 0 else { return false }
        return frame.composer.mode == .send && hasDraft
    }

    public var canResume: Bool {
        guard let frame, !sending, uploading == 0 else { return false }
        return frame.composer.mode == .resume
    }

    public var hasDraft: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
    }

    private var composed: Draft {
        Draft(text: draft, attachments: attachments.isEmpty ? nil : attachments)
    }

    public func send() {
        guard canSend else { return }
        dictation.stop()
        let sent = composed
        clearDraft()
        sending = true
        notice = nil
        following = true
        toNewest += 1
        Task {
            let outcome = await source.send(sent)
            sending = false
            if case .failure(let failure) = outcome {
                restore(sent)
                notice = failure.description
            }
            woke()
        }
    }

    /// The exited composer's one tap: the draft starts the agent again as its
    /// first prompt.
    public func resume() {
        guard canResume else { return }
        dictation.stop()
        let sent = composed
        sending = true
        notice = nil
        Task {
            let outcome = await source.resume(with: sent)
            sending = false
            switch outcome {
            case .done?:
                if composed == sent { clearDraft() }
                following = true
                toNewest += 1
            case .rejected(let reason)?, .failed(let reason)?: notice = reason
            case .notConfirmed?: notice = Self.notConfirmed
            case nil: notice = Self.closed
            }
            woke()
        }
    }

    // MARK: - Slash commands and pastes

    /// How many commands the slash rows offer at once.
    public static let slashRows = 5
    /// A paste this long becomes one attachment instead of draft text, as
    /// the terminal client does.
    public static let pasteLines = 8
    public static let pasteCharacters = 1_000

    /// The agent's commands that match a draft that is exactly a leading
    /// "/word": by prefix, or by prefix after a plugin's namespace. The
    /// list is the settings view's, which already leaves out what a
    /// headless agent cannot run; an agent that offers none lists nothing.
    public var slashMatches: [CommandView] {
        guard let word = Self.slashWord(draft), let commands = settings?.commands else { return [] }
        return Array(commands.filter { Self.command($0.name, matches: word) }.prefix(Self.slashRows))
    }

    static func slashWord(_ draft: String) -> String? {
        guard draft.hasPrefix("/") else { return nil }
        let word = draft.dropFirst()
        guard !word.contains(where: { $0.isWhitespace || $0 == "/" }) else { return nil }
        return word.lowercased()
    }

    static func command(_ name: String, matches word: String) -> Bool {
        let name = name.lowercased()
        if name.hasPrefix(word) { return true }
        guard let colon = name.firstIndex(of: ":") else { return false }
        return name[name.index(after: colon)...].hasPrefix(word)
    }

    /// Puts a picked command in the draft as the agent reads one: Claude's
    /// "/name", and a Codex skill by its "$name" mention.
    public func pick(_ command: CommandView) {
        let sigil = frame?.kind == .codex ? "$" : "/"
        draft = sigil + command.name + " "
    }

    /// The field's edits. A long run arriving in one edit is a paste: it
    /// leaves the draft and joins it as one inline text attachment. Only
    /// the person's typing comes through here, so words put back into the
    /// draft stay words.
    public func type(_ text: String) {
        let text = text.replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        // The text view sends a paste's whole text again after the draft
        // takes it out; the same text straight after is that echo.
        if let echo = pasteEcho, echo.text == text, echo.at.duration(to: .now) < .milliseconds(500) {
            return
        }
        pasteEcho = nil
        let old = Array(draft)
        let new = Array(text)
        var head = 0
        while head < old.count, head < new.count, old[head] == new[head] { head += 1 }
        var tail = 0
        while tail < old.count - head, tail < new.count - head,
              old[old.count - 1 - tail] == new[new.count - 1 - tail] { tail += 1 }
        let inserted = String(new[head..<(new.count - tail)])
        guard inserted.count >= Self.pasteCharacters || Self.lines(inserted) >= Self.pasteLines else {
            draft = text
            return
        }
        draft = String(new[..<head]) + String(new[(new.count - tail)...])
        attachments.append(.text(name: String(localized: "Pasted text"), text: inserted))
        pasteEcho = (text, .now)
    }

    /// Lines as the chat counts them: a last line break ends a line rather
    /// than starting one.
    nonisolated public static func lines(_ text: String) -> Int {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        if lines.last?.isEmpty == true { lines.removeLast() }
        return lines.count
    }

    /// The whole of what dictation has heard so far.
    public func heard(_ text: String) {
        dictation.receive(text, draft: &draft)
    }

    public func clearDraft() {
        draft = ""
        attachments = []
    }

    private func restore(_ sent: Draft) {
        if draft.isEmpty { draft = sent.text } else { draft = sent.text + "\n" + draft }
        attachments = (sent.attachments ?? []) + attachments
    }

    /// Stores a photo or file and adds it to the draft once it is stored.
    public func attach(_ data: Data, name: String, mime: String, image: Bool) {
        uploading += 1
        notice = nil
        Task {
            let stored = await source.putBlob(data, name: name, mime: mime)
            uploading -= 1
            switch stored {
            case .success(let blob): attachments.append(image ? .image(blob) : .file(blob))
            case .failure(let failure): notice = failure.description
            }
        }
    }

    /// The review page's model for the changes the chip counted: the one
    /// already being written when it is on the same patch, else a new one.
    public func review(of changes: WorkingChanges) -> ReviewModel {
        if let reviewing, reviewing.review.diff.patch?.hash == changes.review.diff.patch?.hash {
            return reviewing
        }
        let review = ReviewModel(review: changes.review)
        reviewing = review
        return review
    }

    /// Puts a written review into the draft as one token, replacing the
    /// token of an earlier review of the same patch.
    public func attach(_ review: ReviewModel) {
        let patch = review.review.diff.patch?.hash
        attachments.removeAll { attachment in
            if case .review(let diff, _) = attachment { return diff.patch?.hash == patch }
            return false
        }
        attachments.append(review.attachment)
    }

    public func removeAttachment(at index: Int) {
        guard attachments.indices.contains(index) else { return }
        attachments.remove(at: index)
    }

    /// Takes a queued prompt back; its words return to the draft.
    public func withdraw(_ queued: QueuedRow) {
        act({ await $0.withdraw(queued.inputId) }) { [weak self] in
            guard let self else { return }
            let words = queued.text.compactMap { segment -> String? in
                if case .text(let text) = segment { text } else { nil }
            }.joined()
            let kept = queued.text.compactMap { segment -> DraftAttachment? in
                switch segment {
                case .attachment(.image(let blob)): .image(blob)
                case .attachment(.file(let blob)): .file(blob)
                default: nil
                }
            }
            if draft.isEmpty { draft = words } else if !words.isEmpty { draft = words + "\n" + draft }
            attachments = kept + attachments
        }
    }

    /// Steers a queued prompt into the running turn.
    public func sendNow(_ queued: QueuedRow) {
        act({ await $0.sendNow(queued.inputId) })
    }

    /// Sends an input that was not confirmed again, under a new id.
    public func resend(_ input: [UInt8]) {
        notice = nil
        Task {
            _ = await source.resend(input)
            woke()
        }
    }

    public func discard(_ input: [UInt8]) {
        source.discard(input)
        woke()
    }

    /// A rejected prompt back into the draft to change and send again.
    public func edit(_ outbox: OutboxRow) {
        let words = outbox.text.compactMap { segment -> String? in
            if case .text(let text) = segment { text } else { nil }
        }.joined()
        if draft.isEmpty { draft = words } else { draft = words + "\n" + draft }
        discard(outbox.inputId)
    }

    /// Stop: the interrupt. The turn ends, an open ask is dismissed, and the
    /// agent stays.
    public func interrupt() {
        act({ await $0.interrupt() })
    }

    /// Sends a pick from the settings card. Nothing changes here until the
    /// agent reports the new value, which moves the current mark and the
    /// strip together.
    public func change(_ setting: SettingChange) {
        act({ await $0.change(setting) })
    }

    // MARK: - Answering

    public func answer(choice: Int, note: String? = nil) {
        guard let ask else { return }
        act({ await $0.answer(ask.key, choice: choice, note: note) })
    }

    public func answer(picks: [Pick], note: String? = nil) {
        guard let ask else { return }
        act({ await $0.answer(ask.key, picks: picks, note: note) })
    }

    public func submit(choice: Int, content: String) {
        guard let ask else { return }
        act({ await $0.answerForm(ask.key, choice: choice, content: content) })
    }

    /// Sends the head ask's answer again after the connection dropped.
    public func resendAnswer() {
        guard let input = frame?.askInput else { return }
        resend(input)
    }

    public func discardAnswer() {
        guard let input = frame?.askInput else { return }
        discard(input)
    }

    private func act(
        _ body: @escaping @Sendable (ChatSource) async -> ActOutcome?,
        done: (@MainActor () -> Void)? = nil
    ) {
        notice = nil
        let source = self.source
        Task {
            switch await body(source) {
            case .done?: done?()
            case .rejected(let reason)?, .failed(let reason)?: notice = reason
            case .notConfirmed?: notice = Self.notConfirmed
            case nil: notice = Self.closed
            }
            woke()
        }
    }

    static let notConfirmed = String(
        localized: "The connection dropped before the agent answered.")
    static let closed = String(localized: "This chat is closed.")
}
