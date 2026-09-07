import Foundation
import XCTest

/// Writing to an agent on a phone, against machines the runner is really
/// running.
///
/// Everything the composer is for is here: a paragraph typed over two lines,
/// every kind of attachment standing in the sentence as a token, a message
/// sent and answered for by the host, one message held while a turn runs and
/// replaced and taken back, and the two controls that must never be one — the
/// cross that empties the field and the button that stops the turn.
///
/// Two agents, because no one layer offers all of it. A Claude session driven
/// over a terminal takes messages, holds one while it works and reports every
/// input it received, which is what the sending half is proved against; it
/// refuses model and effort outright and takes no commands. A Codex session
/// offers all three, so the command token, the model and the effort are
/// exercised there. What the host received is the journey's to check
/// afterwards.
final class WritingTests: JourneyCase {
    /// The agent the sending half is written to, by the name the runner gave
    /// it: the control channel names agents rather than identifying them.
    private static let host = "talk-me-through-it"

    private static let firstLine = "The parser drops the trailing newline."
    private static let secondLine = "Start with the tokenizer, then the round trip."
    private static let pasted = (1...14).map { "  log line \($0)" }.joined(separator: "\n")
    private static let sent = "Read the parser and tell me where the newline goes."
    private static let queued = "And then look at the wire format."
    private static let replacement = "Actually, look at the wire format first."
    private static let delivered = "Wire format after the parser, please."
    private static let renamed = "reads-the-parser"
    private static let arguments = " the parser before the wire format"
    private static let remark = "This is the line that drops the newline."

    /// A one-pixel PNG. The picker is the system's and bytes are all this app
    /// ever sees of it; these are the smallest honest ones.
    private static let pixel =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC"

    func testAMessageIsWrittenSentHeldAndTheAgentReconfigured() throws {
        let runner = try Runner()
        let control = try Lines(address: runner.control)
        let codex = try XCTUnwrap(
            ProcessInfo.processInfo.environment["AMUX_CODEX_AGENT"],
            "the journey did not pass AMUX_CODEX_AGENT")
        let app = launch(runner)

        // MARK: The conversation, opened.
        waitFor(app, "home.row.\(runner.agent)",
                "the machine never answered for \(runner.agent)")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation", "opening the row did not lead to a conversation")
        waitFor(app, "composer", "the conversation offered nowhere to write")

        // MARK: A paragraph over two lines.
        //
        // Return puts a newline in, because sending is the button and only the
        // button: a keyboard whose return key sends is one that cannot write a
        // second paragraph, and a phone has no modifier to escape it with.
        writable(app).tap()
        writable(app).typeText(Self.firstLine + "\n" + Self.secondLine)
        record["typed"] = try waitForSpoken(
            runner, containing: Self.secondLine, "what was typed never reached the field")

        // MARK: Every kind of token, each at the caret.
        //
        // The photo library and the file browser are the system's own screens
        // and run outside this app; what they hand back — a kind, a name, a
        // type and the bytes — goes through the app's door into the same code
        // a real pick calls, so a token appears because a host stored the
        // bytes and not because a picker closed. The paste is the field's own:
        // the door puts the text on the clipboard and sends the field the same
        // message the system's Paste menu item sends.
        try door(runner, .init(kind: "paste", text: Self.pasted, identifier: "composer.field"))
        record["afterPaste"] = try waitForSpoken(
            runner, containing: "[Pasted text \u{00B7} 14 lines]",
            "a paste of fourteen lines did not become one token")
        try door(runner, .init(
            kind: "attach", agent: runner.agent, attachment: "image", name: "screenshot.png",
            mime: "image/png", base64: Self.pixel))
        record["afterPhoto"] = try waitForSpoken(
            runner, containing: "[screenshot.png]", "the stored photograph named nothing")

        // MARK: One backspace, and the whole token goes.
        //
        // A token put in from outside leaves the caret after it, so the key
        // that takes one letter of a word takes all of this one: the name is
        // fourteen characters on the screen and one character in the sentence.
        writable(app).typeText(XCUIKeyboardKey.delete.rawValue)
        let afterDelete = try waitForSpoken(
            runner, containing: Self.secondLine, "the field lost more than the photograph")
        XCTAssertFalse(afterDelete.contains("screenshot"),
                       "one press left part of the photograph behind: \(afterDelete)")
        XCTAssertTrue(afterDelete.contains("[Pasted text"),
                      "one press took the paste with it as well: \(afterDelete)")
        record["afterRemove"] = afterDelete

        try door(runner, .init(
            kind: "attach", agent: runner.agent, attachment: "file", name: "parser.rs",
            mime: "text/x-rust", base64: Self.pixel))
        record["afterFile"] = try waitForSpoken(
            runner, containing: "[parser.rs]", "the stored file named nothing")

        // MARK: The review, attached from the page that holds it.
        //
        // The machine is asked for what its working tree has changed, freezes
        // it and sends the patch. Reading and writing about a patch is proved
        // in its own journey; what is claimed here is that attaching comes
        // back to the sentence being written and stands in it as one more
        // token. One range is written about because a review with nothing
        // said about it is not offered for attaching, which is right.
        try door(runner, .init(kind: "requestChanges", agent: runner.agent, base: ""))
        waitFor(app, "conversation.changes", "the machine answered with no changes to review")
        press(app, "conversation.changes")
        waitFor(app, "review", "the changes chip did not lead to the patch")
        remark(app, Self.remark)
        press(app, "review.attach")
        waitFor(app, "conversation", "attaching the review did not come back to the conversation")
        record["afterReview"] = try waitForSpoken(
            runner, containing: "[Review", "the attached review is not in the message")
        photograph(app, "writing-tokens")

        // MARK: Nothing kept, and the field is empty again.
        //
        // Pressed through the app's own door rather than by coordinate. The
        // cross sits in the composer, and the composer sits under an open
        // keyboard: a tap aimed at where the tree says the control is lands on
        // a letter key and types it into the message being cleared. The door
        // does to it what VoiceOver does, which is the same press.
        try door(runner, .init(kind: "tap", identifier: "composer.clear"))
        try waitUntil(runner, "clearing left something in the field") {
            self.said($0, "composer")?.value.isEmpty == true
        }

        // MARK: One message sent, drawn at once and reconciled to one row.
        //
        // The row appears before the host has answered for it, and what proves
        // it was reconciled rather than duplicated is that the host says it
        // received the message and the feed still shows exactly one of it.
        writable(app).tap()
        writable(app).typeText(Self.sent)
        press(app, "composer.send")
        try waitUntil(runner, "sending put no message in the feed") {
            $0.contains { $0.identifier == "transcript.prompt" }
        }
        XCTAssertEqual(try spoken(runner), "", "sending left the message in the field")
        XCTAssertTrue(
            waitUntil { self.received(control, saying: Self.sent) },
            "the machine never reported the message it was sent")
        // One row, not two: the row drawn before the machine answered and the
        // one the machine sent back are the same message, and the feed has to
        // end up holding it once.
        try waitUntil(runner, "the message the machine confirmed is drawn more than once") {
            $0.filter { $0.identifier == "transcript.prompt" }.count == 1
        }
        record["sent"] = said(try declared(runner), "transcript.prompt")?.label ?? ""

        // MARK: A turn, so there is something to queue behind.
        try control.ask(["AgentPlay": ["agent": Self.host, "steps": [
            ["Prompt": ["text": "Reading the parser"]],
            ["Working": ["secs": 3]],
        ]]])
        try waitUntil(runner, "the composer never noticed the turn") {
            self.said($0, "composer")?.label == "Queue a message"
        }
        record["working"] = said(try declared(runner), "composer.working")?.label ?? ""

        // MARK: One message held, replaced, and taken back.
        writable(app).tap()
        writable(app).typeText(Self.queued)
        press(app, "composer.send")
        waitFor(app, "facts.queued", "the message was not held above the composer")
        try waitUntil(runner, "the strip is not showing the message that was queued") {
            self.said($0, "facts.queued")?.label == Self.queued
        }
        record["queued"] = said(try declared(runner), "facts.queued")?.label ?? ""
        photograph(app, "writing-queued")

        writable(app).tap()
        writable(app).typeText(Self.replacement)
        press(app, "composer.send")
        try waitUntil(runner, "writing a second message did not replace the one being held") {
            self.said($0, "facts.queued")?.label == Self.replacement
        }
        record["replaced"] = said(try declared(runner), "facts.queued")?.label ?? ""

        // Tapping the row takes it back: the words land in the field as an
        // ordinary unsent message and the machine stops holding anything.
        press(app, "facts.queued")
        waitForNo(app, "facts.queued", "unqueueing left the message held")
        record["unqueued"] = try waitForSpoken(
            runner, containing: Self.replacement, "unqueueing did not put the words in the field")

        // MARK: Clearing is not stopping.
        //
        // The cross empties the field and the turn goes on, which is what the
        // placeholder still says afterwards.
        try door(runner, .init(kind: "tap", identifier: "composer.clear"))
        XCTAssertEqual(try spoken(runner), "", "the cross did not empty the field")
        XCTAssertEqual(said(try declared(runner), "composer")?.label, "Queue a message",
                       "emptying the field stopped the turn as well")

        // MARK: One held message that will be delivered.
        writable(app).tap()
        writable(app).typeText(Self.delivered)
        press(app, "composer.send")
        waitFor(app, "facts.queued", "the message to be delivered was not held")

        // MARK: Stopping is not clearing.
        //
        // Stop is offered only where there is nothing left to send, so no one
        // press can ever do both. What is held survives the turn being stopped
        // and is still there to be delivered.
        press(app, "composer.interrupt")
        XCTAssertTrue(
            waitUntil { self.element(app, "facts.queued").exists },
            "stopping the turn threw the held message away")
        record["interruptedNotCleared"] = said(try declared(runner), "facts.queued")?.label ?? ""

        // MARK: The turn ends, and the held message goes.
        try control.ask(["AgentEndTurn": ["agent": Self.host]])
        waitForNo(app, "facts.queued", "the turn ended and the held message stayed held")
        XCTAssertTrue(
            waitUntil { self.received(control, saying: Self.delivered) },
            "the turn ended and the machine was never given the message it was holding")

        // MARK: The address, on the clipboard.
        press(app, "conversation.overflow")
        waitFor(app, "overflow", "the ellipsis opened nothing")
        record["address"] = said(try declared(runner), "overflow.copy-address")?.value ?? ""
        photograph(app, "writing-overflow")
        press(app, "overflow.copy-address")
        waitForNo(app, "overflow", "copying the address left the menu open")

        // MARK: A rename, abandoned and then made.
        press(app, "conversation.overflow")
        press(app, "overflow.rename")
        waitFor(app, "rename", "the rename row opened no card")
        press(app, "rename.cancel")
        waitForNo(app, "rename", "cancelling the rename left the card open")
        XCTAssertEqual(said(try declared(runner), "conversation.subject")?.value, Self.host,
                       "a cancelled rename renamed the agent anyway")

        press(app, "conversation.overflow")
        press(app, "overflow.rename")
        waitFor(app, "rename.field", "the rename card offered nowhere to write")
        let naming = try XCTUnwrap(
            app.textFields.matching(identifier: "rename.field")
                .allElementsBoundByIndex.first { $0.isHittable },
            "the rename field could not be reached")
        naming.tap()
        naming.typeText(String(
            repeating: XCUIKeyboardKey.delete.rawValue, count: Self.host.count))
        naming.typeText(Self.renamed)
        photograph(app, "writing-rename")
        press(app, "rename.confirm")
        // The machine keeps the name: what the pill says afterwards is what it
        // answered with, and never what was typed here.
        try waitUntil(runner, "the machine did not rename the agent") {
            self.said($0, "conversation.subject")?.value == Self.renamed
        }
        record["renamed"] = said(try declared(runner), "conversation.subject")?.value ?? ""

        // MARK: A deletion, asked for and taken back.
        press(app, "conversation.overflow")
        press(app, "overflow.delete-agent")
        waitFor(app, "agent-delete", "the delete row opened no card")
        photograph(app, "writing-delete")
        press(app, "agent-delete.cancel")
        waitForNo(app, "agent-delete", "cancelling the deletion left the card open")
        waitFor(app, "composer", "cancelling the deletion did not give the composer back")
        XCTAssertEqual(said(try declared(runner), "conversation")?.value, runner.agent,
                       "a cancelled deletion left the conversation anyway")

        // MARK: The Codex agent, which offers what a terminal session cannot.
        press(app, "conversation.drawer")
        waitFor(app, "drawer.row.\(codex)", "the drawer does not hold the other agent")
        press(app, "drawer.row.\(codex)")
        try waitUntil(runner, "the drawer did not open the other agent's conversation") {
            self.said($0, "conversation")?.value == codex
        }
        waitFor(app, "composer", "the Codex conversation offered nowhere to write")

        // MARK: A command, raised by a slash and sent as a token.
        //
        // The rows are the session's own commands, and picking one commits a
        // token rather than the letters that were typed: what follows it is
        // the command's arguments and travels beside it.
        writable(app).tap()
        writable(app).typeText("/pl")
        waitFor(app, "slash.plan", "typing a slash raised none of the session's commands")
        record["commandOffered"] = said(try declared(runner), "slash.plan")?.value ?? ""
        photograph(app, "writing-commands")
        press(app, "slash.plan")
        writable(app).typeText(Self.arguments)
        record["command"] = try waitForSpoken(
            runner, containing: "[plan]", "picking the command left no token in the field")
        press(app, "composer.send")
        XCTAssertTrue(
            waitUntil { self.transcriptRows(app).contains("transcript.prose") },
            "the command was sent and the session never answered; the feed shows "
            + "\(transcriptRows(app))")

        // MARK: The model and the effort, changed and read back off the host.
        //
        // The chip is relabelled from the facts the session publishes once it
        // has taken the change, so what is on the screen afterwards is the
        // machine's account of how this agent now runs. Choosing a model
        // chooses that model's own default effort, which is the machine's
        // arithmetic and not this phone's.
        record["modelBefore"] = said(try declared(runner), "composer.model")?.value ?? ""
        press(app, "composer.model")
        waitFor(app, "settings", "the chip opened no settings")
        photograph(app, "writing-settings")
        press(app, "settings.model.model-b")
        try waitUntil(runner, "the machine did not report the new model") {
            self.said($0, "composer.model")?.value == "Model B \u{00B7} medium"
        }
        record["model"] = said(try declared(runner), "composer.model")?.value ?? ""
        press(app, "settings.effort.high")
        try waitUntil(runner, "the machine did not report the new effort") {
            self.said($0, "composer.model")?.value == "Model B \u{00B7} high"
        }
        record["effort"] = said(try declared(runner), "composer.model")?.value ?? ""

        // MARK: A sheet left without choosing anything.
        //
        // What the plus opens is about the message being written, and a press
        // on the conversation behind it puts the sheet away without deciding
        // anything — which is what the agent still runs under afterwards.
        press(app, "composer.attach")
        waitFor(app, "plus", "the plus opened nothing")
        press(app, "plus.permissions")
        waitFor(app, "permissions", "the permissions row opened no card")
        record["abandoned"] = said(try declared(runner), "permissions")?.value ?? ""
        dismiss(app)
        waitForNo(app, "permissions", "pressing the conversation left the card open")
        XCTAssertEqual(said(try declared(runner), "composer.model")?.value,
                       "Model B \u{00B7} high", "leaving a sheet changed how the agent runs")

        // MARK: A deletion, confirmed.
        //
        // Nothing is left on the press: the conversation stays until the
        // machine says the agent is gone, and leaving is driven by that.
        press(app, "conversation.overflow")
        press(app, "overflow.delete-agent")
        waitFor(app, "agent-delete", "the delete row opened no card the second time")
        press(app, "agent-delete.confirm")
        try waitUntil(runner, "the machine confirmed the deletion and the conversation stayed") {
            self.said($0, "conversation")?.value != codex
        }
        record["deleted"] = codex

        try write("writing.json")
    }

    /// Takes hold of one line of the patch and says something about it.
    ///
    /// Held and dragged rather than tapped: a range needs two ends, and a
    /// plain drag on this page scrolls it. The row is found by what it says —
    /// a diff row is a line of a file and carries no name of its own.
    private func remark(_ app: XCUIApplication, _ text: String) {
        let rows = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@", "Removed line "))
            .allElementsBoundByIndex.filter { $0.isHittable }
        guard rows.count > 1 else {
            return XCTFail("the patch shows \(rows.count) removed lines to write about")
        }
        rows[0].press(forDuration: 0.6, thenDragTo: rows[1])
        waitFor(app, "review.commentSheet", "holding a range of lines opened no sheet")
        let writing = app.descendants(matching: .any)
            .matching(identifier: "review.commentField")
            .allElementsBoundByIndex.first { $0.isHittable }
        guard let writing else { return XCTFail("the sheet offered nowhere to write") }
        writing.tap()
        writing.typeText(text)
        press(app, "review.addComment")
        waitForNo(app, "review.commentSheet", "adding the remark left the sheet open")
    }

    /// The field a message is written in, which is the one text view on the
    /// conversation screen.
    ///
    /// Asked for afresh every time rather than held: a page pushed over the
    /// conversation and popped again builds a new one, and a query resolved
    /// before that taps nothing afterwards.
    private func writable(_ app: XCUIApplication) -> XCUIElement {
        let named = app.textViews.matching(identifier: "composer.field")
            .allElementsBoundByIndex.first { $0.isHittable }
        return named ?? app.descendants(matching: .any)
            .matching(identifier: "composer.field").firstMatch
    }

    /// Presses the dimmed conversation behind whatever is open over it, which
    /// is how everything here closes. It carries no name of its own — it is
    /// the ground, not a control — so it is found by what VoiceOver calls it.
    private func dismiss(_ app: XCUIApplication) {
        let closing = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "Close"))
            .allElementsBoundByIndex.first { $0.isHittable }
        guard let closing else { return XCTFail("nothing on screen closes what is open") }
        closing.tap()
    }

    /// Waits until the screen says what it is expected to say.
    ///
    /// Read without letting the screen settle: settling renders the whole
    /// window until two frames agree, and a caret blinking in the field means
    /// they never do, so a poll that settled would spend the whole wait on
    /// every turn of the loop.
    private func waitUntil(
        _ runner: Runner, _ complaint: String, _ condition: ([Said]) -> Bool
    ) throws {
        var seen: [Said] = []
        for _ in 0..<40 {
            seen = try declared(runner, settling: false)
            if condition(seen) { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        }
        XCTFail("\(complaint); the screen says "
                + "\(seen.map { "\($0.identifier)=\($0.label)/\($0.value)" })")
    }

    /// What the composer says it is holding, with each token read as the words
    /// on it. A chip is a picture; this is what VoiceOver speaks and the only
    /// thing a driver can read.
    private func spoken(_ runner: Runner) throws -> String {
        said(try declared(runner, settling: false), "composer")?.value ?? ""
    }

    /// Waits until the field says something, and says what it said instead
    /// when it never does.
    @discardableResult
    private func waitForSpoken(
        _ runner: Runner, containing needle: String, _ complaint: String
    ) throws -> String {
        var seen = ""
        for _ in 0..<40 {
            seen = try spoken(runner)
            if seen.contains(needle) { return seen }
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
        XCTFail("\(complaint); the field says \(seen.isEmpty ? "nothing" : seen)")
        return seen
    }

    /// Every message the machine says it has been given, in its own words.
    private func received(_ control: Lines) throws -> [String] {
        let answer = try control.ask(["AgentObserve": ["agent": Self.host]])
        let observed = answer["observed"] as? [[String: Any]] ?? []
        return observed.compactMap { $0["text"] as? String }
    }

    /// Whether the machine says it has been given a message saying this.
    private func received(_ control: Lines, saying words: String) -> Bool {
        ((try? received(control)) ?? []).contains { $0.contains(words) }
    }
}
