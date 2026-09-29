import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest
@testable import Amux

/// The chat's list on screen while the run of rows it draws changes at
/// either end: the rows the reader is looking at stay exactly where they are.
@MainActor
final class ChatFeedTests: XCTestCase {
    private var window: UIWindow?
    private let probe = Probe()

    override func tearDown() async throws {
        window?.isHidden = true
        window?.rootViewController = nil
        window = nil
    }

    /// What the list reported last: each prose row's text and where it is.
    @MainActor
    private final class Probe {
        var elements: [IdentifiedElement] = []
    }

    /// Messages of a few different lengths, so rows are not all one height.
    private static func messages(_ numbers: ClosedRange<Int>) -> [Row] {
        numbers.map { n in
            let words = "message \(n): the agent reports progress on its task"
                + String(repeating: " and then some more", count: n % 4 * 3)
            return ScriptedChat.row(
                "m\(n)", UInt64(n), .prose(text: [.text(words)], streaming: false, workingNote: false))
        }
    }

    private func show(_ model: ChatModel) throws {
        let probe = self.probe
        let root = ChatScreen(
            model: model,
            subject: ChatSubject(
                name: "flood", host: "desk", directory: "~/src", presence: .online, away: nil)
        ) { _ in }
            .reportingIdentifiedElements(prefix: "chat.row.prose")
            .onPreferenceChange(IdentifiedElements.self) { declared in
                MainActor.assumeIsolated { probe.elements = declared }
            }
        let controller = UIHostingController(rootView: root)
        let scene = try XCTUnwrap(
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window
    }

    private func spin(_ seconds: Double = 0.4) async {
        try? await Task.sleep(for: .seconds(seconds))
    }

    /// The chat's scroll view: the tallest one that is not a text field.
    private func list() throws -> UIScrollView {
        var found: [UIScrollView] = []
        func walk(_ view: UIView) {
            if let scroll = view as? UIScrollView, !(view is UITextView) { found.append(scroll) }
            view.subviews.forEach(walk)
        }
        walk(try XCTUnwrap(window))
        return try XCTUnwrap(found.max { $0.bounds.height < $1.bounds.height })
    }

    /// Where each prose row on screen is, by its text.
    private func onScreen() throws -> [String: CGFloat] {
        let bounds = try XCTUnwrap(window).bounds
        var rows: [String: CGFloat] = [:]
        for element in probe.elements where element.frame.intersects(bounds) {
            rows[element.label ?? ""] = element.frame.minY
        }
        return rows
    }

    private func assertStill(
        _ before: [String: CGFloat], _ what: String, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let after = try onScreen()
        let kept = before.keys.filter { after[$0] != nil }
        XCTAssertGreaterThan(kept.count, 3, "\(what): the rows on screen went", file: file, line: line)
        for label in kept {
            XCTAssertEqual(
                after[label]!, before[label]!, accuracy: 0.5, "\(what) moved \(label.prefix(12))",
                file: file, line: line)
        }
    }

    /// The pixels of the rows between the header and the composer, as the
    /// screen shows them.
    private func rowsBand() throws -> Data {
        let window = try XCTUnwrap(self.window)
        let band = CGRect(x: 0, y: 150, width: window.bounds.width, height: 550)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let image = UIGraphicsImageRenderer(size: band.size, format: format).image { _ in
            window.drawHierarchy(
                in: CGRect(origin: CGPoint(x: 0, y: -band.minY), size: window.bounds.size),
                afterScreenUpdates: true)
        }
        return try XCTUnwrap(image.cgImage?.dataProvider?.data as Data?)
    }

    /// Up into history, moved the way the door moves it.
    private func scrollUp(_ list: UIScrollView, _ model: ChatModel, screens: CGFloat) async {
        model.reading(atNewest: false)
        list.setContentOffset(
            CGPoint(x: 0, y: max(-list.adjustedContentInset.top, list.contentOffset.y - screens * list.bounds.height)),
            animated: false)
        await spin()
    }

    func testRowsTakenInOrLetGoAtEitherEndMoveNothingOnScreen() async throws {
        let source = ScriptedChat(rows: Self.messages(1...600), frame: ScriptedChat.frame(hasOlder: false))
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        XCTAssertEqual(model.drawn.count, ChatModel.drawnRows)
        let list = try list()
        // Where a reader scrolling up stands just before the top of the
        // drawn run comes near: about thirty rows below it.
        model.reading(atNewest: false)
        list.setContentOffset(CGPoint(x: 0, y: list.contentSize.height * 30 / 240), animated: false)
        await spin()
        XCTAssertFalse(model.following)
        XCTAssertEqual(model.drawn.first, "m\(601 - ChatModel.drawnRows)", "nothing is taken in yet")

        // Held rows taken in above, then rows past the cap let go below.
        var before = try onScreen()
        model.reachedTop()
        await spin()
        try assertStill(before, "taking in rows above")
        XCTAssertEqual(model.drawn.count, ChatModel.drawnRows, "rows past the cap left at the bottom")
        XCTAssertEqual(model.drawn.first, "m\(601 - ChatModel.drawnRows - ChatModel.drawnStep)")

        // Rows arriving below a reader in history are held, not drawn.
        let drawn = model.drawn
        before = try onScreen()
        source.append(Self.messages(601...640))
        model.woke()
        await spin()
        try assertStill(before, "rows arriving")
        XCTAssertEqual(model.drawn, drawn)
        XCTAssertTrue(model.newActivity)

        // Held rows taken in below, then rows past the cap let go above.
        before = try onScreen()
        model.reachedBottom()
        await spin()
        try assertStill(before, "taking in rows below")
        XCTAssertEqual(model.drawn.count, ChatModel.drawnRows, "rows past the cap left at the top")
        XCTAssertEqual(model.drawn.last, "m600")
    }

    func testAPageLandingAboveAReaderAtTheTopMovesNothingOnScreen() async throws {
        let source = ScriptedChat(rows: Self.messages(961...1_000), frame: ScriptedChat.frame(hasOlder: true))
        source.paged = .arrived(0)
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        await scrollUp(list, model, screens: 40)
        XCTAssertTrue(model.drawsOldestHeld)

        // The anchor moves the list inside SwiftUI's layout pass, and until
        // something else changes SwiftUI goes on reporting the rows where they
        // were before that move: at the top, where the loading notice coming
        // was taken back, the reported frames lag the screen. So the screen
        // itself is compared, over the rows between the header and the
        // composer.
        let before = try onScreen()
        let photographed = try rowsBand()
        source.prepend(Self.messages(921...960))
        model.woke()
        await spin()
        let after = try onScreen()
        XCTAssertGreaterThan(
            before.keys.filter { after[$0] != nil }.count, 3, "a page landing: the rows on screen went")
        let now = try rowsBand()
        try photographed.write(to: URL(fileURLWithPath: "/tmp/band-before.raw"))
        try now.write(to: URL(fileURLWithPath: "/tmp/band-after.raw"))
        XCTAssertEqual(now, photographed, "a page landing moved the rows on screen")
        XCTAssertEqual(model.drawn.first, "m921", "the page the reader waited at the top for is drawn")
        XCTAssertEqual(model.ids.count, 80)
    }

    func testAFollowingListKeepsItsNewestRowAtTheBottomAsOldRowsLeave() async throws {
        let source = ScriptedChat(rows: Self.messages(1...300), frame: ScriptedChat.frame())
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        for n in 301...340 {
            source.append(Self.messages(n...n))
            model.woke()
            await spin(0.05)
        }
        await spin()
        XCTAssertEqual(model.drawn.count, ChatModel.drawnRows)
        XCTAssertTrue(model.following)
        let newest = try XCTUnwrap(probe.elements.last)
        XCTAssertTrue(newest.label?.hasPrefix("message 340:") == true)
        let bottom = list.contentOffset.y + list.bounds.height - list.adjustedContentInset.bottom
        XCTAssertEqual(bottom, list.contentSize.height, accuracy: 1, "the list is at its newest row")
    }
}
