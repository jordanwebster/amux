import XCTest

@testable import AmuxCore

final class DictationTests: XCTestCase {
    func testPartialResultsStreamIntoTheDraftAndCorrectionsReplaceEarlierWords() {
        var draft = "Before"
        var state = DictationState()
        state.prepare(speech: .allowed, microphone: .allowed, available: true)
        state.began(draft: draft)
        XCTAssertEqual(state.phase, .listening)
        state.receive("read", draft: &draft)
        XCTAssertEqual(draft, "Before read")
        state.receive("red the parser", draft: &draft)
        XCTAssertEqual(draft, "Before red the parser")
        state.receive("read the parser", draft: &draft)
        XCTAssertEqual(draft, "Before read the parser", "a correction replaces, never appends")
        let written = draft
        state.stop()
        state.receive("late result", draft: &draft)
        XCTAssertEqual(state.phase, .idle)
        XCTAssertEqual(draft, written)
    }

    func testAManualEditEndsTheSessionBeforeItIsOverwritten() {
        var draft = ""
        var state = DictationState()
        state.prepare(speech: .allowed, microphone: .allowed, available: true)
        state.began(draft: draft)
        state.receive("Hello", draft: &draft)
        XCTAssertEqual(draft, "Hello")
        draft += " world"
        state.receive("Hello there", draft: &draft)
        XCTAssertEqual(draft, "Hello world")
        XCTAssertEqual(state.phase, .idle)
    }

    func testEveryPermissionOutcomeHasASentenceAndNeverChangesTheDraft() {
        let permissions: [DictationState.Permission] = [.notAsked, .allowed, .denied]
        for speech in permissions {
            for microphone in permissions {
                for available in [true, false] {
                    var state = DictationState()
                    var draft = "Keep this"
                    state.prepare(speech: speech, microphone: microphone, available: available)
                    let expected: DictationState.Phase =
                        if speech == .denied || microphone == .denied { .denied }
                        else if !available { .unavailable }
                        else if speech == .notAsked || microphone == .notAsked { .requestingPermission }
                        else { .starting }
                    XCTAssertEqual(state.phase, expected)
                    XCTAssertNotNil(state.sentence)
                    state.receive("must not land", draft: &draft)
                    XCTAssertEqual(draft, "Keep this")
                    if expected == .denied { XCTAssertTrue(state.sentence!.contains("Settings")) }
                }
            }
        }
    }

    func testAFailedRecognitionKeepsWhatWasHeardAndSaysDictationIsUnavailable() {
        var draft = "Keep this"
        var state = DictationState()
        state.prepare(speech: .notAsked, microphone: .notAsked, available: true)
        state.stop()
        state.began(draft: draft)
        XCTAssertEqual(state.phase, .idle, "a stopped preparation never starts listening")
        state.prepare(speech: .allowed, microphone: .allowed, available: true)
        state.began(draft: draft)
        state.receive("please", draft: &draft)
        state.failed()
        XCTAssertEqual(state.phase, .unavailable)
        XCTAssertEqual(draft, "Keep this please")
    }
}
