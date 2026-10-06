import AmuxCore
import AmuxDesign
import SwiftUI
import UIKit
import XCTest
@testable import AmuxFeatures

/// The list's own behaviour, laid out in a collection view without a window.
@MainActor
final class TranscriptListTests: XCTestCase {
    func testTheTopIsReachedPastCollapsedRows() {
        // A long collapsed tool run leaves the window beginning with many
        // rows that stand at no height. The list judges the top by row
        // index, which holds because the collection view displays a cell
        // at no height like any other, so the oldest rows coming on screen
        // are the oldest rows; a review claimed otherwise, and this pins
        // what was found.
        let collapsed = (0..<25).map { row("c\($0)", UInt64($0), collapsed: true) }
        let shown = (25..<85).map { row("m\($0)", UInt64($0)) }
        let chat = StubChat(rows: collapsed + shown, hasOlder: true)
        let model = ChatModel(source: chat)
        model.reading(atNewest: false)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)

        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()

        XCTAssertEqual(model.paging, .fetching, "the reader at the top was not given older history")
    }

    func testATypeSizeChangeMeasuresTheRowsAgain() {
        // A hosted row takes the type size the screen carries in; when it
        // changes, in Settings or by the driving door, every cached height
        // is stale and the rows are measured again at the new size.
        let chat = StubChat(rows: (0..<10).map { row("m\($0)", UInt64($0)) }, hasOlder: false)
        let model = ChatModel(source: chat)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)
        let items = model.ids.map { ListItem.row($0) }
        list.update(
            items: items, notices: [], environment: environment(), revision: model.revision,
            toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let before = list.view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!.frame.height

        list.update(
            items: items, notices: [], environment: environment(typeSize: .accessibility3),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let after = list.view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!.frame.height

        XCTAssertGreaterThan(after, before, "the row kept its height at the old size")
    }
}

private func environment(typeSize: DynamicTypeSize = .large) -> CellEnvironment {
    CellEnvironment(
        design: .app, photographed: true, reducesMotion: true, reducesTransparency: true,
        hidesNeedsYouDot: false, reportsElements: false, reportedPrefix: nil,
        reportsGeometry: false, dynamicTypeSize: typeSize)
}

private func row(_ id: String, _ order: UInt64, collapsed: Bool = false) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(id)], streaming: false, workingNote: false),
        collapsed: collapsed, attention: false, decision: nil, parent: nil, run: nil)
}

/// A chat that holds its rows and answers nothing else.
private final class StubChat: ChatSource, @unchecked Sendable {
    let ordered: [Row]
    let hasOlder: Bool

    init(rows: [Row], hasOlder: Bool) {
        ordered = rows
        self.hasOlder = hasOlder
    }

    func keys() -> [String] { ordered.map(\.id) }
    func keys(above newest: String) -> [String]? { nil }
    func keys(below oldest: String) -> [String]? { nil }
    func oldestKey() -> String? { ordered.first?.id }
    func follow(_ following: Bool) {}
    func rows(for keys: [String], options: RowOptions?) -> [Row] {
        let wanted = Set(keys)
        return ordered.filter { wanted.contains($0.id) }
    }
    func askCard() -> AskCard? { nil }
    func overview() -> Overview? { nil }
    func settings() -> SettingsView? { nil }
    func frame() -> ChatFrame? {
        ChatFrame(
            agent: AgentKey(host: [1], agent: [2]), name: "a", kind: .claudeSdk, phase: .idle,
            composer: ComposerView(mode: .send, activity: nil), connection: .live, caughtUp: true,
            hasOlder: hasOlder, arrivalsHeld: false, queue: [], outbox: [], askInput: nil,
            context: nil, effort: nil, ended: nil, git: nil, mode: nil, model: nil, permission: nil, signIn: nil, waiting: nil)
    }
    func takeChanges() -> ChatChanges { ChatChanges(keys: [], reloaded: false, session: false) }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("stub")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { nil }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { nil }
    func toggleRun(_ member: String, open: [String]) -> [String] {
        open.contains(member) ? open.filter { $0 != member } : open + [member]
    }
    func keepOpenRuns(_ open: [String]) -> [String] { open }
    func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome? { nil }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { nil }
    func draft(of input: [UInt8]) -> Draft? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { nil }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func interrupt() async -> ActOutcome? { nil }
    func change(_ setting: SettingChange) async -> ActOutcome? { nil }
    func resume(with draft: Draft) async -> ActOutcome? { nil }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { nil }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("stub"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> { .failure(RuntimeFailure("stub")) }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> { .failure(RuntimeFailure("no")) }
}
