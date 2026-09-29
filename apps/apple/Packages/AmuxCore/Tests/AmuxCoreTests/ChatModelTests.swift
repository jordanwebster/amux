import Foundation
import XCTest

@testable import AmuxCore

/// A chat source that records what the model asks it for.
private final class FakeChat: ChatSource, @unchecked Sendable {
    var ordered: [Row]
    var current: ChatFrame
    var card: AskCard?
    var pending = ChatChanges(keys: [], reloaded: false, session: false)
    /// Every key the model asked a row for, per call.
    var fetched: [[String]] = []
    var paged: [UInt32] = []
    /// The history before `ordered` that a page brings in, oldest first.
    var earlier: [Row] = []
    var sent: [Draft] = []
    var resumed: [Draft] = []
    var withdrawn: [[UInt8]] = []
    var discarded: [[UInt8]] = []
    /// What each queued or sent prompt held, by input id.
    var drafts: [[UInt8]: Draft] = [:]
    var interrupts = 0
    var facts: Strip?
    var offered: SettingsView?
    var changed: [SettingChange] = []
    /// The working-tree diff the machine answers with, and how often it was
    /// asked for.
    var working: FrozenReview?
    var reviews = 0

    init(rows: [Row], frame: ChatFrame) {
        ordered = rows
        current = frame
    }

    func keys() -> [String] { ordered.map(\.id) }

    func keys(above newest: String) -> [String]? {
        guard let index = ordered.firstIndex(where: { $0.id == newest }) else { return nil }
        return ordered[(index + 1)...].map(\.id)
    }

    func keys(below oldest: String) -> [String]? {
        guard let index = ordered.firstIndex(where: { $0.id == oldest }) else { return nil }
        return ordered[..<index].map(\.id)
    }

    func rows(for keys: [String], options: RowOptions?) -> [Row] {
        fetched.append(keys)
        return ordered.filter { keys.contains($0.id) }
    }

    func askCard() -> AskCard? { card }
    func strip() -> Strip? { facts }
    func settings() -> SettingsView? { offered }
    func frame() -> ChatFrame? { current }

    func takeChanges() -> ChatChanges {
        defer { pending = ChatChanges(keys: [], reloaded: false, session: false) }
        return pending
    }

    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        sent.append(draft)
        return .success(SendOutcome(inputId: [1], state: .sent))
    }

    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    func answer(_ ask: String, picks: [Pick], note: String?) async -> ActOutcome? { .done }
    func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome? { .done }

    func withdraw(_ input: [UInt8]) async -> ActOutcome? {
        withdrawn.append(input)
        return .done
    }

    func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) { discarded.append(input) }
    func draft(of input: [UInt8]) -> Draft? { drafts[input] }

    func interrupt() async -> ActOutcome? {
        interrupts += 1
        return .done
    }

    func change(_ setting: SettingChange) async -> ActOutcome? {
        changed.append(setting)
        return .done
    }

    func resume(with draft: Draft) async -> ActOutcome? {
        resumed.append(draft)
        return .done
    }

    func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        paged.append(rows)
        let page = Array(earlier.suffix(Int(rows)))
        earlier.removeLast(page.count)
        ordered.insert(contentsOf: page, at: 0)
        pending.keys.append(contentsOf: page.map(\.id))
        return .arrived(UInt32(page.count))
    }

    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .success(BlobRef(hash: [9], name: name, mime: mime, size: UInt64(data.count)))
    }

    func blob(_ hash: [UInt8]) -> Data? { nil }

    func review() async -> Result<FrozenReview, RuntimeFailure> {
        reviews += 1
        guard let working else { return .failure(RuntimeFailure("no machine")) }
        return .success(working)
    }
}

private func row(_ id: String, _ order: UInt64, _ text: String = "", run: RunInfo? = nil) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(text.isEmpty ? id : text)], streaming: false, workingNote: false),
        collapsed: false, attention: false, decision: nil, parent: nil, run: run)
}

private func frame(
    mode: Composer = .send, caughtUp: Bool = true, hasOlder: Bool = false,
    phase: PhaseView = .idle, kind: Kind = .claudeSdk
) -> ChatFrame {
    ChatFrame(
        agent: AgentKey(host: [1], agent: [2]), name: "a", kind: kind, phase: phase,
        composer: ComposerView(mode: mode, activity: nil), connection: .live, caughtUp: caughtUp,
        hasOlder: hasOlder, queue: [], outbox: [], askInput: nil, ended: nil, waiting: nil)
}

/// Prose rows "m<first>" through "m<last>", in order.
private func numbered(_ numbers: ClosedRange<Int>) -> [Row] {
    numbers.map { row("m\($0)", UInt64($0)) }
}

/// Waits for the model's acts, which run on the main actor after a hop.
@MainActor
private func settle() async {
    for _ in 0..<20 { await Task.yield() }
}

@MainActor
final class ChatModelTests: XCTestCase {
    func testOpeningReadsTheKeysAndReadsARowOnlyWhenItsCellIsDrawn() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        XCTAssertEqual(model.ids, ["a", "b", "c"])
        XCTAssertTrue(source.fetched.isEmpty, "no row is read before its cell is drawn")
        XCTAssertEqual(model.cell(for: "b").row?.id, "b")
        XCTAssertEqual(model.cell(for: "b").row?.id, "b")
        XCTAssertEqual(source.fetched, [["b"]], "a drawn cell is not read again unchanged")
    }

    func testNewRowsExtendTheSequenceAtItsEdgesAndOnlyChangedDrawnCellsAreReadAgain() {
        let source = FakeChat(rows: [row("b", 2), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        let b = model.cell(for: "b")
        _ = model.cell(for: "c")
        source.fetched = []

        // A streamed revision of c and a new row d above the newest.
        source.ordered = [row("b", 2), row("c", 3, "c, longer"), row("d", 4)]
        source.pending = ChatChanges(keys: ["c", "d"], reloaded: false, session: false)
        model.woke()
        XCTAssertEqual(model.ids, ["b", "c", "d"])
        XCTAssertEqual(source.fetched, [["c"]], "only the drawn cell that changed is read")
        XCTAssertEqual(model.cell(for: "c").row?.kind, row("c", 3, "c, longer").kind)
        XCTAssertTrue(model.cell(for: "b") === b, "an unchanged cell keeps its identity")

        // A page of older rows below the oldest.
        source.ordered.insert(row("a", 1), at: 0)
        source.pending = ChatChanges(keys: ["a"], reloaded: false, session: false)
        model.woke()
        XCTAssertEqual(model.ids, ["a", "b", "c", "d"])
    }

    func testAResetSwapsTheWholeSequenceInAtOnceAndFollowsTheNewestRow() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2)], frame: frame())
        let model = ChatModel(source: source)
        let b = model.cell(for: "b")
        let before = model.toNewest

        source.ordered = [row("b", 20, "b again"), row("x", 21), row("y", 22)]
        source.pending = ChatChanges(keys: [], reloaded: true, session: true)
        model.woke()
        XCTAssertEqual(model.ids, ["b", "x", "y"])
        XCTAssertTrue(model.cell(for: "b") === b, "a kept key keeps its cell")
        XCTAssertEqual(model.cell(for: "b").row?.order, 20, "and the cell holds the new row")
        XCTAssertEqual(model.toNewest, before + 1)
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.drawn, ["b", "x", "y"])
    }

    func testAResetInHistoryDrawsTheNewestRowsAndFollowsThem() {
        let source = FakeChat(rows: numbered(1...600), frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        model.reachedTop()
        source.ordered = numbered(1...700)
        source.pending = ChatChanges(keys: [], reloaded: true, session: true)
        model.woke()
        XCTAssertTrue(model.following)
        XCTAssertEqual(model.drawn.first, "m\(701 - ChatModel.drawnRows)")
        XCTAssertEqual(model.drawn.last, "m700")
    }

    func testRowsArrivingBelowAReaderWhoScrolledUpAreOfferedRatherThanFollowed() {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        let before = model.toNewest
        source.ordered.append(row("b", 2))
        source.pending = ChatChanges(keys: ["b"], reloaded: false, session: false)
        model.woke()
        XCTAssertTrue(model.newActivity)
        XCTAssertEqual(model.toNewest, before, "the list is not moved under the reader")
        model.jumpToNewest()
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.toNewest, before + 1)
    }

    // MARK: - The drawn run

    /// While the reader follows, the list draws the newest rows up to the
    /// cap; everything else stays held.
    func testFollowingDrawsAtMostTheCapThroughFiveThousandArrivals() {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        var widest = 0
        for n in 1...5_000 {
            source.ordered.append(row("m\(n)", UInt64(n)))
            source.pending = ChatChanges(keys: ["m\(n)"], reloaded: false, session: false)
            model.woke()
            widest = max(widest, model.drawn.count)
        }
        XCTAssertEqual(widest, ChatModel.drawnRows)
        XCTAssertEqual(model.ids.count, 5_000, "every row stays held")
        XCTAssertEqual(model.drawn, (5_001 - ChatModel.drawnRows...5_000).map { "m\($0)" })

        // Scrolling up draws the held rows again without asking for them.
        model.reading(atNewest: false)
        model.reachedTop()
        XCTAssertEqual(model.drawn.first, "m\(5_001 - ChatModel.drawnRows - ChatModel.drawnStep)")
        XCTAssertTrue(source.paged.isEmpty)
    }

    /// In history, arrivals are held but not drawn and New activity shows;
    /// dragging down takes in held rows until the head, where following
    /// resumes, all without a fetch.
    func testArrivalsWhileReadingAreHeldAndReturningDownLandsOnTheHead() {
        let source = FakeChat(rows: numbered(1...600), frame: frame(hasOlder: true))
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        for _ in 0..<3 {
            model.reachedTop()
        }
        let reading = model.drawn
        XCTAssertEqual(reading.count, ChatModel.drawnRows)
        XCTAssertEqual(reading.first, "m\(601 - ChatModel.drawnRows - 3 * ChatModel.drawnStep)")
        let toNewest = model.toNewest

        for n in 601...650 {
            source.ordered.append(row("m\(n)", UInt64(n)))
            source.pending = ChatChanges(keys: ["m\(n)"], reloaded: false, session: false)
            model.woke()
        }
        XCTAssertEqual(model.drawn, reading, "arrivals leave the drawn run alone")
        XCTAssertTrue(model.newActivity)
        XCTAssertEqual(model.toNewest, toNewest, "the list is not moved under the reader")
        XCTAssertEqual(model.ids.count, 650)

        // The reader drags down: each time the bottom of the drawn run is
        // reached, held rows below are taken in, and past the cap rows leave
        // at the top.
        var steps = 0
        while model.drawn.last != "m650" {
            let top = model.drawn.first
            model.reading(atNewest: true)
            XCTAssertFalse(model.following, "the bottom of the drawn run is not the head")
            XCTAssertEqual(model.drawn.count, ChatModel.drawnRows)
            XCTAssertNotEqual(model.drawn.first, top)
            steps += 1
            XCTAssertLessThan(steps, 10)
        }
        model.reading(atNewest: true)
        XCTAssertTrue(model.following)
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.drawn.last, "m650")
        XCTAssertLessThanOrEqual(model.drawn.count, ChatModel.drawnRows)
        XCTAssertTrue(source.paged.isEmpty, "coming back down asks for nothing")
    }

    /// New activity draws the newest run at the live head, however far up
    /// the reader was.
    func testNewActivityTakesTheReaderToTheNewestRun() {
        let source = FakeChat(rows: numbered(1...600), frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        model.reachedTop()
        source.ordered.append(contentsOf: numbered(601...1_000))
        source.pending = ChatChanges(keys: (601...1_000).map { "m\($0)" }, reloaded: false, session: false)
        model.woke()
        XCTAssertTrue(model.newActivity)
        let toNewest = model.toNewest
        model.jumpToNewest()
        XCTAssertEqual(model.drawn, (1_001 - ChatModel.drawnRows...1_000).map { "m\($0)" })
        XCTAssertTrue(model.following)
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.toNewest, toNewest + 1)
    }

    /// The top of the drawn run takes in held rows first, and a page is
    /// asked for only once fewer than a page of held rows are left above it.
    func testTheTopTakesInHeldRowsBeforeAnyPageIsAsked() async {
        let source = FakeChat(rows: numbered(1...600), frame: frame(hasOlder: true))
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        XCTAssertEqual(model.drawn.first, "m\(601 - ChatModel.drawnRows)")
        XCTAssertFalse(model.drawsOldestHeld)
        var arrivals = 0
        while !model.drawsOldestHeld {
            let bottom = model.drawn.last
            model.reachedTop()
            arrivals += 1
            await settle()
            if !model.drawsOldestHeld {
                XCTAssertTrue(source.paged.isEmpty, "held rows are drawn before any page is asked")
            }
            XCTAssertEqual(model.drawn.count, ChatModel.drawnRows, "rows past the cap leave at the bottom")
            XCTAssertNotEqual(model.drawn.last, bottom)
        }
        XCTAssertEqual(arrivals, 5)
        XCTAssertEqual(model.drawn.first, "m1")
        XCTAssertEqual(model.drawn.count, ChatModel.drawnRows)
        XCTAssertEqual(source.paged, [ChatModel.pageRows], "one page, asked as the last held rows were taken in")
        XCTAssertEqual(model.ids.count, 600, "every row stays held")
    }

    /// A reader resting at the top pages ahead once per arrival there, and a
    /// page landing while the reader waits at the oldest row is drawn,
    /// never cascading into the next page.
    func testOnePageIsAskedPerArrivalAtTheTop() async {
        let source = FakeChat(rows: numbered(1_001...1_020), frame: frame(hasOlder: true))
        source.earlier = numbered(1...1_000)
        let model = ChatModel(source: source)
        model.reading(atNewest: false)

        model.reachedTop()
        await settle()
        XCTAssertEqual(source.paged.count, 1)
        XCTAssertEqual(model.drawn.first, "m961", "the page the reader waited for is drawn")
        for _ in 0..<5 { await settle() }
        XCTAssertEqual(source.paged.count, 1, "a page landing asks for no other")

        model.reachedTop()
        await settle()
        XCTAssertEqual(source.paged.count, 2)
        XCTAssertEqual(model.drawn.first, "m921")
        XCTAssertEqual(model.ids.count, 100)
    }

    /// The last drawn row's rail reads the row held below it, drawn or not.
    func testTheRowBelowTheDrawnRunIsReadFromTheHeldRows() {
        let source = FakeChat(rows: numbered(1...300), frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        XCTAssertNil(model.drawnRow(below: "m300"))
        source.ordered.append(row("m301", 301))
        source.pending = ChatChanges(keys: ["m301"], reloaded: false, session: false)
        model.woke()
        XCTAssertEqual(model.drawn.last, "m300")
        XCTAssertEqual(model.drawnRow(below: "m300")?.id, "m301")
    }

    func testAPageIsTheUsualSizeExceptAtACollapsedRunThatContinuesBelow() async {
        let run = RunInfo(
            newest: "r", oldest: "q", reads: 300, searches: 0, len: 300, anchor: "", isSummary: true,
            openBelow: true)
        let source = FakeChat(rows: [row("r", 5, run: run), row("s", 6)], frame: frame(hasOlder: true))
        let model = ChatModel(source: source)
        _ = model.cell(for: "r")
        model.reachedTop()
        await settle()
        XCTAssertEqual(source.paged, [300])

        let huge = RunInfo(
            newest: "r", oldest: "q", reads: 5_000, searches: 0, len: 5_000, anchor: "",
            isSummary: true, openBelow: true)
        source.ordered[0] = row("r", 5, run: huge)
        source.pending = ChatChanges(keys: ["r"], reloaded: false, session: false)
        model.woke()
        model.reachedTop()
        await settle()
        XCTAssertEqual(source.paged.last, ChatModel.largestPage)

        let plain = FakeChat(rows: [row("a", 1)], frame: frame(hasOlder: true))
        let other = ChatModel(source: plain)
        _ = other.cell(for: "a")
        other.reachedTop()
        await settle()
        XCTAssertEqual(plain.paged, [ChatModel.pageRows])
    }

    func testTheRowDrawnBelowPassesOverCollapsedAndEmptyRows() {
        var folded = row("b", 2)
        folded.collapsed = true
        var empty = row("c", 3)
        empty.kind = .hidden
        let source = FakeChat(
            rows: [row("a", 1), folded, empty, row("d", 4)], frame: frame())
        let model = ChatModel(source: source)
        XCTAssertEqual(model.drawnRow(below: "a")?.id, "d")
        XCTAssertNil(model.drawnRow(below: "d"))
    }

    func testNothingIsAskedForWhenNoOlderHistoryExists() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame(hasOlder: false))
        let model = ChatModel(source: source)
        model.reachedTop()
        await settle()
        XCTAssertTrue(source.paged.isEmpty)
    }

    func testTheDraftIsKeptWhileCatchingUpAndSendsOnlyWhenCaughtUpAndLive() async {
        let source = FakeChat(rows: [], frame: frame(mode: .disabled(.catchingUp), caughtUp: false))
        let model = ChatModel(source: source)
        model.draft = "Run the tests."
        XCTAssertFalse(model.canSend)
        model.send()
        await settle()
        XCTAssertTrue(source.sent.isEmpty)
        XCTAssertEqual(model.draft, "Run the tests.")

        source.current = frame()
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        XCTAssertTrue(model.canSend)
        model.send()
        await settle()
        XCTAssertEqual(source.sent.map(\.text), ["Run the tests."])
        XCTAssertEqual(model.draft, "")
    }

    func testAnExitedAgentResumesWithTheDraft() async {
        let source = FakeChat(rows: [], frame: frame(mode: .resume, phase: .exited(cause: nil)))
        let model = ChatModel(source: source)
        model.draft = "Carry on."
        XCTAssertFalse(model.canSend)
        XCTAssertTrue(model.canResume)
        model.resume()
        await settle()
        XCTAssertEqual(source.resumed.map(\.text), ["Carry on."])
        XCTAssertEqual(model.draft, "")
    }

    func testAWithdrawnPromptComesBackToTheDraft() async {
        let source = FakeChat(rows: [], frame: frame(phase: .working))
        source.drafts[[7]] = Draft(text: "Then run the Windows check.", attachments: nil)
        let model = ChatModel(source: source)
        let queued = QueuedRow(
            inputId: [7], text: [.text("Then run the Windows check.")], mine: true, steered: false,
            canWithdraw: true, canSendNow: true, fromAgent: nil)
        model.withdraw(queued)
        await settle()
        XCTAssertEqual(source.withdrawn, [[7]])
        XCTAssertEqual(model.draft, "Then run the Windows check.")
    }

    /// Withdrawn, a prompt gives back every attachment whole: a pasted text
    /// with all its words, a review with its comments and patch, not the
    /// line count and label its queued row shows.
    func testAWithdrawnPromptGivesBackItsPastedTextAndReview() async {
        let source = FakeChat(rows: [], frame: frame(phase: .working))
        let pasted = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        let patch = BlobRef(hash: [4, 2], name: "patch", mime: "text/x-diff", size: 120)
        let review = DraftAttachment.review(
            diff: Diff(head: "abc123", base: nil, mergeBase: nil, patch: patch),
            comments: [ReviewComment(path: "src/lib.rs", line: 12, oldLine: 0, text: "Name this.")])
        source.drafts[[7]] = Draft(text: "Look at these.", attachments: [pasted, review])
        let model = ChatModel(source: source)
        model.draft = "half written"
        let queued = QueuedRow(
            inputId: [7],
            text: [
                .text("Look at these."),
                .attachment(.text(name: "Pasted text", lines: 3)),
            ],
            mine: true, steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil)
        model.withdraw(queued)
        await settle()
        XCTAssertEqual(source.withdrawn, [[7]])
        XCTAssertEqual(model.draft, "Look at these.\nhalf written")
        XCTAssertEqual(model.attachments, [pasted, review])
    }

    /// Editing a rejected prompt gives back its words and its photo, and
    /// forgets the rejected input.
    func testEditingARejectedPromptGivesBackItsPhoto() async {
        let source = FakeChat(rows: [], frame: frame())
        let photo = BlobRef(hash: [5], name: "screen.png", mime: "image/png", size: 2048)
        source.drafts[[8]] = Draft(text: "What is this?", attachments: [.image(photo)])
        let model = ChatModel(source: source)
        let rejected = OutboxRow(
            inputId: [8], text: [.text("What is this?"), .attachment(.image(photo))],
            state: .rejected("the agent is busy"))
        model.edit(rejected)
        await settle()
        XCTAssertEqual(model.draft, "What is this?")
        XCTAssertEqual(model.attachments, [.image(photo)])
        XCTAssertEqual(source.discarded, [[8]])
    }

    func testAPickIsSentAndTheCurrentValueMovesOnlyWhenTheAgentReportsIt() async {
        func offer(current: String) -> SettingsView {
            let models = ["opus", "sonnet"].map { value in
                ModelChoice(
                    value: value, displayName: value, description: "", efforts: [],
                    current: value == current, reported: false, defaultEffort: nil)
            }
            return SettingsView(
                models: models, efforts: [], modes: [], cycleMode: false, commands: [],
                changeByTyping: nil, effortRefusal: nil, modeRefusal: nil, modelRefusal: nil)
        }
        func strip(model: String) -> Strip {
            Strip(
                failedServers: [], background: nil, context: nil, effort: nil, mode: nil,
                model: model, signIn: nil, tasks: nil, usage: nil, workingOn: nil)
        }
        let source = FakeChat(rows: [], frame: frame())
        source.offered = offer(current: "opus")
        source.facts = strip(model: "opus")
        let model = ChatModel(source: source)
        model.change(.model("sonnet"))
        await settle()
        XCTAssertEqual(source.changed, [.model("sonnet")])
        XCTAssertEqual(model.settings?.models.first { $0.current }?.value, "opus", "not before the agent says so")
        XCTAssertEqual(model.strip?.model, "opus")
        source.offered = offer(current: "sonnet")
        source.facts = strip(model: "sonnet")
        source.pending.session = true
        model.woke()
        XCTAssertEqual(model.settings?.models.first { $0.current }?.value, "sonnet")
        XCTAssertEqual(model.strip?.model, "sonnet")
    }

    private func offering(_ names: [String]) -> SettingsView {
        SettingsView(
            models: [], efforts: [], modes: [], cycleMode: false,
            commands: names.map { CommandView(name: $0, description: "", argumentHint: "", source: "") },
            changeByTyping: nil, effortRefusal: nil, modeRefusal: nil, modelRefusal: nil)
    }

    func testALeadingSlashWordListsUpToFiveMatchingCommandsAndAPickFillsTheDraft() {
        let source = FakeChat(rows: [], frame: frame())
        source.offered = offering([
            "clear", "compact", "context", "cost", "config-check", "code-review:review",
            "documents:documents", "init",
        ])
        let model = ChatModel(source: source)
        model.draft = "/c"
        XCTAssertEqual(
            model.slashMatches.map(\.name), ["clear", "compact", "context", "cost", "config-check"],
            "five at most, in the agent's order")
        model.draft = "/Doc"
        XCTAssertEqual(model.slashMatches.map(\.name), ["documents:documents"])
        model.draft = "/rev"
        XCTAssertEqual(model.slashMatches.map(\.name), ["code-review:review"], "after a plugin's namespace")
        for draft in ["/compact now", "please /c", "c", "/a/b", ""] {
            model.draft = draft
            XCTAssertTrue(model.slashMatches.isEmpty, "\(draft) is not a leading /word")
        }
        model.draft = "/comp"
        model.pick(model.slashMatches[0])
        XCTAssertEqual(model.draft, "/compact ")
        XCTAssertTrue(model.slashMatches.isEmpty)
    }

    func testACodexSkillIsPickedAsItsMention() {
        let source = FakeChat(rows: [], frame: frame(kind: .codex))
        source.offered = offering(["autopilot"])
        let model = ChatModel(source: source)
        model.draft = "/auto"
        model.pick(model.slashMatches[0])
        XCTAssertEqual(model.draft, "$autopilot ")
    }

    func testAnAgentOfferingNoCommandsListsNothing() {
        let source = FakeChat(rows: [], frame: frame(kind: .claudePty))
        source.offered = offering([])
        let model = ChatModel(source: source)
        model.draft = "/model"
        XCTAssertTrue(model.slashMatches.isEmpty)
    }

    func testASpaceAddedWherePastedTextMeetsAWordStaysInTheSentence() {
        let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
        model.type("Please check")
        let log = (1...12).map { "line \($0)" }.joined(separator: "\n")
        model.type("Please check " + log)
        XCTAssertEqual(model.draft, "Please check ")
        XCTAssertEqual(model.attachments, [.text(name: "Pasted text", text: log)])
        model.type("Please check " + log + "against")
        XCTAssertEqual(model.draft, "Please check against")
        XCTAssertEqual(model.attachments.count, 1)
    }

    func testALongPasteBecomesOnePastedTextAttachmentSentInline() async {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        model.type("Look at this: ")
        let log = (1...12).map { "line \($0)" }.joined(separator: "\r\n") + "\r\n"
        model.type("Look at this: " + log)
        XCTAssertEqual(model.draft, "Look at this: ", "the paste leaves the draft")
        let text = (1...12).map { "line \($0)" }.joined(separator: "\n") + "\n"
        XCTAssertEqual(model.attachments, [.text(name: "Pasted text", text: text)])
        XCTAssertEqual(ChatModel.lines(text), 12)
        model.type("Look at this: " + log)
        XCTAssertEqual(model.attachments.count, 1, "the text view's echo of the paste is not a second paste")
        XCTAssertEqual(model.draft, "Look at this: ")
        model.type("Look at this: " + log + "now")
        XCTAssertEqual(model.attachments.count, 1, "typing while the field still shows the paste")
        XCTAssertEqual(model.draft, "Look at this: now")
        model.type("Look at this: two\nlines")
        XCTAssertEqual(model.draft, "Look at this: two\nlines", "a short paste stays words")
        XCTAssertEqual(model.attachments.count, 1)
        model.type(model.draft + String(repeating: "x", count: ChatModel.pasteCharacters))
        XCTAssertEqual(model.attachments.count, 2, "one very long line is a paste too")
        model.send()
        await settle()
        XCTAssertEqual(source.sent.first?.text, "Look at this: two\nlines")
        XCTAssertEqual(source.sent.first?.attachments?.first, .text(name: "Pasted text", text: text))
    }

    func testAnEmptyChatSaysItIsLoadingOnlyAfterAMoment() async throws {
        let source = FakeChat(rows: [], frame: frame(caughtUp: false))
        let model = ChatModel(source: source)
        XCTAssertFalse(model.loadingHint)
        try await Task.sleep(for: ChatModel.loadingHintDelay + .milliseconds(200))
        XCTAssertTrue(model.loadingHint)
        source.ordered = [row("a", 1)]
        source.pending = ChatChanges(keys: ["a"], reloaded: false, session: false)
        model.woke()
        XCTAssertFalse(model.loadingHint)
    }

    func testARunOpensByAnyOfItsKeysAndStaysOpenAsItGrows() {
        let run = RunInfo(
            newest: "b", oldest: "a", reads: 2, searches: 0, len: 2, anchor: "", isSummary: true,
            openBelow: false)
        let source = FakeChat(rows: [row("a", 1, run: run), row("b", 2, run: run)], frame: frame())
        let model = ChatModel(source: source)
        _ = model.cell(for: "a")
        _ = model.cell(for: "b")
        model.toggle("b")
        XCTAssertTrue(model.isExpanded("b"))
        XCTAssertEqual(model.options, RowOptions(tools: .collapseRuns(expanded: ["b"])))

        let grown = RunInfo(
            newest: "c", oldest: "a", reads: 3, searches: 0, len: 3, anchor: "", isSummary: false,
            openBelow: false)
        source.ordered = [row("a", 1, run: grown), row("b", 2, run: grown), row("c", 3, run: grown)]
        source.pending = ChatChanges(keys: ["a", "b", "c"], reloaded: false, session: false)
        model.woke()
        _ = model.cell(for: "c")
        XCTAssertTrue(model.isExpanded("c"), "the new summary is open by the key it was opened by")
        model.toggle("c")
        XCTAssertFalse(model.isExpanded("c"))
        XCTAssertTrue(model.expanded.isEmpty)
    }

    /// Picks on a question card not yet sent stay with the chat, as the
    /// draft does, until the ask closes.
    func testAQuestionCardsPicksStayUntilItsAskCloses() async {
        func card(_ key: String) -> AskCard {
            AskCard(
                kind: .claudeSdk, key: key, itemKey: "", position: 1, count: 1, body: .question([]),
                choices: [], questionNote: true, state: .open)
        }
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.card = card("ask:1")
        let model = ChatModel(source: source)
        await settle()
        let picked = QuestionDraft(
            step: 1, picks: [[2], []], others: [nil, "Only on staging"], highlighted: [2, nil],
            note: "and say when")
        model.keep(picked, onAsk: "ask:1")
        model.keep(picked, onAsk: "ask:2")
        XCTAssertNil(model.questionDraft(onAsk: "ask:2"), "only the head ask keeps progress")

        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertEqual(model.questionDraft(onAsk: "ask:1"), picked, "the ask is still open")

        source.card = card("ask:2")
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertNil(model.questionDraft(onAsk: "ask:1"), "the ask closed")

        source.card = card("ask:1")
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertNil(model.questionDraft(onAsk: "ask:1"), "gone for good once closed")
    }

    func testTheWorkingTreeIsAskedAboutOnceCurrentAndAgainWhenATurnEnds() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame(caughtUp: false))
        source.working = ReviewFixtures.frozen
        let model = ChatModel(source: source)
        await settle()
        XCTAssertEqual(source.reviews, 0, "nothing is asked before the chat is current")
        XCTAssertNil(model.changes)

        source.current = frame(phase: .working)
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertEqual(source.reviews, 1, "caught up: the chip is counted once")
        XCTAssertEqual(model.changes?.files, 3)
        XCTAssertEqual(model.changes?.added, 4)
        XCTAssertEqual(model.changes?.removed, 2)

        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertEqual(source.reviews, 1, "a turn still running changes nothing")

        source.current = frame(phase: .idle)
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        model.woke()
        await settle()
        XCTAssertEqual(source.reviews, 2, "the turn ended: the edits settled")
    }

    func testAnAgentWithNoChangesShowsNoChip() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.working = FrozenReview(diff: ReviewFixtures.frozen.diff, patch: "")
        let model = ChatModel(source: source)
        await settle()
        XCTAssertEqual(source.reviews, 1)
        XCTAssertNil(model.changes)
    }

    func testAnAttachedReviewIsOneDraftTokenThatSendsWithItsPatch() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.working = ReviewFixtures.frozen
        let model = ChatModel(source: source)
        await settle()
        let changes = try! XCTUnwrap(model.changes)
        let review = model.review(of: changes)
        XCTAssertTrue(review === model.review(of: changes), "the review being written is kept")
        review.begin(at: ReviewLine(file: 0, hunk: 0, line: 1))
        review.comment("Keep the old name.")
        model.attach(review)
        review.begin(at: ReviewLine(file: 1, hunk: 0, line: 0))
        review.comment("Say why.")
        model.attach(review)
        XCTAssertEqual(model.attachments.count, 1, "a second attach replaces the first token")
        guard case .review(let diff, let comments)? = model.attachments.first else {
            return XCTFail("the draft holds no review")
        }
        XCTAssertEqual(diff, ReviewFixtures.frozen.diff)
        XCTAssertEqual(comments.map(\.text), ["Keep the old name.", "Say why."])

        model.send()
        await settle()
        XCTAssertEqual(source.sent.first?.attachments, [.review(diff: ReviewFixtures.frozen.diff, comments: comments)])
    }

    func testDictationStreamsIntoTheDraftAndStopsOnSend() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let model = ChatModel(source: source)
        model.draft = "Please"
        model.dictation.prepare(speech: .allowed, microphone: .allowed, available: true)
        model.dictation.began(draft: model.draft)
        model.heard("run the")
        model.heard("rerun the tests")
        XCTAssertEqual(model.draft, "Please rerun the tests")
        model.send()
        XCTAssertFalse(model.dictation.active, "sending ends dictation")
        await settle()
        XCTAssertEqual(source.sent.map(\.text), ["Please rerun the tests"])
        model.heard("late words")
        XCTAssertEqual(model.draft, "", "nothing heard after the send lands in the next draft")
    }

    func testAPushPayloadNamesTheAgentAndItsHost() {
        let host = UUID(uuidString: "11111111-2222-3333-4444-555555555555")!
        let agent = UUID(uuidString: "66666666-7777-8888-9999-000000000000")!
        let payload: [AnyHashable: Any] = [
            "aps": ["content-available": 1],
            "amux": ["host": host.uuidString, "agent": agent.uuidString],
        ]
        XCTAssertEqual(PushPayload.agent(payload), AgentKey(host: HostId(host), agent: agent))
        XCTAssertNil(PushPayload.agent(["aps": ["content-available": 1]]))
    }
}
