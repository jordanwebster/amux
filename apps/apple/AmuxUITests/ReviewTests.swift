import Foundation
import XCTest

/// A patch a real host computed, read and written about on a phone.
///
/// Everything on the page arrived over the relay: the journey leaves a
/// repository with an uncommitted change in it, the host is asked for the
/// changes, and the host freezes and sends the patch. What a finger does to it
/// is here — holding a line and dragging to take a range, saying something
/// about it, cancelling one, and attaching the whole review to the message
/// that goes back. What the host then received is the journey's to check.
final class ReviewTests: JourneyCase {
    /// What is said about each range. The first words of each are what the
    /// journey looks for in the element the host received.
    private static let remarks = [
        "These are the two lines that dropped the trailing newline.",
        "Worth a test of its own before this lands.",
        "Nothing below this has to change with it.",
    ]
    private static let cancelled = "Never mind, this one is fine."
    private static let prose = "Three remarks on the parser change — the last one matters most."

    func testAReviewIsWrittenAndSentWithItsPatch() throws {
        let runner = try Runner()
        let app = launch(runner)
        try pairThroughTheDoor(runner)

        // MARK: The agent, opened, and its changes asked for.
        waitFor(app, "home.row.\(runner.agent)",
                "the machine never answered for \(runner.agent)")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation", "opening the row did not lead to a conversation")
        try door(runner, .init(kind: "requestChanges", agent: runner.agent, base: ""))
        waitFor(app, "conversation.changes", "the host answered with no changes to review")
        press(app, "conversation.changes")

        // MARK: The patch, as one scroll.
        waitFor(app, "review", "the changes chip did not lead to the patch")
        let page = try declared(runner)
        record["diff"] = said(page, "review")?.value ?? ""
        record["magnitudes"] = said(page, "review.chrome")?.value ?? ""
        // What the page says it covers is the chrome's own count; which files
        // those are is gathered as the reader passes them, because a patch
        // taller than the phone never has all of its headings on screen at
        // once.
        record["onOpening"] = page.filter { $0.identifier == "review.file" }.map { $0.label }
        photograph(app, "review-diff")

        // MARK: The three ways around a patch longer than the screen.
        //
        // The list every heading opens, folding a file away, and the wheel
        // down the right edge. Each is judged by where the page is left: a
        // line that belongs to one file and to no other is on screen, and the
        // line the page was showing before is not.
        XCTAssertNotNil(onScreen(app, Self.firstOfParser),
                        "the patch did not open on the first file")
        let around = try navigate(app, runner)
        record["navigation"] = around
        record["files"] = around["files"]

        // MARK: Three remarks, each about a range taken hold of.
        //
        // Held and dragged rather than tapped: a range needs two ends, and a
        // plain drag on this page is how it scrolls. The rows are found by
        // what they say — a diff row is a line of a file and carries no name
        // of its own — and what the sheet says it is about is recorded, so the
        // journey can hold the host's copy against it.
        //
        // In that order because a range that is taken hold of is scrolled to
        // the top of the page: what was above it goes off screen, so the
        // remarks run down the patch rather than back up it.
        var written: [[String: String]] = []
        written.append(try remark(app, runner, Self.remarks[0],
                                  from: "Removed line ", offset: 0, span: 1))
        written.append(try remark(app, runner, Self.remarks[1], from: "Added line ",
                                  offset: 0, span: 2, photographing: "review-comment"))
        written.append(try remark(app, runner, Self.remarks[2],
                                  from: "Line ", offset: 0, span: 1))
        record["comments"] = written
        // Counted off the button that sends them rather than off a badge: the
        // page carries one badge per file as well as the total, and only the
        // button names the whole review.
        XCTAssertEqual(label(app, "review.attach"), "Attach Review \u{00B7} 3 comments",
                       "three remarks were written and the page offers to attach "
                       + "\(self.label(app, "review.attach") ?? "nothing")")

        // MARK: One taken back before it was said.
        let takenBack = try select(app, runner, from: "Line ", offset: 0, span: 1)
        let field = try XCTUnwrap(writable(app), "the sheet offered nowhere to write")
        field.tap()
        field.typeText(Self.cancelled)
        press(app, "review.cancelComment")
        waitForNo(app, "review.commentSheet", "cancelling left the sheet open")
        XCTAssertEqual(label(app, "review.attach"), "Attach Review \u{00B7} 3 comments",
                       "cancelling a remark added it anyway")
        record["cancelled"] = ["about": takenBack, "text": Self.cancelled]

        // MARK: Attached, and sent with what was said about the whole patch.
        //
        // The words beside the review go through the door: the composer is
        // later work, so there is nowhere on screen to write them yet. The
        // message itself is the draft's, built and addressed by the same code
        // the composer will use, and it goes through the same gate.
        press(app, "review.attach")
        waitFor(app, "conversation", "attaching the review did not come back to the conversation")
        try door(runner, .init(kind: "awaitSendable", agent: runner.agent, seconds: 90))
        let sent = try door(runner, .init(kind: "sendDraft", agent: runner.agent,
                                          prose: Self.prose))
        XCTAssertEqual(sent["delivered"] as? Bool, true,
                       "the review did not leave the phone: \(sent)")
        record["sent"] = sent
        photograph(app, "review-sent")

        try write("review.json")
    }

    // MARK: - Holding a range and saying something about it

    /// Holds a range of lines, says something about it and adds it.
    ///
    /// The answer is what the sheet said the range was — the file and the
    /// lines in that file's own numbering — beside the words, which is the
    /// pair the journey holds the host's copy against.
    private func remark(
        _ app: XCUIApplication, _ runner: Runner, _ text: String, from kind: String,
        offset: Int, span: Int, photographing: String? = nil
    ) throws -> [String: String] {
        var about = try select(app, runner, from: kind, offset: offset, span: span)
        let field = try XCTUnwrap(writable(app), "the sheet offered nowhere to write")
        field.tap()
        field.typeText(text)
        if let photographing { photograph(app, photographing) }
        press(app, "review.addComment")
        waitForNo(app, "review.commentSheet", "adding the remark left the sheet open")
        about["text"] = text
        return about
    }

    /// Takes hold of a range: a long press on one row dragged onto another.
    ///
    /// `offset` counts from the first row of that kind on screen and `span`
    /// says how many rows further down the range reaches, so a range is
    /// described by what is in front of a reader rather than by line numbers
    /// this test would have to know in advance.
    private func select(
        _ app: XCUIApplication, _ runner: Runner, from kind: String, offset: Int, span: Int
    ) throws -> [String: String] {
        let rows = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", kind))
            .allElementsBoundByIndex.filter { $0.isHittable }
        guard rows.count > offset + span else {
            throw Lines.Failure(
                "the patch shows \(rows.count) rows beginning \(kind), and this wanted "
                + "\(offset + span + 1)")
        }
        rows[offset].press(forDuration: 0.6, thenDragTo: rows[offset + span])
        waitFor(app, "review.commentSheet", "holding a range of lines opened no sheet")
        let sheet = said(try declared(runner), "review.commentSheet")
        // "6 lines in parser.rs": how much was taken hold of, and where. The
        // file is pulled out of it because that is what a comment is finally
        // addressed by, and the sentence is kept as the sheet said it.
        let says = sheet?.label ?? ""
        return [
            "says": says,
            "path": says.components(separatedBy: " in ").last ?? "",
            "lines": sheet?.value ?? "",
        ]
    }


    /// The file list, folding, and the wheel, in that order, on the patch the
    /// host froze.
    ///
    /// The order matters: the list is what takes the reader off the first
    /// file, folding is checked on the file the list reached, and the wheel
    /// takes the reader to the last file and back to the first — which is
    /// where the remarks below expect to start.
    private func navigate(_ app: XCUIApplication, _ runner: Runner) throws -> [String: Any] {
        var found: [String: Any] = [:]
        var passed = Set(headings(app))

        // The list of every file, opened from the heading of the one on
        // screen, and a different file picked out of it.
        press(app, "review.files")
        XCTAssertTrue(app.staticTexts["Files"].waitForExistence(timeout: waiting),
                      "the stack of chevrons beside the path opened no list of files")
        // The list is a sheet over the patch, so the row wanted is the lowest
        // thing on screen that begins with that path: the heading of the same
        // file is behind it, near the top.
        let listed = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", Self.middleFile))
            .allElementsBoundByIndex.filter { $0.isHittable }
            .max { $0.frame.minY < $1.frame.minY }
        XCTAssertNotNil(listed, "the file list offered no \(Self.middleFile) to jump to")
        photograph(app, "review-files")
        listed?.tap()
        XCTAssertTrue(waitUntil { self.onScreen(app, Self.firstOfTokens) != nil },
                      "picking \(Self.middleFile) out of the list did not reach it")
        XCTAssertNil(onScreen(app, Self.firstOfParser),
                     "picking a file out of the list left the page where it was")
        found["picked"] = Self.middleFile
        passed.formUnion(headings(app))

        // That file folded away, and opened again. Folding is the heading
        // itself: the chevron beside the path is what it says it is.
        pressFile(app, Self.middleFile)
        XCTAssertEqual(try fileSays(runner, Self.middleFile), "collapsed",
                       "the heading did not say the file was folded away")
        XCTAssertTrue(waitUntil(within: 10) { self.onScreen(app, Self.firstOfTokens) == nil },
                      "folding \(Self.middleFile) away left its lines on the page")
        photograph(app, "review-collapsed")
        found["folded"] = ["file": Self.middleFile, "lines": "hidden"]
        passed.formUnion(headings(app))
        pressFile(app, Self.middleFile)
        XCTAssertEqual(try fileSays(runner, Self.middleFile), "open",
                       "pressing the heading again did not open the file")
        XCTAssertTrue(waitUntil(within: 10) { self.onScreen(app, Self.firstOfTokens) != nil },
                      "opening \(Self.middleFile) again did not bring its lines back")

        // The wheel down the edge: a thumb held against it and dragged, which
        // is the only way to work it. It is drawn as a column of one dot per
        // file over the whole height of the page, so where the thumb ends is
        // which file the page goes to.
        scrub(app, to: 0.93)
        XCTAssertTrue(waitUntil { self.onScreen(app, Self.firstOfWire) != nil },
                      "scrubbing to the foot of the wheel did not reach the last file")
        XCTAssertNil(onScreen(app, Self.firstOfTokens),
                     "scrubbing to the last file left the page on the one before it")
        photograph(app, "review-wheel")
        found["scrubbedTo"] = ["last": "wire.rs"]
        passed.formUnion(headings(app))
        scrub(app, to: 0.08)
        XCTAssertTrue(waitUntil { self.onScreen(app, Self.firstOfParser) != nil },
                      "scrubbing back to the head of the wheel did not reach the first file")
        XCTAssertNil(onScreen(app, Self.firstOfWire),
                     "scrubbing back to the first file left the page on the last")
        var reached = found["scrubbedTo"] as? [String: String] ?? [:]
        reached["first"] = "parser.rs"
        found["scrubbedTo"] = reached
        passed.formUnion(headings(app))
        found["files"] = passed.sorted()
        return found
    }

    /// Every file heading the page is currently showing.
    private func headings(_ app: XCUIApplication) -> [String] {
        app.descendants(matching: .any).matching(identifier: "review.file")
            .allElementsBoundByIndex.map { $0.label }.filter { !$0.isEmpty }
    }

    /// Presses one file's heading, which is the control that folds it.
    ///
    /// Every heading shares one name, so the one wanted is the one whose label
    /// is that path.
    private func pressFile(_ app: XCUIApplication, _ path: String) {
        let heading = app.descendants(matching: .any).matching(identifier: "review.file")
            .allElementsBoundByIndex.first { $0.label == path }
        guard let heading else {
            return XCTFail("no heading on the page is \(path)")
        }
        if heading.isHittable {
            heading.tap()
        } else {
            heading.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    /// What the page says about one file: open, or folded away.
    private func fileSays(_ runner: Runner, _ path: String) throws -> String {
        try declared(runner)
            .first { $0.identifier == "review.file" && $0.label == path }?.value ?? ""
    }

    /// Drags a thumb down the wheel at the right edge to the given fraction of
    /// the screen.
    ///
    /// By coordinate because the wheel is hidden from assistive technology on
    /// purpose — it is a shortcut through a document that is already readable
    /// end to end, and a column of undescribable dots is not something to read
    /// out. A finger is all it answers to, so a finger is what this is.
    private func scrub(_ app: XCUIApplication, to fraction: Double) {
        let from = app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0.5))
        let to = app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: fraction))
        from.press(forDuration: 0.2, thenDragTo: to, withVelocity: .slow, thenHoldForDuration: 0.3)
    }

    /// One line out of each file, which is in that file and in no other. Where
    /// the page is, is which of them is on screen.
    private static let firstOfParser = "fn parse(input: &str) -> Vec<Token> {"
    private static let firstOfTokens = "pub struct Token {"
    private static let firstOfWire = "pub fn encode(tokens: &[Token])"
    private static let middleFile = "tokens.rs"

    /// The field in the comment sheet, which takes the keyboard on arrival.
    private func writable(_ app: XCUIApplication) -> XCUIElement? {
        app.descendants(matching: .any).matching(identifier: "review.commentField")
            .allElementsBoundByIndex.first { $0.isHittable }
    }
}
