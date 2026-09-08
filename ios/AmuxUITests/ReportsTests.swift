import Foundation
import XCTest

/// Reporting a problem from the phone: what the app does when the system
/// photographs it, what it does when somebody goes looking for the row under
/// Help, and what leaves the phone when they press Send.
///
/// One launch from beginning to end, against a real relay and a real machine.
/// What is frozen is what was on screen, so the phone has to have reached
/// something before any of this means anything: it pairs with the machine the
/// journey named and draws that machine's agents, and the picture in the
/// report is that screen.
///
/// The account service is the scripted one, handed to the app at launch. A
/// report is the one thing on this screen that leaves the phone, and the only
/// way a refusal and the retry after it are states a finger can reach is if
/// the far side is a double that says what it will do before it is asked.
/// Nothing here reaches amux.sh.
final class ReportsTests: JourneyCase {
    /// The machine, the code it printed, and where the bundles are to be left
    /// for the Mac to read out of the app's container.
    private struct Cast {
        let machine: String
        let code: String
        let bundle: String
        let helpBundle: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            machine = try required("AMUX_HOST_ID")
            code = try required("AMUX_PIN")
            bundle = try required("AMUX_BUNDLE")
            helpBundle = try required("AMUX_HELP_BUNDLE")
        }
    }

    private var runner: Runner!
    private var cast: Cast!
    /// The one launch every act works in. What a report is about is the state
    /// this phone is in, and relaunching between acts would throw away both
    /// the frozen frame and the machine's answer for it.
    private var app: XCUIApplication!

    /// The three rectangles, as fractions of the frozen picture, and what is
    /// said about each. Drawn wide rather than tall: the picture sits in a
    /// scroll view, and a drag that travelled further down than across would
    /// be the scroll's rather than the frame's.
    private static let boxes: [(from: CGVector, to: CGVector, note: String)] = [
        (CGVector(dx: 0.12, dy: 0.13), CGVector(dx: 0.64, dy: 0.23),
         "the count in this badge is one too many"),
        (CGVector(dx: 0.28, dy: 0.41), CGVector(dx: 0.86, dy: 0.51),
         "this row says it is offline and it is not"),
        (CGVector(dx: 0.10, dy: 0.68), CGVector(dx: 0.72, dy: 0.80),
         "this line is cut off on the right"),
    ]

    private static let note = "The home has been wrong since it reconnected."
    private static let refusal = "amux.sh could not take this report"

    /// One act: what a person does in it, and the cheapest way to leave behind
    /// what doing it leaves behind, so one failing act can be reproduced
    /// without paying for the whole story.
    private struct Act {
        let name: String
        let perform: () throws -> Void
        let shortcut: () throws -> Void

        init(_ name: String, _ perform: @escaping () throws -> Void,
             shortcut: @escaping () throws -> Void = {}) {
            self.name = name
            self.perform = perform
            self.shortcut = shortcut
        }
    }

    private func script() -> [Act] {
        [
            Act("screenshot-thumbnail", aScreenshotWithTheThumbnailPreview),
            Act("screenshot-full-screen", aScreenshotTheSystemThenCoversTheAppOver,
                shortcut: { try self.screenshot() }),
            // The shortcut opens the report and writes nothing on it: what
            // sending does is the same whether or not anything was drawn, and
            // drawing is the slowest thing here.
            Act("annotate", theReportOnTheFrozenFrame,
                shortcut: { self.press(self.app, "report.prompt") }),
            Act("send", sendingItAndTheRefusalBefore,
                shortcut: { self.press(self.app, "report.cancel") }),
            Act("from-help", theSameFlowAskedForDeliberately),
        ]
    }

    func testAPhoneFreezesWhatWasWrongAnnotatesItAndSendsIt() throws {
        runner = try Runner()
        cast = try Cast(ProcessInfo.processInfo.environment)
        defer { try? write("reports.json") }

        app = launch(runner, scripted: true)
        try door(runner, .init(kind: "pairByCode", host: cast.machine, pin: cast.code))
        waitFor(app, "home", "the home never appeared")
        XCTAssertTrue(waitUntil { !self.identifiers(self.app, startingWith: "home.row.").isEmpty },
                      "the machine never answered for anything, so there is no screen worth "
                      + "reporting")
        record["fleetUnderTheReport"] = identifiers(app, startingWith: "home.row.").count

        let acts = script()
        let asked = (ProcessInfo.processInfo.environment["AMUX_ACTS"] ?? "")
            .split(separator: ",").map(String.init).filter { !$0.isEmpty }
        let unknown = asked.filter { name in !acts.contains { $0.name == name } }
        XCTAssertTrue(unknown.isEmpty,
                      "this journey has no act called \(unknown.joined(separator: ", ")); "
                      + "it has \(acts.map { $0.name })")
        guard unknown.isEmpty else { return }
        let last = asked.isEmpty
            ? acts.count - 1
            : (acts.lastIndex { asked.contains($0.name) } ?? acts.count - 1)
        var performed: [String] = []
        var shortcut: [String] = []
        record["actsPerformed"] = performed
        record["actsShortcut"] = shortcut
        for act in acts[...last] {
            if asked.isEmpty || asked.contains(act.name) {
                try act.perform()
                performed.append(act.name)
            } else {
                try act.shortcut()
                shortcut.append(act.name)
            }
            record["actsPerformed"] = performed
            record["actsShortcut"] = shortcut
        }

        let calls = try door(runner, .init(kind: "calls"))
        record["cloudCalls"] = calls["cloud"] as? [String] ?? []
    }

    // MARK: - The acts

    /// The system photographed the app, and the phone is set to show its
    /// preview as a thumbnail in the corner.
    ///
    /// The offer is the app's own control. It cannot be attached to the
    /// system's thumbnail — no app is given one to attach anything to — so it
    /// has to stand clear of where that thumbnail sits, which is the bottom
    /// leading corner and about a hundred points across. The app is never told
    /// the preview's frame and cannot ask, so the gap is the app's decision
    /// and this is where it is checked.
    private func aScreenshotWithTheThumbnailPreview() throws {
        try screenshot()
        waitFor(app, "report.prompt", "a screenshot did not offer to report the screen")
        record["promptSays"] = label(app, "report.prompt")
        let placed = try rectOf("report.prompt")
        record["promptLeftEdge"] = Int(placed.minX.rounded())
        XCTAssertGreaterThanOrEqual(
            placed.minX, 100,
            "the offer sits at \(placed.minX) points from the leading edge, under where the "
            + "system draws its screenshot thumbnail")
        photograph(app, "prompt")

        // Anywhere else is no, and the capture goes with it: an accidental
        // screenshot must cost nothing.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)).tap()
        waitForNo(app, "report.prompt", "the offer stayed after the screen was tapped")
        record["promptAfterTappingElsewhere"] = false
    }

    /// The same screenshot with the other preview setting, where the system's
    /// own picture covers the whole app and the person is left looking at it.
    ///
    /// What the app is put through is being sent to the background and brought
    /// back, which is what a full-screen preview does to it and rather more:
    /// the offer has to be the same offer afterwards, over the same frozen
    /// frame, or somebody who took a screenshot and glanced at it would come
    /// back to nothing.
    private func aScreenshotTheSystemThenCoversTheAppOver() throws {
        try screenshot()
        waitFor(app, "report.prompt", "a screenshot did not offer to report the screen")

        XCUIDevice.shared.press(.home)
        XCTAssertTrue(waitUntil(within: 15) { self.app.state != .runningForeground },
                      "the app never left the foreground")
        app.activate()
        waitFor(app, "home", "the app never came back")
        XCTAssertTrue(element(app, "report.prompt").waitForExistence(timeout: waiting),
                      "the offer was gone once the system stopped covering the app")
        record["promptSurvivedTheSystemPreview"] = true
    }

    /// Taking the offer up: the report opens on the frame that was frozen when
    /// the offer appeared, and three things wrong with it are drawn on and
    /// written about.
    ///
    /// Nothing the system owns is involved. No share sheet is put up, nothing
    /// asks for the photo library, and the picture being drawn on is the app's
    /// own — the system's screenshot went to Photos and this app never sees
    /// it.
    private func theReportOnTheFrozenFrame() throws {
        press(app, "report.prompt")
        waitFor(app, "report.screen", "taking the offer did not open the report")
        record["reportOpensAt"] = try says("report.screen")
        record["frameWhenOpened"] = try says("report.frame")
        record["sheetsWhileReporting"] = app.sheets.count
        record["systemAlertsWhileReporting"] =
            XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.count
        XCTAssertEqual(app.sheets.count, 0,
                       "something was presented over the report: \(app.debugDescription)")

        // Every rectangle first, then everything written about them. A
        // keyboard raised between two drags would be over the picture the next
        // one is drawn on, which is not something the picture can do anything
        // about — and is not how anybody marks three things up either.
        record["pictureOnScreen"] = try describe(rectOf("report.frame"))
        for (at, box) in Self.boxes.enumerated() {
            try draw(box.from, to: box.to)
            let marked = try waitForValue(runner, "report.frame", "\(at + 1) marked")
            XCTAssertEqual(marked, "\(at + 1) marked",
                           "drawing box \(at + 1) left the frame saying \(marked)")
        }
        // The keyboard is put away after each note. Raised, it covers the
        // bottom third of the page and the report is almost exactly one screen
        // tall, so the next thing to write is behind it until it goes.
        for (at, box) in Self.boxes.enumerated() {
            try type(box.note, into: "report.mark.\(at).note", readBackFrom: "report.mark.\(at)")
            putTheKeyboardAway(dragging: "report.mark.\(at).remove")
        }
        try type(Self.note, into: "report.note")
        putTheKeyboardAway(dragging: "report.mark.\(Self.boxes.count - 1).remove")
        try? app.debugDescription.write(
            to: Self.inContainer("report-tree.txt"), atomically: true, encoding: .utf8)

        let written = try declared(runner)
        record["marksOnTheFrame"] = try says("report.frame")
        record["marksWritten"] = Self.boxes.indices.map {
            said(written, "report.mark.\($0)")?.value ?? ""
        }
        record["noteWritten"] = said(written, "report.note")?.value
        photograph(app, "report-screen")
    }

    /// Sending it: turned down once, and the same report sent again by the
    /// press that offered to.
    ///
    /// A refusal must lose nothing. Three rectangles and four notes are what
    /// somebody spent their attention on, and a report that had to be written
    /// twice is a report nobody writes.
    private func sendingItAndTheRefusalBefore() throws {
        try scriptCloud(["upload": "refused", "uploadReason": Self.refusal])
        press(app, "report.send")
        waitFor(app, "report.refusal", "a refused upload said nothing")
        record["refusalSaid"] = try says("report.refusal")
        record["offersAfterRefusal"] = try called("report.send")
        let kept = try declared(runner)
        record["marksAfterRefusal"] = Self.boxes.indices.map {
            said(kept, "report.mark.\($0)")?.value ?? ""
        }
        record["noteAfterRefusal"] = said(kept, "report.note")?.value

        try scriptCloud(["upload": "accepted", "receipt": "report-7c2"])
        press(app, "report.send")
        waitFor(app, "report.sent", "an accepted report said nothing")
        record["receipt"] = try says("report.sent")
        record["stateAfterSending"] = try says("report.screen")

        let uploaded = try door(runner, .init(kind: "uploaded", path: cast.bundle))
        record["uploadedParts"] = uploaded["parts"] as? [String] ?? []
        // Read here rather than at the end of the journey: the report asked
        // for under Help is handed over afterwards, and what this act claims
        // is that a refusal and the retry after it were two hand-offs of one
        // report.
        record["uploadsWhenSent"] = try uploads()
    }

    /// Report a Problem under Help: the same flow, asked for rather than
    /// offered, with no prompt in between — somebody who went looking for the
    /// row has already said yes.
    private func theSameFlowAskedForDeliberately() throws {
        if element(app, "report.screen").exists {
            press(app, "report.cancel")
            waitForNo(app, "report.screen", "Cancel did not close the report")
        }
        pressTab(app, "You")
        waitFor(app, "you", "the You page never appeared")

        press(app, "you.report")
        waitFor(app, "report.screen", "Report a Problem did not open the report")
        record["offeredBeforeTheHelpReport"] = element(app, "report.prompt").exists
        XCTAssertFalse(element(app, "report.prompt").exists,
                       "the deliberate path stopped to offer what had already been asked for")
        record["helpReportOpensAt"] = try says("report.screen")
        record["helpFrameWhenOpened"] = try says("report.frame")

        try scriptCloud(["upload": "accepted", "receipt": "report-help"])
        press(app, "report.send")
        waitFor(app, "report.sent", "the report from Help was never accepted")
        record["helpReceipt"] = try says("report.sent")
        let uploaded = try door(runner, .init(kind: "uploaded", path: cast.helpBundle))
        record["helpUploadedParts"] = uploaded["parts"] as? [String] ?? []
        record["uploadsAfterHelp"] = try uploads()
    }

    // MARK: - Doing it

    /// The system has just taken a screenshot, said the way the system says
    /// it. An app is given the notification and nothing else — the picture is
    /// already taken and already saved, and no app can intercept the gesture —
    /// so this is the whole of what the app has to go on.
    private func screenshot() throws {
        try door(runner, .init(kind: "screenshot"))
    }

    /// Puts the keyboard away, by the swipe the report itself offers: the page
    /// dismisses it interactively, so dragging the content down is what a
    /// person does when they have finished writing.
    ///
    /// The drag begins on a rectangle's own Remove button. Above the notes is
    /// the frozen picture, where a drag would draw another rectangle instead
    /// of scrolling, and on the field itself a drag moves the caret — a button
    /// claims neither, so the scroll gets it.
    private func putTheKeyboardAway(dragging handle: String) {
        for _ in 0..<3 {
            guard app.keyboards.firstMatch.exists else { return }
            let button = element(app, handle)
            guard button.exists else { break }
            let from = button.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            from.press(
                forDuration: 0.1,
                thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98)))
            _ = waitUntil(within: 5) { !self.app.keyboards.firstMatch.exists }
        }
        if app.keyboards.firstMatch.exists {
            record["keyboardWouldNotGoAwayAfter.\(handle)"] = true
        }
    }

    /// Drags a rectangle across the frozen picture, from a press held long
    /// enough that the picture takes the touch rather than the scroll it sits
    /// in — which is what a finger marking something out does anyway.
    ///
    /// Where the picture is is asked of the app rather than measured off the
    /// system's tree. A name declared on a screen reaches that tree on every
    /// element under it, so the first match for the picture stops being the
    /// picture as soon as a rectangle is drawn on it — and a second drag
    /// placed against that match would be measured across a rectangle a few
    /// points wide.
    private func draw(_ from: CGVector, to: CGVector) throws {
        let picture = try rectOf("report.frame")
        func at(_ corner: CGVector) -> XCUICoordinate {
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(
                dx: picture.minX + picture.width * corner.dx,
                dy: picture.minY + picture.height * corner.dy))
        }
        at(from).press(forDuration: 0.6, thenDragTo: at(to))
    }

    /// Types into a field the way a person does: the keyboard, into the field
    /// they tapped. What was typed is then read back off the screen's own
    /// name for it, so a press that missed cannot pass for one that landed.
    private func type(
        _ text: String, into identifier: String, readBackFrom: String? = nil
    ) throws {
        let field = app.textFields.matching(identifier: identifier).firstMatch
        guard field.waitForExistence(timeout: waiting) else {
            return XCTFail("there is no field named \(identifier)")
        }
        if !waitUntil(within: 10, { field.isHittable }) {
            // What the screen was when a finger could not reach the field,
            // left where the Mac collects it: a frame under the keyboard and
            // a frame that was never laid out look the same from here.
            record["unreachable.\(identifier)"] = "\(field.frame)"
            record["keyboardWhenUnreachable"] = app.keyboards.firstMatch.exists
            photograph(app, "report-unreachable")
            try? app.debugDescription.write(
                to: Self.inContainer("report-tree.txt"), atomically: true, encoding: .utf8)
            return XCTFail("\(identifier) never came somewhere a finger could reach it")
        }
        field.tap()
        field.typeText(text)
        // Read back off the name the screen states the writing under, which
        // for a rectangle's note is the card the field sits in.
        let says = readBackFrom ?? identifier
        let read = try waitForValue(runner, says, text)
        XCTAssertEqual(read, text, "\(says) reads \(read) after it was typed into")
    }

    // MARK: - Reading

    /// Every report the account service has been handed so far, as it
    /// recorded them.
    private func uploads() throws -> [String] {
        let calls = try door(runner, .init(kind: "calls"))
        return (calls["cloud"] as? [String] ?? []).filter { $0.hasPrefix("uploadReport") }
    }

    private func says(_ identifier: String) throws -> String? {
        said(try declared(runner), identifier)?.value
    }

    private func called(_ identifier: String) throws -> String? {
        said(try declared(runner), identifier)?.label
    }

    /// Where a named thing is on screen, in the window's own points.
    ///
    /// Asked of the app's own door rather than measured off the system's tree:
    /// the frame the app laid the control out at is the one its own decision
    /// about placement is made in, and it is the app's placement that has to
    /// stand clear of the system's preview.
    private func rectOf(_ identifier: String) throws -> CGRect {
        try door(runner, .init(kind: "settle"))
        let answer = try door(runner, .init(kind: "query"))
        let elements = (answer["state"] as? [String: Any])?["elements"] as? [[String: Any]] ?? []
        let found = elements.first { $0["identifier"] as? String == identifier }
        let frame = try XCTUnwrap(
            found?["frame"] as? [String: Any], "nothing on screen is named \(identifier)")
        return CGRect(
            x: frame["x"] as? Double ?? 0, y: frame["y"] as? Double ?? 0,
            width: frame["width"] as? Double ?? 0, height: frame["height"] as? Double ?? 0)
    }

    private func describe(_ rectangle: CGRect) -> String {
        "\(Int(rectangle.minX.rounded())),\(Int(rectangle.minY.rounded())) "
        + "\(Int(rectangle.width.rounded()))x\(Int(rectangle.height.rounded()))"
    }

    /// What the account service will answer from here on. Everything unsaid
    /// keeps the answer it had.
    private func scriptCloud(_ said: [String: Any]) throws {
        try door(runner, .init(kind: "cloud", cloud: said))
    }
}
