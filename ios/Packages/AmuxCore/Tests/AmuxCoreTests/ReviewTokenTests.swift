import Foundation
import XCTest
@testable import AmuxCore

/// What leaving the diff page with a written review actually sends.
///
/// Every assertion here goes through the shared library twice: the element is
/// formatted by it and then read back by the same parser every other client
/// reads messages with. A review the phone could write but nothing could read
/// would pass a test that only looked at the string.
@MainActor
final class ReviewTokenTests: XCTestCase {
    private func written() -> ReviewStore {
        let review = ReviewStore(diff: Self.diff, document: Self.document)
        review.comment(LineRange(file: 1, from: 1, to: 3), "This collapses two cases into one.")
        review.comment(LineRange(file: 0, from: 1, to: 1), "Say which message.")
        return review
    }

    /// The token carries the whole remark set and says how much was written.
    func testTheTokenCountsWhatWasWritten() throws {
        let token = try XCTUnwrap(written().token)
        XCTAssertEqual(token.comments, 2)
        XCTAssertEqual(token.label, "Review \u{00B7} 2 comments")
        XCTAssertEqual(token.attachment.id, Self.diff)
        XCTAssertEqual(token.attachment.kind, .diff)
        XCTAssertEqual(token.attachment.name, "review")
        XCTAssertEqual(token.attachment.mime, "text/x-diff")
        // No bytes: the patch is already stored where it was produced and this
        // rides the send only to keep it fetchable for whoever reads it.
        XCTAssertEqual(token.attachment.size, 0)
    }

    /// The general remark is prose in the same message, not a field of the
    /// review, and the review is a token beside it.
    func testTheRemarkIsProseBesideTheToken() throws {
        var draft = MessageDraft()
        draft.attach(try XCTUnwrap(written().token))
        draft.prose = "Two small things, otherwise good."

        let segments = draft.segments
        XCTAssertEqual(segments.count, 2)
        guard case .mention(let mention) = segments.first else {
            return XCTFail("the review is not the first thing in the message")
        }
        XCTAssertEqual(mention.name, "review")
        XCTAssertEqual(segments.last, .prose("\n\nTwo small things, otherwise good."))
    }

    /// A review says nothing about whether the work is good. There is no
    /// verdict to record, so nothing in what is sent carries one.
    func testAReviewCarriesNoVerdict() throws {
        let element = try XCTUnwrap(written().token).element
        for verdict in ["approve", "reject", "request-changes", "verdict"] {
            XCTAssertFalse(element.contains(verdict), "the element claims a \(verdict)")
        }
    }

    /// What was reviewed, exactly: the artifact, the base it was taken
    /// against, the commit it was taken at, and the blob of every changed path
    /// so the identity survives the tree moving on.
    func testTheSentElementNamesWhatWasReviewed() throws {
        var draft = MessageDraft()
        draft.attach(try XCTUnwrap(written().token))

        let mention = try XCTUnwrap(draft.segments.first?.mention)
        guard case .review(let header, _) = mention.kind else {
            return XCTFail("the attachment came back as something other than a review")
        }
        XCTAssertEqual(header.diff, Self.diff)
        XCTAssertEqual(header.base, "branch:main")
        XCTAssertEqual(header.head, "9f2c1b40")
        XCTAssertNil(header.mergeBase)
        XCTAssertEqual(
            header.blobs,
            [["src/pairing.rs", "b71c3f0a"], ["PROTOCOL.md", "8f13c7a2"]])
    }

    /// Each remark comes back at the range it was written about, on the side
    /// each end of that range lives on, with the rows it quoted.
    func testEachRemarkComesBackAtItsOwnRange() throws {
        var draft = MessageDraft()
        draft.attach(try XCTUnwrap(written().token))

        let mention = try XCTUnwrap(draft.segments.first?.mention)
        guard case .review(_, let comments) = mention.kind else {
            return XCTFail("the attachment came back as something other than a review")
        }
        // Ordered the way the frozen patch lists its files, which is the
        // shared model's order and not the alphabetical one the page reads in.
        XCTAssertEqual(comments.map(\.path), ["src/pairing.rs", "PROTOCOL.md"])

        let markdown = try XCTUnwrap(comments.first { $0.path == "PROTOCOL.md" })
        XCTAssertEqual(markdown.text, "Say which message.")
        XCTAssertEqual(markdown.startSide, .new)
        XCTAssertEqual(markdown.startLine, 9)
        XCTAssertEqual(markdown.side, .new)
        XCTAssertEqual(markdown.line, 9)
        XCTAssertEqual(markdown.quoted, ["+ One message, whatever broke."])

        // A range whose ends are a removed row and an added one is addressed
        // in the old numbering at one end and the new at the other.
        let source = try XCTUnwrap(comments.first { $0.path == "src/pairing.rs" })
        XCTAssertEqual(source.text, "This collapses two cases into one.")
        XCTAssertEqual(source.startSide, .old)
        XCTAssertEqual(source.startLine, 119)
        XCTAssertEqual(source.side, .new)
        XCTAssertEqual(source.line, 119)
        XCTAssertEqual(source.quoted.count, 3)
    }

    /// A message with no review in it is only what was typed, and text that
    /// is not an attachment stays prose byte for byte.
    func testAMessageWithoutAReviewIsOnlyProse() {
        var draft = MessageDraft()
        draft.prose = "no review here <amux-attachment kind=\"review\">"
        XCTAssertEqual(draft.text, draft.prose)
        XCTAssertEqual(draft.segments, [.prose(draft.prose)])
        XCTAssertTrue(draft.attachments.isEmpty)
    }

    /// The send carries the message as one piece of text and the patch as an
    /// artifact, because a reader has to be able to fetch what was reviewed.
    func testTheSendCarriesTheMessageAndThePatch() throws {
        var draft = MessageDraft()
        draft.attach(try XCTUnwrap(written().token))
        draft.prose = "Two small things."

        let agent = AgentId(UUID(uuidString: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0")!)
        guard case .shared(let body) = try XCTUnwrap(draft.command(to: agent)) else {
            return XCTFail("a draft is sent with the shared vocabulary")
        }
        XCTAssertEqual(body["command"]?.stringValue, "send")
        XCTAssertEqual(body["agent"]?.stringValue, agent.description)
        let segments = try XCTUnwrap(body["draft"]?["segments"]?.arrayValue)
        XCTAssertEqual(segments.count, 1)
        XCTAssertEqual(segments.first?["segment"]?.stringValue, "text")
        XCTAssertEqual(segments.first?["text"]?.stringValue, draft.text)
        let attachments = try XCTUnwrap(body["draft"]?["attachments"]?.arrayValue)
        XCTAssertEqual(attachments.count, 1)
        XCTAssertEqual(attachments.first?["kind"]?.stringValue, "diff")
        XCTAssertEqual(attachments.first?["id"]?.stringValue, Self.diff.description)
    }

    private static let diff = ArtifactId(
        "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")

    private static let document = ReviewDocument(
        files: [
            ReviewFile(
                path: "src/pairing.rs", added: 1, removed: 2,
                rows: [
                    DiffRow(old: 118, new: 118, kind: .context, text: "  let message = {"),
                    DiffRow(old: 119, new: nil, kind: .removed, text: "-   a => \"one\","),
                    DiffRow(old: 120, new: nil, kind: .removed, text: "-   b => \"two\","),
                    DiffRow(old: nil, new: 119, kind: .added, text: "+   _ => \"the same\","),
                ],
                hunkStarts: [0]),
            ReviewFile(
                path: "PROTOCOL.md", added: 1, removed: 0,
                rows: [
                    DiffRow(old: 8, new: 8, kind: .context, text: "  ## Failures"),
                    DiffRow(old: nil, new: 9, kind: .added, text: "+ One message, whatever broke."),
                ],
                hunkStarts: [0]),
        ],
        identity: BaseIdentity(
            base: .branch("main"), head: "9f2c1b40", mergeBase: nil,
            blobs: [["src/pairing.rs", "b71c3f0a"], ["PROTOCOL.md", "8f13c7a2"]]))
}
