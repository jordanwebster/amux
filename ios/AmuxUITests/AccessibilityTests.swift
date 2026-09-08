import Foundation
import UIKit
import XCTest

/// The app used the way somebody uses it who has turned the accessibility
/// settings on: every screen drawn at the largest text size there is, with
/// VoiceOver running on the device, against machines the runner is really
/// running.
///
/// It is the same work the other journeys do — the fleet, a conversation, an
/// ask answered, a patch written about, a message composed, the machines — cut
/// down to the primary path through each and driven again under the settings
/// that change how everything is laid out and read out. What it is looking for
/// is what those settings break: a control that is no longer reachable, a
/// sheet that cannot be got out of, a name VoiceOver has nothing to say about,
/// a failure that is drawn in one appearance and lost in the other.
///
/// VoiceOver is genuinely running. The journey turns it on on the device
/// before this starts, which is the only place it can be turned on from, and
/// the app is asked whether it is in a VoiceOver session rather than trusted
/// to be: a run that wrote the setting and never checked would be making a
/// claim about a preferences file. Under it the accessibility tree is the
/// tree the system builds for a screen reader, and `isHittable` stops meaning
/// what it means without one — so nothing here is found by it.
final class AccessibilityTests: JourneyCase {
    /// The largest size a reader can ask for. Everything is drawn at it,
    /// because a layout that survives this survives the five below it.
    private static let size = "accessibility5"

    /// The smallest a control may be, in points, in either direction.
    private static let smallest: CGFloat = 44

    private var runner: Runner!
    private var control: Lines!
    private var app: XCUIApplication!
    /// The second machine, which is what makes the Hosts tab a list.
    private var desktop: (identity: String, pin: String)!
    /// The agent every ask in this run is raised on, by the name its machine
    /// knows it by.
    private var asking: String!
    /// The same agent, by the identity the phone knows it by.
    private var askingAgent: String!

    /// One act: a stretch of the primary path through one part of the app.
    private struct Act {
        let name: String
        let perform: () throws -> Void

        init(_ name: String, _ perform: @escaping () throws -> Void) {
            self.name = name
            self.perform = perform
        }
    }

    private func script() -> [Act] {
        [
            Act("home", theFleetAtTheLargestSize),
            Act("conversation", aConversationAndThePatchItOffers),
            Act("asks", everyPanelAnsweredAndOneGotOutOf),
            Act("writing", aMessageWrittenAndTakenApart),
            Act("hosts", theMachinesThisPhoneWorksOn),
            Act("failures", aMachineLostInBothAppearances),
        ]
    }

    func testThePrimaryJourneysUnderVoiceOverAtTheLargestSize() throws {
        runner = try Runner()
        control = try Lines(address: runner.control)
        let environment = ProcessInfo.processInfo.environment
        desktop = (try XCTUnwrap(environment["AMUX_DESKTOP"], "the journey did not pass AMUX_DESKTOP"),
                   try XCTUnwrap(environment["AMUX_DESKTOP_PIN"],
                                 "the journey did not pass AMUX_DESKTOP_PIN"))
        asking = try XCTUnwrap(environment["AMUX_ASKING"], "the journey did not pass AMUX_ASKING")
        askingAgent = try XCTUnwrap(environment["AMUX_ASKING_AGENT"],
                                    "the journey did not pass AMUX_ASKING_AGENT")
        defer { try? write("accessibility.json") }

        app = launch(runner)
        // The size before the trust: what this journey is about is how the app
        // is laid out, and a fleet that arrived while it was still drawn at
        // the ordinary size would have laid its rows out twice.
        try door(runner, .init(kind: "dynamicType", size: Self.size))
        try pairThroughTheDoor(runner)
        try door(runner, .init(kind: "pairByCode", host: desktop.identity, pin: desktop.pin))

        // The two settings that make this run what it is, read back off the
        // app rather than off what was asked for.
        let opening = try door(runner, .init(kind: "query"))
        let state = opening["state"] as? [String: Any] ?? [:]
        record["voiceOver"] = state["voiceOver"] as? Bool ?? false
        record["typeSize"] = state["typeSize"] as? String ?? ""
        XCTAssertEqual(state["voiceOver"] as? Bool, true,
                       "this run is not a VoiceOver session; the app says it is not")
        XCTAssertEqual(state["typeSize"] as? String, Self.size,
                       "the app is drawn at \(state["typeSize"] ?? "nothing") and this journey "
                       + "is about \(Self.size)")

        let acts = script()
        let asked = (environment["AMUX_ACTS"] ?? "")
            .split(separator: ",").map(String.init).filter { !$0.isEmpty }
        let unknown = asked.filter { name in !acts.contains { $0.name == name } }
        XCTAssertTrue(unknown.isEmpty,
                      "this journey has no act called \(unknown.joined(separator: ", ")); "
                      + "it has \(acts.map { $0.name })")
        guard unknown.isEmpty else { return }

        var performed: [String] = []
        var skipped: [String] = []
        // Every act starts from the home and every act is reached from there,
        // so an act asked for on its own costs the launch and nothing else:
        // there is nothing an earlier act leaves behind that a later one needs.
        for act in acts {
            guard asked.isEmpty || asked.contains(act.name) else {
                skipped.append(act.name)
                continue
            }
            try act.perform()
            performed.append(act.name)
            record["actsPerformed"] = performed
            record["actsShortcut"] = skipped
        }
        record["actsPerformed"] = performed
        record["actsShortcut"] = skipped
    }

    // MARK: - The acts

    /// The fleet, at the largest size a reader can ask for.
    private func theFleetAtTheLargestSize() throws {
        try home()
        waitFor(app, "home.row.\(runner.agent)", "the machine never answered for \(runner.agent)")
        record["home"] = try audited("home", reaching: ["home.newAgent",
                                                        "home.row.\(runner.agent)"])
        record["homeRows"] = identifiers(app, startingWith: "home.row.").count
        XCTAssertGreaterThan(identifiers(app, startingWith: "home.row.").count, 1,
                             "the fleet drew fewer rows than the two machines are running")
        photograph(app, "accessibility-home")
    }

    /// A conversation, and the patch its machine froze — read at the largest
    /// size and written about with a finger.
    private func aConversationAndThePatchItOffers() throws {
        try openTheConversation()
        record["conversation"] = try audited(
            "conversation", reaching: ["conversation.drawer", "conversation.overflow",
                                       "composer.attach"])

        // The patch, asked of the machine and taken hold of by dragging down
        // its lines. A range is the one thing on this page a reader has to be
        // able to take, and the rows it is taken from are twice the height
        // they are at the ordinary size.
        try door(runner, .init(kind: "requestChanges", agent: runner.agent, base: ""))
        if !waitUntil({ self.element(self.app, "conversation.changes").exists }) {
            record["diagnosis"] = [
                "screen": try declared(runner).map { "\($0.identifier)=\($0.value)" },
                "bridge": try door(runner, .init(kind: "bridge")),
            ]
            XCTFail("the machine answered with no changes to review")
        }
        press(app, "conversation.changes")
        waitFor(app, "review", "the changes chip did not lead to the patch")
        record["review"] = try audited("review", reaching: ["review.back"])

        // One range taken and let go of again: a sheet a reader cannot get
        // out of is a trap, and this is the one this journey opens.
        let cancelled = try takeARange()
        press(app, "review.cancelComment")
        waitForNo(app, "review.commentSheet", "cancelling the remark left the sheet open")
        record["sheetCancelled"] = cancelled

        // And one taken, written about and kept, so the range that was let go
        // of can be told from a page that never took one.
        let kept = try takeARange()
        let remark = try XCTUnwrap(
            app.descendants(matching: .any).matching(identifier: "review.commentField")
                .allElementsBoundByIndex.last,
            "the comment sheet offered nowhere to write")
        remark.tap()
        remark.typeText("The largest text size still leaves this line readable.")
        photograph(app, "accessibility-review")
        press(app, "review.addComment")
        waitForNo(app, "review.commentSheet", "adding the remark left the sheet open")
        record["sheetKept"] = kept
        XCTAssertEqual(label(app, "review.attach"), "Attach Review \u{00B7} 1 comment",
                       "one remark was written and the page offers to attach "
                       + "\(self.label(app, "review.attach") ?? "nothing")")
        press(app, "review.back")

        // One message, so there is something to read. This agent has said
        // nothing yet, and a feed with no rows in it would prove nothing about
        // whether a feed is readable at this size. It is sent after the patch
        // and not before: a machine part way through a turn has no changes to
        // freeze, and asking it for them then is asking the wrong question.
        waitFor(app, "composer", "coming back from the patch left nowhere to write")
        record["transcript"] = try sayOneThing()
        photograph(app, "accessibility-conversation")
        try backToTheHome()
    }

    /// Everything a machine can be waiting to be told, answered at the largest
    /// size — and one panel got out of rather than answered.
    private func everyPanelAnsweredAndOneGotOutOf() throws {
        // The agent the asks are raised on, because a panel belongs to the
        // conversation it was raised in and nowhere else.
        try openTheConversation(askingAgent, called: asking)

        try control.ask(["AgentRaiseAsk": ["agent": asking as Any, "ask": ["Permission": [
            "tool": "Bash", "invocation": ["tool": "bash", "command": "cargo test"],
            "scoped_directories": ["/work/scratch"],
        ]]]])
        waitFor(app, "ask.allow", "the permission never reached the phone")
        record["permission"] = try audited("ask.permission",
                                           reaching: ["ask.allow", "ask.deny", "ask.scope"])
        photograph(app, "accessibility-ask")
        answer(app, "ask.deny", "denying the permission left the panel up")

        // A plan, opened out and then sent back. Send Back asks what should
        // change, and that question is the second sheet this journey cancels:
        // it arrives over a panel that is itself over the transcript, which is
        // the deepest a reader gets at this size.
        try control.ask(["AgentRaiseAsk": [
            "agent": asking as Any,
            "ask": ["Plan": ["markdown": "## What I would do next\n\n1. Read the parser\n"]],
        ]])
        waitFor(app, "ask.approve", "the plan never reached the phone")
        press(app, "ask.plan.more")
        XCTAssertEqual(try waitForValue(runner, "ask.plan", "open"), "open",
                       "the grabber did not open the rest of the plan at \(Self.size)")
        record["plan"] = try audited("ask.plan",
                                     reaching: ["ask.approve", "ask.sendback", "ask.plan.more"])
        press(app, "ask.sendback")
        waitFor(app, "ask.feedback", "Send Back did not ask what should change")
        record["feedback"] = try audited("ask.feedback",
                                         reaching: ["ask.feedback.send", "ask.feedback.cancel"])
        press(app, "ask.feedback.cancel")
        waitForNo(app, "ask.feedback", "cancelling what should change left the sheet open")
        XCTAssertTrue(element(app, "ask.approve").waitForExistence(timeout: waiting),
                      "getting out of the sheet took the plan with it")
        answer(app, "ask.approve", "approving the plan left the panel up")
        try backToTheHome()
    }

    /// A message written at the largest size, with a token put into it and
    /// taken out of it again, and a card opened over it and cancelled.
    private func aMessageWrittenAndTakenApart() throws {
        try openTheConversation()
        let field = writable()
        field.tap()
        field.typeText("Read the parser")
        record["typed"] = try waitForSpoken(containing: "Read the parser",
                                            "what was typed never reached the field")
        // Read after something has been written, because the button beside the
        // field is Send only where there is a message to send: an empty
        // composer offers Stop instead, and auditing then would be auditing a
        // different control.
        record["composer"] = try audited("composer",
                                         reaching: ["composer.attach", "composer.send"])

        // A token, which is where the largest size and a text view meet: the
        // chip is drawn by UIKit and has to grow with the reader's size like
        // everything around it, and one press of the key that takes a letter
        // has to take the whole of it.
        try door(runner, .init(kind: "paste", text: Self.pasted, identifier: "composer.field"))
        record["afterPaste"] = try waitForSpoken(
            containing: "[Pasted text \u{00B7} 14 lines]",
            "a paste of fourteen lines did not become one token")
        photograph(app, "accessibility-composer")
        field.typeText(XCUIKeyboardKey.delete.rawValue)
        let afterDelete = try waitForSpoken(containing: "Read the parser",
                                            "the field lost more than the token")
        XCTAssertFalse(afterDelete.contains("Pasted text"),
                       "one press left part of the token behind: \(afterDelete)")
        record["afterRemove"] = afterDelete
        press(app, "composer.clear")
        try waitUntilTheField("clearing left something in the field") { $0.isEmpty }

        // The card that renames the agent, opened and cancelled. It is the
        // third thing this journey gets out of, and the one that covers the
        // whole screen at this size.
        press(app, "conversation.overflow")
        waitFor(app, "overflow", "the ellipsis opened nothing")
        record["overflow"] = try audited("overflow",
                                         reaching: ["overflow.rename", "overflow.delete-agent"])
        press(app, "overflow.rename")
        waitFor(app, "rename", "the rename row opened no card")
        record["rename"] = try audited("rename", reaching: ["rename.cancel", "rename.confirm"])
        photograph(app, "accessibility-rename")
        press(app, "rename.cancel")
        waitForNo(app, "rename", "cancelling the rename left the card open")
        waitFor(app, "composer", "cancelling the rename did not give the composer back")
        try backToTheHome()
    }

    /// The machines this phone works on, listed at the largest size.
    private func theMachinesThisPhoneWorksOn() throws {
        pressTab(app, "Hosts")
        waitFor(app, "hosts", "the Hosts tab drew nothing")
        record["hosts"] = try audited("hosts", reaching: ["hosts.pair"])
        let listed = identifiers(app, startingWith: "hosts.row.")
        record["hostsListed"] = listed
        XCTAssertEqual(listed.count, 2,
                       "this phone is paired with two machines and the tab lists \(listed)")
        photograph(app, "accessibility-hosts")
        pressTab(app, "Agents")
        try backToTheHome()
    }

    /// The machine taken away while somebody is reading one of its agents,
    /// said in both appearances.
    ///
    /// A failure a reader cannot make out is a failure they cannot act on, so
    /// what is claimed here is that the same sentence is on screen in light
    /// and in dark and that both were photographed at this size.
    private func aMachineLostInBothAppearances() throws {
        try openTheConversation()
        // Something in the feed first, so that what survives the machine going
        // away is a transcript somebody was reading rather than an empty one.
        try sayOneThing()
        if element(app, "conversation.finished").waitForExistence(timeout: 5) {
            press(app, "ask.later")
        }
        try control.ask("CloudOffline")
        let gone = app.staticTexts["\(runner.host) is unreachable"]
        XCTAssertTrue(gone.waitForExistence(timeout: waiting),
                      "losing the machine left the conversation saying nothing about it")
        var said: [String: String] = [:]
        for appearance in ["light", "dark"] {
            try door(runner, .init(kind: "appearance", appearance: appearance))
            try door(runner, .init(kind: "settle"))
            let spoken = app.staticTexts["\(runner.host) is unreachable"]
            XCTAssertTrue(spoken.waitForExistence(timeout: waiting),
                          "the \(appearance) appearance does not say the machine has gone")
            said[appearance] = spoken.label
            record["failure.\(appearance)"] = try audited("failure.\(appearance)", reaching: [])
            photograph(app, "accessibility-failure-\(appearance)")
        }
        record["failureSays"] = said
        XCTAssertEqual(said["light"], said["dark"],
                       "the two appearances say different things about the same lost machine")
        XCTAssertFalse(transcriptRows(app).isEmpty,
                       "losing the machine emptied the transcript")
        try door(runner, .init(kind: "appearance", appearance: "light"))
        try control.ask("CloudOnline")
    }

    // MARK: - What an act finds out about a screen

    /// Everything on one screen, judged as a reader who cannot see it and a
    /// thumb that cannot aim would judge it, plus how far the actions this
    /// screen is for are from the bottom of the window.
    ///
    /// Names and kinds come from XCUITest, which is the accessibility client;
    /// the rectangle comes from the screen itself, because the frame XCUITest
    /// reports is the accessibility frame and not the one the screen laid out
    /// — a round control drawn at 44x44 comes back 42x42. Reachability is
    /// reported rather than asserted: how far up a screen a control may sit
    /// before a thumb cannot get to it is a judgement about a hand, and this
    /// says where each one is and leaves the judging to whoever reads it.
    @discardableResult
    private func audited(_ screen: String, reaching primary: [String]) throws -> [String: Any] {
        let laidOut = try laidOut()
        let window = app.frame
        var faults: [String] = []
        var counted = 0
        for control in controls() {
            counted += 1
            let named = control.identifier.isEmpty ? "an unnamed control" : control.identifier
            if control.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                faults.append("\(screen): \(named) has nothing for VoiceOver to read out")
            }
            let frame = judged(control, laidOut: laidOut)
            if frame.width < Self.smallest || frame.height < Self.smallest {
                faults.append(String(
                    format: "%@: %@ (\"%@\") is %.0f×%.0f pt, under the %d pt a thumb needs",
                    screen, named, control.label, frame.width, frame.height,
                    Int(Self.smallest)))
            }
        }
        var reach: [[String: Any]] = []
        for identifier in primary {
            let frame = (laidOut[identifier] ?? []).first
            let onScreen = element(app, identifier).exists
            reach.append([
                "identifier": identifier,
                "onScreen": onScreen,
                "label": label(app, identifier) ?? "",
                // From the bottom of the window to the nearest edge of the
                // control: how far up the screen a thumb has to travel to
                // reach it at all. Nought where the control runs to the foot
                // of the window, which is where a thumb already is.
                "pointsFromTheBottom": frame.map {
                    Int(max(0, window.maxY - $0.maxY).rounded())
                } ?? -1,
                "points": frame.map { String(format: "%.0f×%.0f", $0.width, $0.height) } ?? "",
            ])
            XCTAssertTrue(onScreen,
                          "\(screen) is missing \(identifier), which is what this screen is for")
            XCTAssertFalse((label(app, identifier) ?? "").isEmpty,
                           "\(identifier) has nothing for VoiceOver to read out")
        }
        // Faults are recorded and not asserted on: whether every control in
        // this build is named and big enough is the accessibility audit's
        // question, swept over every state rather than over the handful a
        // journey walks through, and a second copy of that verdict here would
        // fail this run for something it is not about.
        record["faults"] = (record["faults"] as? [String] ?? []) + faults
        return ["controls": counted, "faults": faults, "reach": reach]
    }

    /// Everything on screen a person can press, as the accessibility client
    /// sees it. Not filtered by `isHittable`: under VoiceOver that answers
    /// about the screen reader's own cursor rather than about a finger.
    private func controls() -> [XCUIElement] {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .link]
        return kinds.flatMap { kind in
            app.descendants(matching: kind).allElementsBoundByIndex.filter { control in
                control.exists && control.frame.width > 0 && control.frame.height > 0
                    && !Self.notOurs.contains { control.identifier.hasPrefix($0) }
            }
        }
    }

    /// Controls this app does not draw and cannot change: the system keyboard
    /// and the status bar report their drawing rather than their hit area.
    private static let notOurs = ["UIKeyboard", "Key.", "com.apple."]

    /// Where the screen laid each named thing out, asked of the screen itself.
    private func laidOut() throws -> [String: [CGRect]] {
        let answered = try door(runner, .init(kind: "query"))
        let elements = (answered["state"] as? [String: Any])?["elements"] as? [[String: Any]] ?? []
        var found: [String: [CGRect]] = [:]
        for element in elements {
            guard let identifier = element["identifier"] as? String, !identifier.isEmpty,
                  let frame = element["frame"] as? [String: Any],
                  let x = frame["x"] as? Double, let y = frame["y"] as? Double,
                  let width = frame["width"] as? Double, let height = frame["height"] as? Double
            else { continue }
            found[identifier, default: []].append(
                CGRect(x: x, y: y, width: width, height: height))
        }
        return found
    }

    /// The rectangle to judge one control on: the smallest one the screen
    /// declared under that name that could hold what XCUITest found, and
    /// XCUITest's own where the screen declared none.
    private func judged(_ control: XCUIElement, laidOut: [String: [CGRect]]) -> CGRect {
        let frame = control.frame
        let holding = (laidOut[control.identifier] ?? []).filter {
            $0.width >= frame.width && $0.height >= frame.height
        }
        return holding.min(by: { $0.width * $0.height < $1.width * $1.height }) ?? frame
    }

    // MARK: - Getting about

    private func home() throws {
        XCTAssertTrue(element(app, "home").waitForExistence(timeout: waiting),
                      "the app did not open on the fleet")
    }

    private func backToTheHome() throws {
        pressTab(app, "Agents")
        XCTAssertTrue(waitUntil { self.element(app, "home").exists },
                      "leaving did not come back to the fleet")
    }

    /// One agent's conversation, opened from the fleet.
    private func openTheConversation(
        _ agent: String? = nil, called subject: String? = nil
    ) throws {
        let opening = agent ?? runner.agent
        if !element(app, "home").exists { try backToTheHome() }
        waitFor(app, "home.row.\(opening)", "the machine never answered for \(opening)")
        try pressWhereItIsDrawn("home.row.\(opening)")
        waitFor(app, "conversation", "opening the row did not lead to a conversation")
        // Which conversation was opened, and not merely that one was: a row
        // at this size is taller than the phone, and a press aimed at the
        // middle of it lands on whatever is drawn where its middle would have
        // been. Opening the wrong agent would satisfy every assertion below.
        let opened = said(try declared(runner), "conversation.subject")?.label ?? ""
        let wanted = subject ?? ProcessInfo.processInfo.environment["AMUX_SUBJECT"] ?? ""
        XCTAssertTrue(opened.hasPrefix(wanted),
                      "pressing the row opened \(opened), which is not \(wanted)")
        // A conversation that has just opened is still catching up: it draws
        // its chrome before the layer under it has anything to say, and a
        // screen read in that moment is missing everything an act is about.
        waitFor(app, "composer", "the conversation offered nowhere to write")
    }

    /// Presses a control where the screen actually drew it, on the part of it
    /// that is on screen.
    ///
    /// At the largest text size a row is taller than the phone, and the tap
    /// XCUITest sends at the middle of such an element lands off the screen —
    /// or, worse, on whatever is drawn where that middle would have been. So
    /// the rectangle the screen declared is intersected with the window and
    /// the middle of what is left is what gets pressed, which is where a
    /// finger would go.
    private func pressWhereItIsDrawn(_ identifier: String) throws {
        var visible = CGRect.null
        for attempt in 0..<12 {
            let frame = (try laidOut()[identifier] ?? []).first
            visible = (frame ?? .null).intersection(reachable())
            if !visible.isNull, visible.height >= Self.smallest, visible.width > 0 { break }
            // Scrolled towards it rather than jumped to: a list at this size
            // is several screens long and what is wanted may be above as well
            // as below. Down first, because a fleet puts what needs answering
            // at the top and everything else under it.
            if attempt < 8 { app.swipeUp(velocity: .slow) } else { app.swipeDown(velocity: .slow) }
        }
        guard !visible.isNull, visible.height >= Self.smallest else {
            return XCTFail("scrolling never brought \(identifier) far enough onto the screen "
                           + "to press: \(visible)")
        }
        app.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: visible.midX, dy: visible.midY))
            .tap()
    }

    /// The band of the window a press can land in: not under the chrome at
    /// the top and not under the tab bar at the foot, both of which float over
    /// what is being scrolled.
    private func reachable() -> CGRect {
        app.frame.inset(by: UIEdgeInsets(top: 150, left: 0, bottom: 160, right: 0))
    }

    // MARK: - Taking a range of a patch

    /// Fourteen lines, so the paste in the composer is a token rather than a
    /// paragraph.
    private static let pasted = (1...14).map { "line \($0)" }.joined(separator: "\n")

    /// Holds one line of the patch and drags onto the next, which is how a
    /// range is taken. Answers with what the sheet said the range was.
    ///
    /// The rows are found by what they say, because a line of a diff carries
    /// no name of its own, and they are not filtered by `isHittable`: under
    /// VoiceOver that is not the question, and a row this drag can start from
    /// is one the tree reports a rectangle for.
    private func takeARange() throws -> [String: String] {
        var rows: [XCUIElement] = []
        waitUntil(within: 20) {
            rows = self.app.descendants(matching: .any)
                .matching(NSPredicate(format: "label BEGINSWITH %@", "Line "))
                .allElementsBoundByIndex
                .filter { $0.exists && $0.frame.height > 0 && self.app.frame.contains($0.frame) }
            return rows.count > 1
        }
        guard rows.count > 1 else {
            throw Lines.Failure("the patch shows \(rows.count) whole lines on screen at "
                                + "\(Self.size), and taking a range needs two")
        }
        rows[0].press(forDuration: 0.6, thenDragTo: rows[1])
        waitFor(app, "review.commentSheet", "holding a range of lines opened no sheet")
        let sheet = said(try declared(runner), "review.commentSheet")
        return ["says": sheet?.label ?? "", "lines": sheet?.value ?? ""]
    }

    /// Sends one message and waits for the feed to hold it, and answers with
    /// what the feed then holds.
    @discardableResult
    private func sayOneThing() throws -> [String] {
        let field = writable()
        field.tap()
        field.typeText("Read the parser and tell me where the newline goes.")
        press(app, "composer.send")
        XCTAssertTrue(waitUntil { !self.transcriptRows(self.app).isEmpty },
                      "sending a message at \(Self.size) put nothing in the feed")
        return transcriptRows(app)
    }

    // MARK: - The composer's field

    private func writable() -> XCUIElement {
        let named = app.textViews.matching(identifier: "composer.field")
            .allElementsBoundByIndex.first { $0.exists }
        return named ?? app.descendants(matching: .any)
            .matching(identifier: "composer.field").firstMatch
    }

    private func spoken() throws -> String {
        said(try declared(runner, settling: false), "composer")?.value ?? ""
    }

    @discardableResult
    private func waitForSpoken(containing needle: String, _ complaint: String) throws -> String {
        var seen = ""
        for _ in 0..<40 {
            seen = try spoken()
            if seen.contains(needle) { return seen }
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
        XCTFail("\(complaint); the field says \(seen.isEmpty ? "nothing" : seen)")
        return seen
    }

    private func waitUntilTheField(_ complaint: String, _ condition: (String) -> Bool) throws {
        var seen = ""
        for _ in 0..<40 {
            seen = try spoken()
            if condition(seen) { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
        XCTFail("\(complaint); the field says \(seen.isEmpty ? "nothing" : seen)")
    }
}
