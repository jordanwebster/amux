import XCTest
@testable import AmuxCore

final class DictationTests: XCTestCase {
    func testPartialResultsReplaceOnlyDictatedWordsAtTheCaret() {
        var draft = MessageDraft(prose: "Before  after")
        draft.place(caret: 7)
        draft.paste((1...14).map { "line \($0)" }.joined(separator: "\n"))
        let tokens = draft.tokens
        var state = DictationState()
        state.prepare(speech: .allowed, microphone: .allowed, available: true)
        state.began(draft: draft)
        XCTAssertEqual(state.phase, .listening)
        state.receive("read", draft: &draft)
        XCTAssertTrue(draft.body.hasSuffix("read after"))
        state.receive("read the parser", draft: &draft)
        XCTAssertTrue(draft.body.hasSuffix("read the parser after"))
        XCTAssertEqual(draft.caret, 8 + "read the parser".count)
        XCTAssertEqual(draft.tokens, tokens)
        let written = draft
        state.stop()
        state.receive("late result", draft: &draft)
        XCTAssertEqual(state.phase, .idle)
        XCTAssertEqual(draft, written)
    }

    func testTypingOrMovingTheCaretStopsBeforeOverwritingAnEdit() {
        for moveOnly in [false, true] {
            var draft = MessageDraft(prose: "A message")
            var state = DictationState()
            state.prepare(speech: .allowed, microphone: .allowed, available: true)
            state.began(draft: draft)
            state.receive("Hello ", draft: &draft)
            if moveOnly { draft.place(caret: 1) } else { draft.clear() }
            let edited = draft
            state.receive("Hello there ", draft: &draft)
            XCTAssertEqual(draft, edited)
            XCTAssertEqual(state.phase, .idle)
        }
    }

    func testEveryPermissionOutcomeHasAStateAndNeverChangesTheDraft() {
        let permissions: [DictationState.Permission] = [.notAsked, .allowed, .denied]
        for speech in permissions {
            for microphone in permissions {
                for available in [true, false] {
                    var state = DictationState()
                    var draft = MessageDraft(prose: "Keep this")
                    let before = draft
                    state.prepare(speech: speech, microphone: microphone, available: available)
                    let expected: DictationState.Phase
                    if speech == .denied || microphone == .denied { expected = .denied }
                    else if !available { expected = .unavailable }
                    else if speech == .notAsked || microphone == .notAsked { expected = .requestingPermission }
                    else { expected = .starting }
                    XCTAssertEqual(state.phase, expected)
                    XCTAssertNotNil(state.sentence)
                    state.receive("must not land", draft: &draft)
                    XCTAssertEqual(draft, before)
                    if expected == .denied { XCTAssertTrue(state.sentence!.contains("Settings")) }
                }
            }
        }
    }

    func testStoppingPermissionPreparationAndRecognitionFailureKeepTheDraft() {
        var draft = MessageDraft(prose: "Keep this")
        var state = DictationState()
        state.prepare(speech: .notAsked, microphone: .notAsked, available: true)
        state.stop()
        state.began(draft: draft)
        XCTAssertEqual(state.phase, .idle)
        state.prepare(speech: .allowed, microphone: .allowed, available: true)
        state.began(draft: draft)
        state.receive("Please ", draft: &draft)
        state.failed()
        XCTAssertEqual(state.phase, .unavailable)
        XCTAssertEqual(draft.body, "Please Keep this")
    }
}
