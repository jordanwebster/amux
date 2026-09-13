import AmuxCore
import UIKit
import XCTest
@testable import Amux

@MainActor
final class DoorClearTests: XCTestCase {
    func testClearFromBeginningMiddleAndEndKeepsDraftAndViewInAgreement() async throws {
        let door = DoorHost.shared
        for fixture in ["typing", "tokens"] {
            for position in [0, 8, Int.max] {
                let opened = await door.handle(.open(screen: "typing", fixture: fixture))
                XCTAssertEqual(opened, .ack)
                _ = await door.handle(.settle)
                let window = try XCTUnwrap(DoorWindow.current)
                let inputs = DoorWindow.allTextViews(in: window).compactMap { $0 as? UITextView }
                XCTAssertEqual(inputs.count, 1)
                let view = try XCTUnwrap(inputs.first)
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
                _ = await door.handle(.settle)
                XCTAssertEqual(view.text, "")
                XCTAssertEqual(conversation.draft.body, "")
                XCTAssertTrue(conversation.draft.tokens.isEmpty)
                XCTAssertEqual(conversation.draft.caret, 0)

                let sentence = "Check the café reconnect path 👩🏽‍💻."
                let typed = await door.handle(.type(identifier: "composer.field", text: sentence))
                XCTAssertEqual(typed, .ack)
                _ = await door.handle(.settle)
                XCTAssertEqual(view.text, sentence)
                XCTAssertEqual(conversation.draft.body, sentence)
                XCTAssertEqual(conversation.draft.caret, sentence.count)
                let clearedAgain = await door.handle(.clear(identifier: "composer.field"))
                XCTAssertEqual(clearedAgain, .ack)
                _ = await door.handle(.settle)
                XCTAssertEqual(view.text, "")
                XCTAssertEqual(conversation.draft.body, "")
                let alreadyEmpty = await door.handle(.clear(identifier: "composer.field"))
                XCTAssertEqual(alreadyEmpty, .ack)
            }
        }
    }
}
