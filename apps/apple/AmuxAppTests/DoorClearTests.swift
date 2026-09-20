import AmuxCore
import UIKit
import XCTest
@testable import Amux

@MainActor
final class DoorClearTests: XCTestCase {
    func testClearFromBeginningMiddleAndEndKeepsDraftAndViewInAgreement() async throws {
        let door = DoorHost.shared
        var previous: UITextView?
        for fixture in ["typing", "tokens"] {
            for position in [0, 8, Int.max] {
                let opened = await door.handle(.open(screen: "typing", fixture: fixture))
                XCTAssertEqual(opened, .ack)
                try await waitForLayout(door) {
                    guard let window = DoorWindow.current else { return false }
                    let inputs = DoorWindow.allTextViews(in: window).compactMap { $0 as? UITextView }
                    return inputs.count == 1 && inputs[0] !== previous && inputs[0].hasText
                }
                let window = try XCTUnwrap(DoorWindow.current)
                let inputs = DoorWindow.allTextViews(in: window).compactMap { $0 as? UITextView }
                XCTAssertEqual(inputs.count, 1)
                let view = try XCTUnwrap(inputs.first)
                previous = view
                let conversation = door.stores.conversation(Scenario.focus)
                XCTAssertFalse(conversation.draft.body.isEmpty)
                if fixture == "tokens" { XCTAssertFalse(conversation.draft.tokens.isEmpty) }
                XCTAssertTrue(view.becomeFirstResponder())
                let offset = min(position, view.text.utf16.count)
                let caret = try XCTUnwrap(view.position(from: view.beginningOfDocument, offset: offset))
                view.selectedTextRange = view.textRange(from: caret, to: caret)
                XCTAssertEqual(view.selectedRange, NSRange(location: offset, length: 0))

                let cleared = await door.handle(.clear(identifier: "composer.field"))
                XCTAssertEqual(cleared, .ack, "\(fixture), caret \(offset)")
                try await waitForLayout(door) {
                    view.text.isEmpty && conversation.draft.body.isEmpty
                        && conversation.draft.tokens.isEmpty && conversation.draft.caret == 0
                }
                XCTAssertEqual(view.text, "")
                XCTAssertEqual(conversation.draft.body, "")
                XCTAssertTrue(conversation.draft.tokens.isEmpty)
                XCTAssertEqual(conversation.draft.caret, 0)

                let sentence = "Check the café reconnect path 👩🏽‍💻."
                let typed = await door.handle(.type(identifier: "composer.field", text: sentence))
                XCTAssertEqual(typed, .ack)
                try await waitForLayout(door) {
                    view.text == sentence && conversation.draft.body == sentence
                        && conversation.draft.caret == sentence.count
                }
                XCTAssertEqual(view.text, sentence)
                XCTAssertEqual(conversation.draft.body, sentence)
                XCTAssertEqual(conversation.draft.caret, sentence.count)
                let clearedAgain = await door.handle(.clear(identifier: "composer.field"))
                XCTAssertEqual(clearedAgain, .ack)
                try await waitForLayout(door) {
                    view.text.isEmpty && conversation.draft.body.isEmpty
                }
                XCTAssertEqual(view.text, "")
                XCTAssertEqual(conversation.draft.body, "")
                let alreadyEmpty = await door.handle(.clear(identifier: "composer.field"))
                XCTAssertEqual(alreadyEmpty, .ack)
            }
        }
    }

    /// Editing needs a mounted input and an updated layout, not stable glass
    /// pixels. Always yield before checking: the text delegate can update the
    /// model synchronously while SwiftUI is still replacing its element map.
    private func waitForLayout(
        _ door: DoorHost, ready: () -> Bool,
        file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let deadline = ContinuousClock.now + .seconds(5)
        repeat {
            try await Task.sleep(for: .milliseconds(10))
            guard let window = DoorWindow.current else { continue }
            window.layoutIfNeeded()
            guard ready(), case .state(let state) = await door.handle(.query),
                let field = state.elements.first(where: { $0.identifier == "composer.field" }),
                field.frame.width > 0, field.frame.height > 0,
                window.hitTest(CGPoint(
                    x: field.frame.x + field.frame.width / 2,
                    y: field.frame.y + field.frame.height / 2), with: nil) != nil
            else { continue }
            return
        } while ContinuousClock.now < deadline
        XCTFail("Timed out waiting for composer text and layout", file: file, line: line)
        throw LayoutTimeout.notReady
    }

    private enum LayoutTimeout: Error { case notReady }
}
