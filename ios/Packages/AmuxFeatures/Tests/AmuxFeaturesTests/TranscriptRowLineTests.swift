import AmuxDesign
import UIKit
import XCTest
@testable import AmuxFeatures

/// How a Ran or Wrote row divides its one line between the thing it acted on
/// and what the layer said about that.
///
/// The row exists to name a path or a command, and a provider is free to
/// answer a write with a whole sentence — Claude answers with "File created
/// successfully at: …". These measure the real strings in the real faces at
/// the width the transcript actually has on a 393-point display, so a change
/// that quietly hands the line back to the sentence fails here rather than in
/// a screenshot nobody reads closely.
final class TranscriptRowLineTests: XCTestCase {
    private let design = Design.app

    /// The width a rail row's content is drawn in: the display, less the
    /// feed's gutter on each side, less the rail's glyph column and the space
    /// between that column and the row.
    private let line: CGFloat = 393 - 2 * 18 - (26 + 8)

    /// The width the two texts contest, once the verb and the clear space
    /// between subject and meta are out of it.
    private func content(verb: String) -> CGFloat {
        line - width(verb, .body) - 8 - 20
    }

    /// A string's untruncated width in the face and size the row sets it in.
    private func width(_ text: String, _ role: Design.Role) -> CGFloat {
        let spec = design.spec(role)
        XCTAssertTrue(BundledFonts.isAvailable(spec.family), "\(spec.family) is not registered")
        let font = UIFont(
            descriptor: UIFontDescriptor(fontAttributes: [.family: spec.family]),
            size: spec.size)
        return (text as NSString)
            .size(withAttributes: [.font: font, .kern: spec.tracking]).width
    }

    /// The row a person reported: a file written, answered with the sentence
    /// the tool prints. The path fits the line with room to spare once the
    /// sentence stops taking the width first, so it is drawn whole and the
    /// sentence is the one that truncates.
    func testAWrittenPathIsDrawnWholeBesideALongSentence() {
        let path = width("/work/notes.md", .mono)
        let sentence = width("File created successfully at: /work/notes.md", .mono)
        let content = content(verb: "Wrote")
        XCTAssertGreaterThan(path + sentence, content, "the line was never contested")

        let share = RowLine.split(content: content, subject: path, meta: sentence)

        XCTAssertEqual(share.subject, path, accuracy: 0.5)
        XCTAssertLessThan(share.meta, sentence)
    }

    /// A path long enough to contest the line on its own keeps two thirds of
    /// it, which is more than the file's own name needs — and a written path
    /// is truncated from its front, so that is what the width is spent on.
    func testALongPathKeepsTwoThirdsOfTheLine() {
        let path = width("crates/amux-ui/src/pairing_copy.rs", .mono)
        let sentence = width(
            "File created successfully at: crates/amux-ui/src/pairing_copy.rs", .mono)
        let content = content(verb: "Wrote")

        let share = RowLine.split(content: content, subject: path, meta: sentence)

        XCTAssertEqual(share.meta, content * RowLine.metaShare, accuracy: 0.5)
        XCTAssertEqual(share.subject, content - share.meta, accuracy: 0.5)
        XCTAssertGreaterThan(share.subject, width("pairing_copy.rs", .mono))
    }

    /// The other direction, which is why the subject cannot simply be given
    /// the priority the meta used to hold: a command that fills the line must
    /// not push off the two words that say how it ended.
    func testALongCommandDoesNotPushOffHowItEnded() {
        let command = width("cargo test --workspace --all-features -- --nocapture", .mono)
        let exit = width("exit 1", .mono)
        let content = content(verb: "Ran")
        XCTAssertGreaterThan(command + exit, content, "the line was never contested")

        let share = RowLine.split(content: content, subject: command, meta: exit)

        XCTAssertEqual(share.meta, exit, accuracy: 0.5)
        XCTAssertEqual(share.subject, content - exit, accuracy: 0.5)
    }

    /// A line nothing contests is left alone, so the rows that already fit
    /// are drawn exactly where they were.
    func testATextThatFitsIsNotTruncated() {
        let path = width("src/pairing_copy.rs", .mono)
        let lines = width("38 lines", .mono)
        let content = content(verb: "Wrote")
        XCTAssertLessThan(path + lines, content, "this pair no longer fits the line")

        let share = RowLine.split(content: content, subject: path, meta: lines)

        XCTAssertEqual(share.subject, path, accuracy: 0.5)
        XCTAssertEqual(share.meta, lines, accuracy: 0.5)
    }
}
