import Foundation
import XCTest

/// Everything an agent can be waiting to be told, answered on a phone against
/// a machine the runner is really running.
///
/// Every panel here is raised by a real host over a real relay and answered by
/// a finger; nothing fills a store and nothing is simulated on this side. What
/// the host received is not asserted here at all — the journey asks the runner
/// afterwards, because a panel that redrew itself while the answer went
/// nowhere would pass every assertion a screen can make.
final class AskTests: JourneyCase {
    private static let planToJudge = """
    ## What I would do next

    1. Move the token table into its own module
    2. Give `Token::new` the span it is missing
    3. Rewrite the round-trip test around spans
    """

    private static let planToSendBack = """
    ## A second idea

    1. Rewrite the lexer by hand
    2. Delete the round-trip test
    """

    func testEveryAskIsAnsweredOnTheHost() throws {
        let runner = try Runner()
        let control = try Lines(address: runner.control)
        let app = launch(runner)
        try pairThroughTheDoor(runner)

        // MARK: The agent, opened.
        waitFor(app, "home.row.\(runner.agent)",
                "the machine never answered for \(runner.agent)")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation", "opening the row did not lead to a conversation")

        // MARK: What was half written and where the reader had got to.
        //
        // A panel takes the composer's place while it is up, so what was
        // being written disappears behind it and the feed is relaid out
        // underneath. Both are set up here — a sentence nobody sent, and a
        // transcript scrolled back off its newest row — and read back after
        // every answer, because answering an agent must never cost a reader
        // either of them.
        var telling: [Any] = Self.reading.map { line in
            ["Markdown": ["text": "\(line)\n\n" + Self.saidUnder]]
        }
        telling.append("EndTurn")
        try control.ask(["AgentPlay": ["agent": "mind-the-gap", "steps": telling]])
        let newest = try XCTUnwrap(Self.reading.last)
        XCTAssertTrue(waitUntil { self.onScreen(app, newest) != nil },
                      "the run this reader is part way through never arrived: "
                      + "\(self.transcriptRows(app))")

        let writing = try XCTUnwrap(
            app.textViews.matching(identifier: "composer.field")
                .allElementsBoundByIndex.first { $0.isHittable },
            "the conversation offered nowhere to write")
        writing.tap()
        writing.typeText(Self.halfWritten)
        // The keys are put down the way the app puts them down — by reaching
        // for something else, here the fleet over the conversation — because a
        // keyboard standing over the feed in one reading and gone in the next
        // would move every row between them for a reason that has nothing to
        // do with any answer. The conversation underneath is not torn down, so
        // coming back out of the drawer comes back to this one.
        press(app, "conversation.drawer")
        waitFor(app, "drawer.row.\(runner.agent)", "the drawer did not list this agent")
        press(app, "drawer.row.\(runner.agent)")
        waitForNo(app, "drawer.row.\(runner.agent)", "picking this agent left the drawer out")
        XCTAssertEqual(said(try declared(runner), "composer.field")?.value, Self.halfWritten,
                       "what was being written did not survive the fleet being opened over it")

        // Scrolled back until the newest row is off the screen, which is what
        // reading something that happened earlier means.
        var anchor = ""
        XCTAssertTrue(
            waitUntil {
                if self.onScreen(app, newest) == nil,
                   let held = Self.reading.first(where: { self.onScreen(app, $0) != nil }) {
                    anchor = held
                    return true
                }
                app.swipeDown(velocity: .slow)
                return false
            },
            "scrolling back from the newest row never settled on an earlier one")
        let before = try XCTUnwrap(onScreen(app, anchor), "the row being read left the screen")
        record["place"] = ["reading": anchor, "draft": Self.halfWritten,
                           "before": Int(before.minY.rounded())]
        photograph(app, "ask-place-before")

        // MARK: A permission, refused.
        //
        // Every permission here offers one standing grant, because Claude
        // generates its menu from the host's suggestions and this build
        // answers only the shape it has been checked against: a permission
        // carrying none is drawn as unanswerable on purpose, which is a
        // different claim and belongs in a different test.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap", "ask": ["Permission": [
            "tool": "Bash", "invocation": ["tool": "bash", "command": "rm -rf /work/scratch"],
            "scoped_directories": ["/work/scratch"],
        ]]]])
        waitFor(app, "ask.allow", "the permission never reached the phone")
        let asking = try declared(runner)
        record["permission"] = [
            "head": said(asking, "ask.head")?.label ?? "",
            "subject": said(asking, "ask.subject")?.value ?? "",
        ]
        photograph(app, "ask-permission")
        answer(app, "ask.deny", "denying the permission left the panel up")
        let afterDenying = try placeKept(
            app, runner, reading: anchor, at: before, newest: newest,
            after: "denying the permission")
        var kept = record["place"] as? [String: Any] ?? [:]
        kept["afterDenying"] = Int(afterDenying.minY.rounded())
        record["place"] = kept
        photograph(app, "ask-place-after")

        // MARK: A permission, allowed.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap", "ask": ["Permission": [
            "tool": "Bash", "invocation": ["tool": "bash", "command": "cargo test -p parser"],
            "scoped_directories": ["/work/parser"],
        ]]]])
        waitFor(app, "ask.allow", "the second permission never reached the phone")
        answer(app, "ask.allow", "allowing the permission left the panel up")

        // MARK: The standing grant the host itself offered.
        //
        // What the row says and what pressing it sends are the same fact, so
        // the name is read off the screen and handed to the journey, which
        // checks the host was told about that directory and no other.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap", "ask": ["Permission": [
            "tool": "Read", "invocation": ["tool": "read", "file_path": "/work/api/schema.rs"],
            "scoped_directories": ["/work/api"],
        ]]]])
        waitFor(app, "ask.scope", "the host's standing grant was not offered")
        let granting = said(try declared(runner), "ask.scope")
        record["scope"] = [
            "title": granting?.label ?? "", "directory": granting?.value ?? "",
        ]
        answer(app, "ask.scope", "taking the standing grant left the panel up")

        // MARK: The agent's own question, with the agent's own answers.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap", "ask": ["Question": [
            "questions": [[
                "question": "Which parser should I fix first?",
                "header": "Where to start",
                "options": [
                    ["label": "The tokenizer", "description": "Where the newline is dropped"],
                    ["label": "The round trip", "description": "Where the test passes wrongly"],
                    ["label": "Neither yet", "description": "Read both first"],
                ],
                "multiSelect": false,
            ]],
        ]]]])
        waitFor(app, "ask.option.1", "the question never reached the phone")
        record["question"] = try declared(runner)
            .filter { $0.identifier.hasPrefix("ask.option.") }.map { $0.label }
        photograph(app, "ask-question")
        answer(app, "ask.option.1", "answering the question left the panel up")

        // MARK: A plan, read and approved.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap",
                                           "ask": ["Plan": ["markdown": Self.planToJudge]]]])
        waitFor(app, "ask.approve", "the plan never reached the phone")
        XCTAssertEqual(said(try declared(runner), "ask.plan")?.value, "folded",
                       "a plan arrived already opened out over the transcript")
        press(app, "ask.plan.more")
        XCTAssertEqual(try waitForValue(runner, "ask.plan", "open"), "open",
                       "the grabber did not open the rest of the plan")
        photograph(app, "ask-plan")
        answer(app, "ask.approve", "approving the plan left the panel up")

        // MARK: A plan, sent back with what should change.
        //
        // The layer will not take a plan back without a reason, so the panel
        // asks for one rather than sending an empty answer the host refuses.
        try control.ask(["AgentRaiseAsk": ["agent": "mind-the-gap",
                                           "ask": ["Plan": ["markdown": Self.planToSendBack]]]])
        waitFor(app, "ask.sendback", "the second plan never reached the phone")
        press(app, "ask.sendback")
        waitFor(app, "ask.feedback", "Send Back did not ask what should change")
        let feedback = app.descendants(matching: .any)
            .matching(identifier: "ask.feedback").allElementsBoundByIndex
            .first { $0.isHittable }
        XCTAssertNotNil(feedback, "the sheet offered nowhere to write")
        feedback?.tap()
        feedback?.typeText(Self.sentBack)
        press(app, "ask.feedback.send")
        waitForNo(app, "ask.sendback", "sending the plan back left the panel up")

        // MARK: The plan that was approved, reopened.
        //
        // Judging a plan finishes the tool the agent offered it as, so the
        // decision lands in the feed as one line. It is history by now — two
        // asks have been answered since — and reopening it has to show the
        // document that was judged rather than whatever the plan file says
        // today. The first decision in the feed is the approval; the one
        // under it is the plan that was sent back.
        XCTAssertTrue(waitUntil { self.transcriptRows(app).contains("transcript.plan") },
                      "a judged plan never arrived in the transcript: "
                      + "\(self.transcriptRows(app))")
        reveal(app, "transcript.plan")
        let judged = said(try declared(runner), "transcript.plan")
        record["verdict"] = judged?.label ?? ""
        XCTAssertEqual(judged?.value, "folded",
                       "a decision in the feed arrived with its document already open")
        press(app, "transcript.plan")
        XCTAssertEqual(try waitForValue(runner, "transcript.plan", "open"), "open",
                       "pressing the decision did not open the plan it was about")
        let reopened = app.staticTexts.allElementsBoundByIndex.map { $0.label }
            .contains { $0.contains("Move the token table into its own module") }
        XCTAssertTrue(reopened, "reopening the decision did not show the plan that was judged")
        record["reopened"] = reopened
        photograph(app, "ask-plan-reopened")
        press(app, "transcript.plan")

        // MARK: A child with an ask of its own, answered where it belongs.
        try control.ask(["AgentSpawnChild": ["agent": "mind-the-gap", "child": "spec-fixer"]])
        try control.ask(["AgentRaiseAsk": ["agent": "spec-fixer", "ask": ["Permission": [
            "tool": "Edit", "invocation": ["tool": "edit", "file_path": "/work/spec/wire.md"],
            "scoped_directories": ["/work/spec"],
        ]]]])
        var child: Said?
        for _ in 0..<20 where child == nil {
            child = try declared(runner).first {
                $0.identifier.hasPrefix("conversation.child.")
                    && $0.label.hasPrefix("spec-fixer")
            }
            if child == nil { RunLoop.current.run(until: Date().addingTimeInterval(0.5)) }
        }
        let childChip = try XCTUnwrap(
            child,
            "the child never appeared beside its parent: "
            + "\(self.identifiers(app, startingWith: "conversation.child."))")
        record["child"] = ["chip": childChip.identifier, "says": childChip.label]
        photograph(app, "ask-children")
        press(app, childChip.identifier)
        // The child's own conversation, and the child's own answer.
        // The child is pushed over its parent and the parent is not torn down,
        // so both conversations declare themselves and the one that is not the
        // parent is the one that was opened. Which of them a finger reaches is
        // settled by the answer below: the host says whose ask it received.
        var opened = ""
        XCTAssertTrue(
            waitUntil {
                opened = ((try? self.declared(runner)) ?? []).first {
                    $0.identifier == "conversation" && !$0.value.isEmpty
                        && $0.value != runner.agent
                }?.value ?? ""
                return !opened.isEmpty
            },
            "pressing the child did not open the child's conversation; the screen shows "
            + "\(((try? self.declared(runner)) ?? []).filter { $0.identifier == "conversation" }.map(\.value))")
        record["childConversation"] = opened
        waitFor(app, "ask.allow", "the child's ask is not answerable in the child")
        answer(app, "ask.allow", "answering the child left its panel up")

        // Back to the parent the way the app offers: the fleet over the
        // conversation, which is how you go sideways from any of them.
        press(app, "conversation.drawer")
        waitFor(app, "drawer.row.\(runner.agent)", "the drawer did not list the parent")
        press(app, "drawer.row.\(runner.agent)")
        XCTAssertEqual(try waitForValue(runner, "conversation", runner.agent), runner.agent,
                       "coming back did not come back to the parent")
        XCTAssertEqual(said(try declared(runner), "composer.field")?.value, Self.halfWritten,
                       "answering the child's ask lost what was being written to its parent")

        // MARK: A child that runs inside the session, which is nowhere to go.
        try control.ask(["AgentPlay": ["agent": "mind-the-gap",
                                       "steps": [["ChildStarted": ["name": "scout"]]]]])
        // A subagent is named by what it is as well as what it is called —
        // the provider reports both and two of the same name doing different
        // work are two rows — so the chip is found by what it starts with
        // rather than by a name it never carries alone.
        var subagent = ""
        XCTAssertTrue(
            waitUntil {
                subagent = self.identifiers(app, startingWith: "conversation.child.scout").first
                    ?? ""
                return !subagent.isEmpty
            },
            "the provider's own subagent was never listed: "
            + "\(self.identifiers(app, startingWith: "conversation.child."))")
        press(app, subagent)
        waitFor(app, "conversation.child.unopenable",
                "a child with nowhere to go said nothing when it was pressed")
        record["unopenable"] = said(try declared(runner), "conversation.child.unopenable")?.value
            ?? ""

        // MARK: A turn that finished, with changes to read.
        //
        // The changes are asked for through the door because the composer and
        // the overflow are later work; the host computes the patch and sends
        // it back as an ordinary event, so the panel gets it the way every
        // other fact reaches this screen.
        // Reading history again, so that what the finished turn's panel is
        // answered over is a reader who is not at the tail.
        var stillReading = ""
        XCTAssertTrue(
            waitUntil {
                if self.onScreen(app, newest) == nil,
                   let held = Self.reading.first(where: { self.onScreen(app, $0) != nil }) {
                    stillReading = held
                    return true
                }
                app.swipeDown(velocity: .slow)
                return false
            },
            "scrolling back from the newest row never settled on an earlier one")
        let beforeFinishing = try XCTUnwrap(
            onScreen(app, stillReading), "the row being read left the screen")

        try door(runner, .init(kind: "requestChanges", agent: runner.agent, base: ""))
        try control.ask(["AgentPlay": ["agent": "mind-the-gap", "steps": [
            ["Prompt": ["text": "Tidy the parser."]],
            ["Markdown": ["text": "Done: the tokenizer keeps the trailing newline now."]],
            "EndTurn",
        ]]])
        waitFor(app, "conversation.finished", "a finished turn with changes offered no review")
        record["finished"] = said(try declared(runner), "conversation.finished")?.value ?? ""
        photograph(app, "ask-finished")

        // Later says nothing to the host and takes the panel away for this
        // visit; the chip in the chrome is still the way to the changes.
        press(app, "ask.later")
        waitForNo(app, "conversation.finished", "Later left the panel where it was")
        let afterLater = try placeKept(
            app, runner, reading: stillReading, at: beforeFinishing, newest: newest,
            after: "deferring the finished turn")
        kept = record["place"] as? [String: Any] ?? [:]
        kept["afterChild"] = ["reading": stillReading,
                              "before": Int(beforeFinishing.minY.rounded()),
                              "after": Int(afterLater.minY.rounded())]
        record["place"] = kept
        photograph(app, "ask-place-after-child")
        XCTAssertTrue(element(app, "conversation.changes").exists,
                      "deferring the review took the way to the changes away too")

        // Coming back offers again, because nothing was decided.
        pressTab(app, "Agents")
        waitFor(app, "home", "leaving the conversation did not return to the home")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation.finished",
                "coming back to a finished turn did not offer the review again")
        press(app, "ask.review")
        waitFor(app, "review", "Review Changes did not lead to the changes")
        record["review"] = said(try declared(runner), "review")?.value ?? ""
        photograph(app, "ask-review")

        try write("asks.json")
    }

    private static let sentBack = "Keep the round-trip test; rewrite it instead."

    /// A sentence somebody started writing and never sent. It stays in the
    /// box for the whole journey: nothing here presses Send, so every reading
    /// of the field afterwards is a reading of the same unsent draft.
    private static let halfWritten = "Before you go further, check the span on"

    /// A run of the agent's own messages, each taking a line or two, so the
    /// feed is taller than the phone and there is somewhere earlier to be.
    private static let reading = (1...12).map { "Reading mark \(String(format: "%02d", $0))" }

    private static let saidUnder =
        "so the tokenizer keeps the trailing newline and the round trip stops "
        + "passing for the wrong reason."

    /// What answering must leave exactly as it found it: the sentence nobody
    /// sent, and the row the reader was on.
    ///
    /// The draft is read from the screen through the app's own door rather
    /// than from the field, because a panel that has just been answered has
    /// only now given the box back. Where the reader is, is read as a
    /// coordinate: the row being read has to still be drawn where it was, and
    /// the newest row — the one a feed snaps to when it loses somebody's
    /// place — has to still be off the screen.
    @discardableResult
    private func placeKept(
        _ app: XCUIApplication, _ runner: Runner, reading anchor: String, at before: CGRect,
        newest: String, after answering: String
    ) throws -> CGRect {
        XCTAssertEqual(said(try declared(runner), "composer.field")?.value, Self.halfWritten,
                       "\(answering) lost what was being written")
        let now = try XCTUnwrap(onScreen(app, anchor),
                                "\(answering) took the reader off the row they were reading")
        XCTAssertEqual(now.minY, before.minY, accuracy: 12,
                       "\(answering) moved the feed under the reader: "
                       + "\(anchor) was drawn at \(before.minY) and is now at \(now.minY)")
        XCTAssertNil(onScreen(app, newest),
                     "\(answering) threw the reader forward to the newest row")
        return now
    }

    /// Where the row saying this sits on the screen, or nothing where the
    /// screen is not showing it.
    ///
    /// Found by what it says: prose carries no name of its own, and the one
    /// name every prose row shares cannot tell one from another. A row that
    /// has scrolled off is either absent from the tree the system builds or
    /// lies outside the window, and both mean nobody is reading it.
    private func onScreen(_ app: XCUIApplication, _ saying: String) -> CGRect? {
        let matching = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", saying))
            .allElementsBoundByIndex
        guard let row = matching.first(where: { $0.exists && $0.frame.height > 0 }) else {
            return nil
        }
        return row.frame.intersects(app.frame) ? row.frame : nil
    }
}
