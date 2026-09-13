import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What an ask panel offers, and when.
///
/// The drawing is proved by the captures; what these pin is the reasoning a
/// panel does before it offers anything — whether one tap is already the whole
/// answer, when Send may be pressed, and which of the several things that can
/// occupy the composer's place wins when more than one is true at once.
@MainActor
final class AskPanelBehaviourTests: XCTestCase {
    private func question(
        _ id: Int, multi: Bool, options: [String] = ["a", "b", "c"]
    ) -> AskPanel.Question {
        AskPanel.Question(
            id: id, header: "H", prompt: "Which?", multiSelect: multi,
            options: options.enumerated().map {
                AskPanel.Question.Option(id: $0.offset, label: $0.element, description: nil)
            })
    }

    /// One question that takes one answer needs no Send: the tap is the
    /// answer, and a confirmation step after it says nothing.
    func testOneSingleSelectQuestionIsAnsweredByTappingIt() {
        XCTAssertTrue(QuestionDraft.immediate([question(0, multi: false)]))
        XCTAssertFalse(QuestionDraft.immediate([question(0, multi: true)]))
        XCTAssertFalse(
            QuestionDraft.immediate([question(0, multi: false), question(1, multi: false)]))
    }

    /// A multi-select question accumulates and un-accumulates, and Send waits
    /// until something has been chosen — the layer refuses an empty answer.
    func testAMultiSelectQuestionCollectsBeforeItSends() {
        let questions = [question(0, multi: true)]
        var draft = QuestionDraft()
        XCTAssertFalse(draft.isComplete(for: questions))
        draft.toggle(0, of: questions[0])
        draft.toggle(2, of: questions[0])
        XCTAssertTrue(draft.picked(0, of: 0))
        XCTAssertTrue(draft.picked(2, of: 0))
        XCTAssertTrue(draft.isComplete(for: questions))
        XCTAssertEqual(draft.replies(for: questions).map(\.selected), [[0, 2]])
        draft.toggle(0, of: questions[0])
        XCTAssertEqual(draft.replies(for: questions).map(\.selected), [[2]])
    }

    /// Several questions all have to be answered before any of them is sent,
    /// because the layer refuses a response with one missing.
    func testEveryQuestionMustBeAnsweredBeforeAnythingIsSent() {
        let questions = [question(0, multi: false), question(1, multi: true)]
        var draft = QuestionDraft()
        draft.toggle(1, of: questions[0])
        XCTAssertFalse(draft.isComplete(for: questions))
        draft.toggle(0, of: questions[1])
        XCTAssertTrue(draft.isComplete(for: questions))
        XCTAssertEqual(draft.replies(for: questions).map(\.selected), [[1], [0]])
    }

    /// A single-select question replaces rather than accumulates: choosing a
    /// second option is a change of mind, not a second answer.
    func testASingleSelectQuestionKeepsOnlyTheLastChoice() {
        let questions = [question(0, multi: false), question(1, multi: false)]
        var draft = QuestionDraft()
        draft.toggle(0, of: questions[0])
        draft.toggle(2, of: questions[0])
        XCTAssertEqual(draft.replies(for: questions).map(\.selected), [[2], []])
    }
}
