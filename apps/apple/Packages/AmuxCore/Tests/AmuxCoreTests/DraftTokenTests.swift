import Foundation
import XCTest
@testable import AmuxCore

/// What a message carrying attachments does when it is edited.
///
/// Three gestures decide whether inline tokens are usable at all: the picker
/// puts one where the caret is, one backspace takes the whole of it, and it
/// can be picked up and put down elsewhere in the sentence. Everything here
/// checks the model those gestures act on, and checks it through the shared
/// parser: a draft that edited cleanly but sent something no other client
/// could read would pass a test that only looked at the string.
@MainActor
final class DraftTokenTests: XCTestCase {
    private var photo: DraftToken {
        get throws {
            try XCTUnwrap(Bridge.token(for: DraftAttachment(
                id: ArtifactId("sha256:" + String(repeating: "ab", count: 32)),
                kind: .image, name: "shot.png", mime: "image/png", size: 1024)))
        }
    }

    private var file: DraftToken {
        get throws {
            try XCTUnwrap(Bridge.token(for: DraftAttachment(
                id: ArtifactId("sha256:" + String(repeating: "cd", count: 32)),
                kind: .file, name: "trace.json", mime: "application/json", size: 2048)))
        }
    }

    // MARK: - Insert

    /// The picker inserts where the caret already is rather than at the end.
    func testATokenLandsAtTheCaret() throws {
        var draft = MessageDraft()
        draft.insert(text: "look at  and tell me")
        draft.place(caret: 8)
        draft.insert(try photo)

        let segments = draft.segments
        XCTAssertEqual(segments.count, 3)
        XCTAssertEqual(segments.first, .prose("look at "))
        XCTAssertEqual(segments.last, .prose(" and tell me"))
        guard case .mention(let mention) = segments[1] else {
            return XCTFail("the photo is not between the two halves of the sentence")
        }
        XCTAssertEqual(mention.name, "shot.png")
        // And the caret is after what was just inserted, so typing continues
        // on the far side of it.
        XCTAssertEqual(draft.caret, 9)
    }

    /// Every kind lives in the same sentence, and each one carries whatever
    /// has to travel with it.
    func testFourKindsInOneMessage() throws {
        var draft = MessageDraft()
        draft.insert(try photo)
        draft.insert(try file)
        draft.paste(String(repeating: "a line of log\n", count: 12))

        XCTAssertEqual(draft.ordered.map(\.kind), [.photo, .file, .text])
        // The photo and the file are stored blobs the reader has to fetch; the
        // pasted text is carried inside its own element and needs nothing.
        XCTAssertEqual(draft.attachments.map(\.name), ["shot.png", "trace.json"])
        XCTAssertEqual(
            draft.segments.compactMap(\.mention).map(\.name),
            ["shot.png", "trace.json", "pasted text"])
    }

    /// A short paste is what was pasted, not a token. Where the line sits is
    /// the shared library's answer and not the phone's.
    func testAShortPasteStaysWords() {
        var draft = MessageDraft()
        draft.paste("two\nlines")
        XCTAssertTrue(draft.tokens.isEmpty)
        XCTAssertEqual(draft.body, "two\nlines")
    }

    /// A paste past about eight lines is named and counted rather than shown.
    func testALongPasteBecomesAToken() throws {
        var draft = MessageDraft()
        draft.paste((1...9).map { "line \($0)" }.joined(separator: "\n"))
        let token = try XCTUnwrap(draft.ordered.first)
        XCTAssertEqual(token.kind, .text)
        XCTAssertEqual(token.label, "Pasted text \u{00B7} 9 lines")
        // The words are still in the message; they are just not in the field.
        guard case .text(let body, let lines) =
            try XCTUnwrap(draft.segments.compactMap(\.mention).first).kind
        else { return XCTFail("the paste is not a text attachment") }
        XCTAssertEqual(lines, 9)
        XCTAssertTrue(body.hasSuffix("line 9"))
    }

    // MARK: - Remove

    /// One backspace takes the whole token, not the last letter of its name.
    func testOneBackspaceRemovesAWholeToken() throws {
        var draft = MessageDraft()
        draft.insert(text: "here: ")
        draft.insert(try photo)
        XCTAssertEqual(draft.ordered.count, 1)

        draft.backspace()

        XCTAssertTrue(draft.ordered.isEmpty)
        XCTAssertTrue(draft.attachments.isEmpty)
        XCTAssertEqual(draft.text, "here: ")
        XCTAssertEqual(draft.caret, 6)
    }

    /// And the backspace after that takes one letter, so a token is not a trap
    /// that swallows the word in front of it.
    func testTheNextBackspaceTakesOneLetter() throws {
        var draft = MessageDraft()
        draft.insert(text: "ok")
        draft.insert(try file)
        draft.backspace()
        draft.backspace()
        XCTAssertEqual(draft.text, "o")
    }

    /// Deleting a token takes what travelled with it too: a message that still
    /// pinned the blob of a photo nobody can see would keep bytes alive for a
    /// reader who will never be shown them.
    func testRemovingATokenDropsItsArtifact() throws {
        var draft = MessageDraft()
        draft.insert(try photo)
        draft.insert(try file)
        XCTAssertEqual(draft.attachments.count, 2)
        draft.backspace()
        XCTAssertEqual(draft.attachments.map(\.name), ["shot.png"])
    }

    // MARK: - Move

    /// A token is picked up and put down elsewhere in the sentence, whole.
    func testATokenMoves() throws {
        var draft = MessageDraft()
        draft.insert(try photo)
        draft.insert(text: "compare with this")

        // From the front of the sentence to the end of it.
        draft.move(from: 0, to: draft.body.count - 1)

        let segments = draft.segments
        XCTAssertEqual(segments.first, .prose("compare with this"))
        XCTAssertEqual(segments.last?.mention?.name, "shot.png")
        XCTAssertEqual(draft.ordered.map(\.kind), [.photo])
        XCTAssertEqual(draft.attachments.map(\.name), ["shot.png"])
    }

    /// Moving does not change what is sent apart from where the token sits,
    /// and the artifacts follow the order they now stand in.
    func testMovingReordersWhatTravels() throws {
        var draft = MessageDraft()
        draft.insert(try photo)
        draft.insert(try file)
        XCTAssertEqual(draft.attachments.map(\.name), ["shot.png", "trace.json"])

        draft.move(from: 1, to: 0)

        XCTAssertEqual(draft.attachments.map(\.name), ["trace.json", "shot.png"])
        XCTAssertEqual(
            draft.segments.compactMap(\.mention).map(\.name), ["trace.json", "shot.png"])
    }

    // MARK: - What is sent

    /// The stand-in characters never leave the phone: what is sent is the
    /// canonical elements, and nothing in it is from the private use area.
    func testWhatIsSentCarriesNoStandIns() throws {
        var draft = MessageDraft()
        draft.insert(text: "before ")
        draft.insert(try photo)
        draft.insert(text: " after")

        let private_ = Unicode.Scalar(0xE000)!...Unicode.Scalar(0xF8FF)!
        XCTAssertFalse(draft.text.unicodeScalars.contains { private_.contains($0) })
        XCTAssertTrue(draft.text.contains("<amux-attachment"))
        XCTAssertEqual(
            draft.segments.compactMap { if case .prose(let said) = $0 { said } else { nil } },
            ["before ", " after"])
    }

    /// Clearing the field takes the tokens with it. Throwing away a message is
    /// throwing away all of it, not the words with the attachments left over.
    func testClearingTakesTheTokens() throws {
        var draft = MessageDraft()
        draft.insert(text: "never mind")
        draft.insert(try photo)
        draft.clear()
        XCTAssertTrue(draft.isEmpty)
        XCTAssertTrue(draft.tokens.isEmpty)
        XCTAssertEqual(draft.caret, 0)
    }

    /// A draft holding only a token is not empty. There is no prose in it, but
    /// there is something to send.
    func testATokenAloneIsAMessage() throws {
        var draft = MessageDraft()
        draft.insert(try photo)
        XCTAssertFalse(draft.isEmpty)
    }
}
