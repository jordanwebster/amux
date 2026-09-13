import Foundation
import XCTest
@testable import AmuxCore

/// Agents write markdown, so structure the agent chose has to survive the trip
/// to the screen. Each case here is a construct that reads as something else
/// entirely when it is flattened.
final class MarkdownTests: XCTestCase {
    private func blocks(_ source: String) -> [MarkdownBlock] {
        MarkdownDocument.parse(source).blocks
    }

    func testHeadingsKeepTheirLevel() {
        guard case .heading(let level, let text) = blocks("## Why it broke")[0] else {
            return XCTFail("expected a heading")
        }
        XCTAssertEqual(level, 2)
        XCTAssertEqual(String(text.characters), "Why it broke")
    }

    func testFencedCodeKeepsItsLinesAndItsLanguage() {
        guard case .code(let language, let text) = blocks("""
            ```rust
            let x = 1;
                let y = 2;
            ```
            """)[0] else { return XCTFail("expected code") }
        XCTAssertEqual(language, "rust")
        XCTAssertEqual(text, "let x = 1;\n    let y = 2;")
    }

    func testAFenceWithNoLanguageSaysSoRatherThanGuessing() {
        guard case .code(let language, _) = blocks("```\nplain\n```")[0] else {
            return XCTFail("expected code")
        }
        XCTAssertNil(language)
    }

    func testListsKeepTheirMarkersAndTheirDepth() {
        guard case .list(let ordered, let items) = blocks("""
            - one
            - two
              - nested
            """)[0] else { return XCTFail("expected a list") }
        XCTAssertFalse(ordered)
        XCTAssertEqual(items.map(\.depth), [0, 0, 1])
        XCTAssertEqual(items.map { String($0.text.characters) }, ["one", "two", "nested"])
    }

    func testANumberedListIsNumberedRatherThanBulleted() {
        guard case .list(let ordered, let items) = blocks("1. first\n2. second")[0] else {
            return XCTFail("expected a list")
        }
        XCTAssertTrue(ordered)
        XCTAssertEqual(items.map(\.marker), ["1.", "2."])
    }

    /// A table's second row is its alignment rule, which is punctuation. Left
    /// in, it renders as a row of dashes pretending to be data.
    func testATablesAlignmentRuleIsNotARow() {
        guard case .table(let header, let rows) = blocks("""
            | Layer | Reads |
            | --- | ---: |
            | claude | yes |
            | codex | no |
            """)[0] else { return XCTFail("expected a table") }
        XCTAssertEqual(header.map { String($0.characters) }, ["Layer", "Reads"])
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual(rows[0].map { String($0.characters) }, ["claude", "yes"])
    }

    func testQuotesKeepTheirLinesAndDropTheirMarker() {
        guard case .quote(let lines) = blocks("> first\n> second")[0] else {
            return XCTFail("expected a quote")
        }
        XCTAssertEqual(lines.map { String($0.characters) }, ["first", "second"])
    }

    func testWrappedProseIsOneParagraph() {
        let parsed = blocks("one line\nand its continuation\n\na second paragraph")
        XCTAssertEqual(parsed.count, 2)
        guard case .paragraph(let first) = parsed[0] else { return XCTFail("expected prose") }
        XCTAssertEqual(String(first.characters), "one line and its continuation")
    }

    func testEmphasisAndCodeSpansResolveRatherThanShowingTheirPunctuation() {
        guard case .paragraph(let text) = blocks("run `cargo test` **now**")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "run cargo test now")
    }

    func testALinkKeepsItsTextAndItsDestination() {
        guard case .paragraph(let text) = blocks("see [the spec](https://example.com)")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "see the spec")
        XCTAssertEqual(
            text.runs.compactMap(\.link?.absoluteString), ["https://example.com"])
        // Underlined rather than coloured: the one accent means "waiting for
        // you", and the system's blue is not a colour this design owns.
        XCTAssertEqual(text.runs.compactMap(\.underlineStyle), [.single])
    }

    /// A stray asterisk must not blank the sentence around it.
    func testMalformedInlineMarkupFallsBackToTheLiteralText() {
        guard case .paragraph(let text) = blocks("a * b")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "a * b")
    }

    func testAHorizontalRuleIsARuleAndNotAList() {
        XCTAssertEqual(blocks("---").count, 1)
        guard case .rule = blocks("---")[0] else { return XCTFail("expected a rule") }
    }

    func testPlainProseIsRecognisedSoItCanBeDrawnCheaply() {
        XCTAssertTrue(MarkdownDocument.parse("just a sentence").isPlainParagraph)
        XCTAssertFalse(MarkdownDocument.parse("# heading\n\ntext").isPlainParagraph)
    }
}
