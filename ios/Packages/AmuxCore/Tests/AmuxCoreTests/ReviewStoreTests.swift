import Foundation
import XCTest
@testable import AmuxCore

/// A review being written: what a range resolves to, what accumulates, and
/// what an unfinished remark leaves behind.
@MainActor
final class ReviewStoreTests: XCTestCase {
    private func store() -> ReviewStore {
        ReviewStore(diff: ArtifactId("sha256:abc"), document: Self.document)
    }

    /// Every address into a review is an index into the page's own order, and
    /// that order is alphabetical rather than the order the patch listed.
    func testFilesReadInAlphabeticalOrderRatherThanThePatchsOrder() {
        XCTAssertEqual(
            Self.document.files.map(\.path), ["src/pairing.rs", "PROTOCOL.md"])
        XCTAssertEqual(store().files.map(\.path), ["PROTOCOL.md", "src/pairing.rs"])
    }

    /// A removed row is only in the old file and an added row only in the new,
    /// so a range across both is addressed on the side each end lives on.
    func testARangeIsAddressedOnTheSideEachEndLivesOn() throws {
        let anchor = try XCTUnwrap(store().anchor(LineRange(file: 1, from: 1, to: 3)))
        XCTAssertEqual(anchor.path, "src/pairing.rs")
        XCTAssertEqual(anchor.startSide, .old)
        XCTAssertEqual(anchor.startLine, 119)
        XCTAssertEqual(anchor.side, .new)
        XCTAssertEqual(anchor.line, 119)
        XCTAssertEqual(anchor.quoted.count, 3)
    }

    /// The break between two hunks belongs to neither numbering, so a range
    /// that ends on one is not taken: a sheet opened on it could not be sent.
    func testARangeEndingOnAHunkBreakIsNotTaken() {
        let review = store()
        review.select(LineRange(file: 1, from: 3, to: 4))
        XCTAssertNil(review.selection)
        XCTAssertNil(review.anchor(LineRange(file: 1, from: 3, to: 4)))
    }

    func testCommentsAccumulateInTheOrderTheDocumentReads() {
        let review = store()
        review.comment(LineRange(file: 1, from: 3, to: 3), "said second")
        review.comment(LineRange(file: 0, from: 1, to: 1), "said first")
        XCTAssertEqual(review.comments.map(\.text), ["said first", "said second"])
        XCTAssertEqual(review.comments(in: "src/pairing.rs"), 1)
        XCTAssertEqual(review.comments(in: "PROTOCOL.md"), 1)
    }

    /// An unfinished remark is unfinished about a particular range. Keeping
    /// the words while losing the lines would put them somewhere nobody chose.
    func testCancellingAnUnfinishedCommentKeepsNothing() {
        let review = store()
        review.select(LineRange(file: 1, from: 1, to: 2))
        review.draft = "half a thought"
        review.cancel()
        XCTAssertNil(review.selection)
        XCTAssertEqual(review.draft, "")
        XCTAssertTrue(review.comments.isEmpty)
    }

    func testARemarkWithNoWordsInItIsNotAComment() {
        let review = store()
        XCTAssertFalse(review.comment(LineRange(file: 0, from: 1, to: 1), "   \n "))
        XCTAssertTrue(review.comments.isEmpty)
    }

    func testACommentIsDrawnUnderTheRowItEndsOn() throws {
        let review = store()
        review.comment(LineRange(file: 1, from: 1, to: 3), "about the whole run")
        let comment = try XCTUnwrap(review.comments.first)
        XCTAssertEqual(review.row(of: comment), RowRef(file: 1, row: 3))
        XCTAssertEqual(review.comments(under: RowRef(file: 1, row: 3)).count, 1)
        XCTAssertTrue(review.comments(under: RowRef(file: 1, row: 1)).isEmpty)
    }

    func testARangeNamesItselfByRowsOfThePatch() {
        XCTAssertEqual(
            store().describe(LineRange(file: 1, from: 1, to: 3)),
            "3 lines in src/pairing.rs")
        XCTAssertEqual(
            store().describe(LineRange(file: 0, from: 1, to: 1)),
            "1 line in PROTOCOL.md")
    }

    func testFoldingAFileIsRememberedByPath() {
        let review = store()
        XCTAssertFalse(review.isCollapsed("PROTOCOL.md"))
        review.toggle(file: "PROTOCOL.md")
        XCTAssertTrue(review.isCollapsed("PROTOCOL.md"))
        review.toggle(file: "PROTOCOL.md")
        XCTAssertFalse(review.isCollapsed("PROTOCOL.md"))
    }

    private static let document = ReviewDocument(
        files: [
            ReviewFile(
                path: "src/pairing.rs", added: 1, removed: 2,
                rows: [
                    DiffRow(old: 118, new: 118, kind: .context, text: "  let message = {"),
                    DiffRow(old: 119, new: nil, kind: .removed, text: "-   a => \"one\","),
                    DiffRow(old: 120, new: nil, kind: .removed, text: "-   b => \"two\","),
                    DiffRow(old: nil, new: 119, kind: .added, text: "+   _ => \"the same\","),
                    DiffRow(old: nil, new: nil, kind: .boundary, text: ""),
                    DiffRow(old: 200, new: 199, kind: .context, text: "  }"),
                ],
                hunkStarts: [0, 5]),
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
