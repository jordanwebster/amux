import Foundation
import XCTest

/// One conversation, against a machine the runner is really running.
///
/// This is a UI test rather than a door conversation because everything it
/// claims is about pressing things: opening an agent, unfolding a run of reads,
/// reaching the changes. The app is launched already told what to connect to
/// and which machine to trust, so every row on screen arrived over a real relay
/// from a real host, and nothing here fills a store.
///
/// Three sockets, all on the loopback the simulator shares with the Mac:
/// XCUITest for what a finger does, the runner's control channel for what the
/// agent on the other side does, and the app's own door for the one thing a
/// finger cannot yet do — trying to send a message, because the composer is not
/// built. The journey that starts this test passes their addresses in.
final class ConversationTests: XCTestCase {
    private let waiting: TimeInterval = 60

    /// Where the photographs and the record are left for the journey to
    /// collect out of this process's container.
    private static func inContainer(_ name: String) -> URL {
        URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(name)
    }

    /// What the journey told this test to talk to.
    private struct Runner {
        let relay: String
        let token: String
        let user: String
        let pairing: String
        let control: String
        let doorPort: String
        let agent: String
        let ended: String
        let host: String
        let report: String

        init() throws {
            let environment = ProcessInfo.processInfo.environment
            func required(_ name: String) throws -> String {
                let value = environment[name]
                return try XCTUnwrap(value, "the journey did not pass \(name)")
            }
            relay = try required("AMUX_RELAY")
            token = try required("AMUX_TOKEN")
            user = try required("AMUX_USER")
            pairing = try required("AMUX_PAIR")
            control = try required("AMUX_CONTROL")
            doorPort = try required("AMUX_DOOR_PORT")
            agent = try required("AMUX_AGENT")
            ended = try required("AMUX_ENDED_AGENT")
            host = try required("AMUX_HOST")
            report = try required("AMUX_REPORT")
        }
    }

    /// What this test found, written out for the journey to assert on and for
    /// a person to read after a failure.
    private var record: [String: Any] = [:]

    func testAConversationStreamsAndRefusesWhatItCannotSend() throws {
        let runner = try Runner()
        let control = try Lines(address: runner.control)
        let app = XCUIApplication()
        app.launchArguments = [
            "-amux-door-port", runner.doorPort,
            "-amux-relay", runner.relay,
            "-amux-token", runner.token,
            "-amux-user", runner.user,
            "-amux-pair", runner.pairing,
        ]
        app.launch()

        // MARK: The fleet a paired phone is given.
        let row = element(app, "home.row.\(runner.agent)")
        XCTAssertTrue(row.waitForExistence(timeout: waiting),
                      "the machine never answered for \(runner.agent); the home shows "
                      + "\(identifiers(app, startingWith: "home.row."))")
        record["fleet"] = identifiers(app, startingWith: "home.row.")

        // Half the turn before anybody is watching, so that opening the
        // conversation has something to catch up on. A layer catching up is
        // one of the three states that will not take a message, and it is the
        // only one that cannot be arranged after the fact.
        // Two things make catching up something a person could watch, and a
        // driver reach: a turn long enough to still be arriving, and a relay
        // slow enough that arriving takes time. Both are put back afterwards.
        try control.ask(["Latency": ["millis": 200]])
        let playing = try Lines(address: runner.control)
        try playing.say(["AgentPlay": ["agent": "carry-on", "steps": Self.beforeAnybodyWatches]])

        // MARK: Opening it, while that is still going on.
        press(app, "home.row.\(runner.agent)")
        let conversation = element(app, "conversation")
        XCTAssertTrue(conversation.waitForExistence(timeout: waiting),
                      "opening the row did not lead to a conversation")

        // A message tried in the moment a conversation opens, which is the
        // moment the layer is catching up on what it missed.
        //
        // Opened through the door rather than by a tap, and on the agent
        // nobody has looked at yet: a tap takes long enough that the layer has
        // already caught up by the time the next thing can be asked, and the
        // window this is about is milliseconds wide.
        _ = try door(runner, .init(kind: "watch", agent: runner.ended))
        let whileCatchingUp = try door(runner, .init(
            kind: "send", agent: runner.ended, text: "sent while catching up"))
        record["whileCatchingUp"] = whileCatchingUp

        // The rest of the turn, into a conversation somebody has open: these
        // rows arrive over the relay while the screen is on show, which is
        // what a person watching an agent work sees.
        _ = try playing.awaitAnswer()
        try control.ask(["Latency": ["millis": 0]])
        try control.ask(["AgentPlay": ["agent": "carry-on", "steps": Self.whileWatching]])

        // MARK: Every kind of row, from the host.
        //
        // Read by scrolling, because the system builds a tree for what is on
        // screen: a feed longer than the display is never all in it at once,
        // and a claim about every kind of row has to walk the whole thing the
        // way a person would.
        _ = try waitForRows(app)
        photograph(app, "conversation-rows")
        let transcript = readWholeFeed(app)
        record["rows"] = transcript.sorted()
        // The tree beside the photograph. A row that did not arrive, or that
        // arrived as something else, is unreadable from a screenshot and from
        // a list of names; what the system built is the only thing that says
        // which.
        try? app.debugDescription.write(
            to: Self.inContainer("conversation-tree.txt"), atomically: true, encoding: .utf8)
        for kind in Self.everyRowKind {
            XCTAssertTrue(transcript.contains(kind),
                          "the transcript never drew \(kind); it drew \(transcript.sorted())")
        }
        // The end of the turn, photographed where it is. The refusals, the
        // failure, the interruption and the provider's error all sit below the
        // fold of a feed this long, so a photograph taken where the feed opens
        // shows none of them: this one is taken with them on screen, and says
        // which of them were.
        record["endOfTurn"] = toTheEnd(app)
        photograph(app, "conversation-row-kinds")
        photograph(app, "conversation-head")

        // MARK: A folded run, opened.
        //
        // Folded and open are told apart by what is written, not by a value on
        // the row: a name declared on a screen reaches the system's tree as an
        // identifier and nothing else, so a test that read a value would be
        // reading the empty string every time. A folded run says how many
        // times it looked and where it looked last; an open one lists each
        // look under it, and those lines are only there once it is open.
        reveal(app, "transcript.exploration")
        XCTAssertFalse(app.staticTexts["Searched"].exists,
                       "the run of reads listed what it did before it was opened")
        press(app, "transcript.exploration")
        XCTAssertTrue(app.staticTexts["Searched"].waitForExistence(timeout: waiting),
                      "the run of reads did not list what it did when it was pressed")
        record["fold"] = app.staticTexts.allElementsBoundByIndex
            .map { $0.label }.filter { $0 == "Read" || $0 == "Searched" }
        photograph(app, "conversation-unfolded")

        // MARK: The changes, asked for and opened.
        _ = try door(runner, .init(
            kind: "requestChanges", agent: runner.agent, base: "HEAD~1"))
        let chip = element(app, "conversation.changes")
        XCTAssertTrue(chip.waitForExistence(timeout: waiting),
                      "the host answered with no changes to review")
        record["changes"] = app.staticTexts.allElementsBoundByIndex
            .map { $0.label }.filter { $0.hasPrefix("+") || $0.hasPrefix("\u{2212}") }
        press(app, "conversation.changes")
        // Where the chip leads. The diff itself is a later screen; what is
        // claimed here is that a real diff, computed by the host that holds
        // the repository, put the chip on screen and that pressing it goes to
        // the changes rather than nowhere.
        XCTAssertTrue(element(app, "page.changes").waitForExistence(timeout: waiting),
                      "pressing the changes chip did not go to the changes")
        photograph(app, "conversation-changes")
        // The bar's own button and nothing else. A drag from the left edge is
        // the system's way back, but it is also the drawer's way out, and the
        // drawer wins: the fleet slides over the conversation and everything
        // asked about it afterwards is asked of a screen nobody is looking at.
        let back = app.navigationBars.buttons.element(boundBy: 0)
        XCTAssertTrue(back.waitForExistence(timeout: waiting),
                      "the changes have no way back")
        back.tap()
        XCTAssertTrue(conversation.waitForExistence(timeout: waiting),
                      "coming back from the changes did not come back to the conversation")
        XCTAssertTrue(identifiers(app, startingWith: "drawer.row.").isEmpty,
                      "coming back from the changes left the fleet over the conversation")

        // MARK: One message that goes, and one tried before it is answered.
        //
        // The wait is for the layer, not for the connection: a machine that
        // has just come back has not finished saying so, and a send in that
        // gap would be refused for a reason nobody arranged.
        _ = try door(runner, .init(kind: "awaitSendable", agent: runner.agent, seconds: 90))
        let sent = try door(runner, .init(kind: "send", agent: runner.agent, text: Self.delivered))
        XCTAssertEqual(sent["delivered"] as? Bool, true,
                       "a conversation with a reachable machine refused a message: \(sent)")
        let whileInFlight = try door(runner, .init(
            kind: "send", agent: runner.agent, text: "sent while the last is unanswered"))
        record["delivered"] = sent
        record["whileInFlight"] = whileInFlight
        photograph(app, "conversation-send-refused")

        // MARK: Reading a turn while it is still arriving.
        //
        // A transcript that grows under a thumb is the one thing about this
        // screen a still photograph cannot show, so this stretch is filmed:
        // the test says when it begins and when it is over by writing a word
        // where the Mac can read it, and the Mac records the simulator for
        // exactly that long.
        //
        // The turn is played a batch at a time, in between the swipes, because
        // a whole turn asked for at once is taken by the host in one go and
        // lands on the phone before a thumb has moved. Split up, arriving and
        // scrolling are genuinely happening at the same time, and the claim is
        // measured rather than assumed: each time the screen is read, the turn
        // has got further into itself than the time before.
        marker("begin")
        let arriving = try Lines(address: runner.control)
        var swipes = 0
        var seenLines = Set<Int>()
        var furthest: [Int] = []
        for batch in 0..<Self.streamedBatches {
            // One batch at a time, each acknowledged before the next is asked
            // for, so the turn genuinely lands in pieces spread across the
            // scrolling rather than in one burst before it.
            try arriving.ask(["AgentPlay": ["agent": "carry-on",
                                            "steps": Self.streamedBatch(batch)]])
            // Towards the end, which is where the new rows are landing: a
            // conversation does not follow a turn on its own, so keeping up
            // with one is something a reader does with a thumb.
            app.swipeUp(velocity: .fast)
            app.swipeUp(velocity: .fast)
            swipes += 2
            // Read three times rather than every time. Asking the system what
            // is on screen means snapshotting the whole feed, which takes
            // seconds on a transcript this long, and doing it between every
            // swipe would leave a film of a screen standing still.
            if Self.readTheScreenAfter.contains(batch) {
                let visible = streamedLines(app)
                seenLines.formUnion(visible)
                furthest.append(visible.max() ?? -1)
            }
        }
        marker("end")
        record["streaming"] = [
            "swipes": swipes,
            "linesSeenWhileScrolling": seenLines.count,
            "furthestRowInView": furthest,
        ]
        XCTAssertGreaterThan(swipes, 14,
                             "the transcript was barely scrolled while the turn arrived")
        XCTAssertFalse(seenLines.isEmpty,
                       "scrolling the feed while the turn arrived read none of its rows")
        // The claim, measured: each time the screen was read, the turn had got
        // further than the time before. Rows were arriving while it was being
        // scrolled, not before.
        XCTAssertEqual(furthest.count, Self.readTheScreenAfter.count,
                       "the screen was not read as often as it was meant to be")
        XCTAssertEqual(furthest, furthest.sorted(),
                       "the feed did not get further into the turn as it was scrolled: \(furthest)")
        XCTAssertGreaterThan(furthest.last ?? -1, furthest.first ?? -1,
                             "no row arrived while the transcript was being scrolled: \(furthest)")

        // MARK: A run that ended.
        pressTab(app, "Agents")
        XCTAssertTrue(element(app, "home").waitForExistence(timeout: waiting),
                      "going back did not return to the home")
        // Opened first and ended while it is open, which is how somebody would
        // see a run end: they are reading it when it stops.
        press(app, "home.row.\(runner.ended)")
        XCTAssertTrue(element(app, "conversation").waitForExistence(timeout: waiting),
                      "opening the second agent did not lead to a conversation")
        try control.ask(["AgentExit": ["agent": "ran-its-course", "code": 7]])
        let ended = app.staticTexts["Exited · code 7"]
        XCTAssertTrue(ended.waitForExistence(timeout: waiting),
                      "the agent that ended does not say so: "
                      + "\(self.identifiers(app, startingWith: "conversation."))")
        record["exited"] = ended.exists ? ended.label : footSays(app)
        try? app.debugDescription.write(
            to: Self.inContainer("conversation-exited-tree.txt"), atomically: true,
            encoding: .utf8)
        XCTAssertFalse(element(app, "conversation.foot").exists,
                       "a run that ended offered somewhere to write")
        photograph(app, "conversation-exited")

        // MARK: The machine goes away, and comes back.
        //
        // Back to the conversation that is still running: the one that ended
        // has nothing to say about a machine going away, because it has
        // already said the only thing it has to say.
        pressTab(app, "Agents")
        XCTAssertTrue(element(app, "home").waitForExistence(timeout: waiting),
                      "leaving the agent that ended did not return to the home")
        press(app, "home.row.\(runner.agent)")
        XCTAssertTrue(conversation.waitForExistence(timeout: waiting),
                      "reopening the running agent did not lead to its conversation")
        // What is on screen before the machine goes, to compare against what
        // is on screen after it has.
        let readable = try waitForRows(app)
        try control.ask("CloudOffline")
        let gone = app.staticTexts["\(runner.host) is unreachable"]
        XCTAssertTrue(gone.waitForExistence(timeout: waiting),
                      "losing the machine left the conversation saying "
                      + "\(self.footSays(app))")
        record["unreachable"] = footSays(app)
        try? app.debugDescription.write(
            to: Self.inContainer("conversation-offline-tree.txt"), atomically: true,
            encoding: .utf8)
        // The transcript is the only account of this conversation there is
        // while the machine that owns it is away, and it is still true. A
        // reader keeps reading it; the chrome and the panel are what say the
        // machine has gone.
        let stale = transcriptRows(app)
        record["feedWhileUnreachable"] = stale
        XCTAssertFalse(stale.isEmpty,
                       "losing the machine emptied the transcript, which held \(readable)")
        XCTAssertTrue(Set(stale).isSubset(of: Set(readable)),
                      "an unreachable machine's transcript grew rows nobody sent: "
                      + "\(Set(stale).subtracting(Set(readable)))")
        // Reachable, not merely present. The panel sits in the composer's
        // place along the bottom edge, which is also where the tab bar floats,
        // and an offer drawn under that bar is one nobody can read or press.
        let retry = app.descendants(matching: .any)
            .matching(identifier: "conversation.retry").allElementsBoundByIndex
        XCTAssertFalse(retry.isEmpty,
                       "an unreachable machine offered no way to ask again")
        XCTAssertTrue(retry.contains(where: { $0.isHittable }),
                      "the way to ask again is on screen and cannot be pressed; the tab bar "
                      + "is over it")
        photograph(app, "conversation-stale")

        // A message tried while nothing can carry it.
        let whileUnreachable = try door(runner, .init(
            kind: "send", agent: runner.agent, text: "sent while unreachable"))
        record["whileUnreachable"] = whileUnreachable

        // The host starts a new transcript while the phone cannot hear it.
        // Replaying this must replace the retained rows, not append to them.
        try control.ask(["AgentPlay": ["agent": "carry-on", "steps": [
            // The synthetic streaming rows omit sessionId. A provider-written
            // row establishes the old identity in the host's replay window,
            // so the next identity is a change rather than its first evidence.
            ["Markdown": ["text": "The previous transcript ended while the phone was away."]],
            Self.recoveryRow(Self.replayed, id: "replayed"),
        ]]])
        XCTAssertFalse(app.staticTexts[Self.replayed].exists,
                       "a disconnected phone already shows the host's new transcript")
        press(app, "conversation.retry")
        try control.ask("CloudOnline")
        XCTAssertTrue(waitUntil { !gone.exists },
                      "the machine came back and the conversation still says it is gone")
        record["restored"] = footSays(app)
        let recovered = app.staticTexts[Self.replayed].waitForExistence(timeout: waiting)
        _ = try door(runner, .init(kind: "report", agent: runner.agent, path: runner.report))
        XCTAssertTrue(recovered,
                      "the open conversation never received the host's replay")
        guard recovered else { throw Lines.Failure("the host's replay did not arrive") }
        let replayedRows = readWholeFeed(app)
        XCTAssertEqual(replayedRows, ["transcript.prose"],
                       "the host's new transcript kept rows from the old one")
        let prose = app.descendants(matching: .any).matching(identifier: "transcript.prose")
        XCTAssertEqual(prose.count, 1, "the replay did not replace the retained transcript")
        record["feedAfterRestored"] = Array(replayedRows).sorted()
        record["replayedText"] = app.staticTexts[Self.replayed].label
        photograph(app, "conversation-restored")

        try control.ask(["AgentPlay": ["agent": "carry-on", "steps": [
            Self.recoveryRow(Self.afterRecovery, id: "live"),
        ]]])
        XCTAssertTrue(app.staticTexts[Self.afterRecovery].waitForExistence(timeout: waiting),
                      "the replay arrived but new rows no longer reach the open conversation")
        XCTAssertTrue(app.staticTexts[Self.replayed].exists,
                      "the live row replaced the replay instead of following it")
        XCTAssertEqual(prose.count, 2, "the live transcript lost or duplicated a row")
        record["liveAfterRestored"] = app.staticTexts[Self.afterRecovery].label
        photograph(app, "conversation-reconnected-live")
        _ = try door(runner, .init(kind: "report", agent: runner.agent, path: runner.report))
        try? app.debugDescription.write(
            to: Self.inContainer("conversation-restored-tree.txt"), atomically: true,
            encoding: .utf8)

        for (situation, attempt) in [
            ("while catching up", whileCatchingUp),
            ("while unreachable", whileUnreachable),
            ("while the last is unanswered", whileInFlight),
        ] {
            XCTAssertEqual(attempt["delivered"] as? Bool, false,
                           "a message sent \(situation) left the phone: \(attempt)")
            XCTAssertNotNil(attempt["reason"] as? String,
                            "a message refused \(situation) said nothing about why")
        }


        try write()
    }

    // MARK: - What the runner is told to play

    /// The text of the one message that is meant to arrive. The journey looks
    /// for exactly this on the host and for nothing else.
    static let delivered = "carry on then"

    private static let replayed = "A fresh transcript, started on the host while the phone was away."
    private static let afterRecovery = "The next row arrived after the connection returned."

    private static func recoveryRow(_ text: String, id: String) -> [String: Any] {
        ["Rows": ["jsonl": [
            ["type": "assistant", "uuid": "recovery-\(id)",
             "sessionId": "9210b4e1-2fb1-4c30-9ca7-490332330127",
             "message": ["id": "recovery-\(id)", "role": "assistant",
                         "content": [["type": "text", "text": text]]]],
        ]]]
    }

    /// The filmed turn, in batches: ten of them, twelve rows each, so that the
    /// turn arrives over the whole stretch that is being scrolled and filmed
    /// rather than all at once at the start of it.
    private static let streamedBatches = 10
    private static let rowsPerBatch = 12
    /// When to ask the system what is on screen. Three times, at the start,
    /// the middle and the end, because each answer costs a snapshot of a feed
    /// hundreds of rows long.
    private static let readTheScreenAfter = [0, 5, streamedBatches - 1]

    /// One batch of plain rows, each numbered, so that how far into the turn
    /// the screen has got can be read off what is written on it.
    private static func streamedBatch(_ batch: Int) -> [Any] {
        ((batch * rowsPerBatch)..<((batch + 1) * rowsPerBatch)).map { line in
            ["Rows": ["jsonl": [
                ["type": "assistant", "uuid": "streaming-\(line)",
                 "message": ["id": "s-\(line)", "role": "assistant",
                             "content": [["type": "text",
                                          "text": "Streaming line \(line) of a turn you are "
                                          + "reading while it arrives."]]]]]]]
        }
    }

    /// Every kind of step the scripted provider can play, in an order a turn
    /// would really take them in.
    private static var beforeAnybodyWatches: [Any] { [
        // Enough history that catching up on it is something a person could
        // watch happen. A conversation opened over three rows is caught up
        // before the screen has drawn, and the state a message is refused in
        // would never be reached.
        // One step per row rather than one step for all of them: each waits
        // for the host to have taken it, so the turn is still arriving while
        // somebody opens the conversation it is arriving into.
    ] + (0..<40).map { line in
        ["Rows": ["jsonl": [
            ["type": "assistant", "uuid": "row-\(line)",
             "message": ["id": "m-\(line)", "role": "assistant",
                         "content": [["type": "text",
                                      "text": "Line \(line) of what it did before anybody was looking."]]]]]]]
    } + [
        ["Markdown": ["text": "Looking at the parser first."]],
        ["Tool": ["name": "Read", "input": ["file_path": "/work/parser.rs"],
                  "output": "1 fn parse(input: &str) -> Vec<Token> {", "denied": false]],
        ["Tool": ["name": "Grep", "input": ["pattern": "split"],
                  "output": "parser.rs:1", "denied": false]],
    ] }

    private static var whileWatching: [Any] { [
        // A turn somebody asked for, so that the rule which closes a turn is
        // one of the rows this reads back. A turn nobody opened never ends.
        ["Prompt": ["text": "Have another look at the parser."]],
        ["Markdown": ["text": """
        ## What I found

        The parser drops the trailing newline. Three things follow from that:

        1. `parse` returns one token short
        2. the round trip is not a round trip
        3. `tests/wire.rs` passes for the wrong reason

        ```rust
        fn parse(input: &str) -> Vec<Token> { input.split('\\n').map(Token::new).collect() }
        ```

        | file | lines |
        | --- | --- |
        | parser.rs | 118 |

        > Worth reading [the note](https://example.invalid/note) before changing it.
        """]],
        ["Tool": ["name": "Read", "input": ["file_path": "/work/parser.rs"],
                  "output": NSNull(), "denied": false]],
        ["Tool": ["name": "Read", "input": ["file_path": "/work/wire.rs"],
                  "output": NSNull(), "denied": false]],
        ["Tool": ["name": "Grep", "input": ["pattern": "split"], "output": NSNull(),
                  "denied": false]],
        ["Tool": ["name": "Edit", "input": ["file_path": "/work/parser.rs",
                                            "old_string": "split", "new_string": "split_terminator"],
                  "output": "The file /work/parser.rs has been updated.", "denied": false,
                  // What the layer writes beside the result of an edit, and
                  // the only thing that says how much of the file moved.
                  "result": ["filePath": "/work/parser.rs",
                             "structuredPatch": [["lines": [
                                "-    input.split('\n')",
                                "+    input.split_terminator('\n')",
                                "+        .map(Token::new)"]]]]]],
        ["Tool": ["name": "Bash", "input": ["command": "cargo test -p parser"],
                  "output": String(repeating: "running 1 test\n", count: 40), "denied": false]],
        ["Tool": ["name": "Write", "input": ["file_path": "/work/secrets.env"],
                  "output": NSNull(), "denied": true]],
        // A file written whole, which is a different row from a file edited
        // and from one the person would not let it write.
        ["Tool": ["name": "Write", "input": ["file_path": "/work/notes.md"],
                  "output": "File created successfully at: /work/notes.md", "denied": false]],
        // Ran and came back an error. Not a refusal: nobody stopped this one.
        ["Tool": ["name": "Bash", "input": ["command": "cargo build -p parser"],
                  "output": "error[E0308]: mismatched types", "denied": false, "failed": true]],
        // A tool this build has no line of its own for, which is drawn as its
        // name and the one thing it came back with.
        ["Tool": ["name": "NotebookEdit", "input": ["notebook_path": "/work/notes.ipynb"],
                  "output": "Updated cell 3", "denied": false]],
        ["Todo": ["items": [["fix the parser", "completed"], ["run the suite", "in_progress"],
                            ["write it up", "pending"]]]],
        ["ChildStarted": ["name": "scout"]],
        ["AgentMessage": ["from": "scout", "text": "I looked at the other three callers and "
                          + "only one of them cares about the trailing newline."]],
        ["ChildFinished": ["name": "scout"]],
        // Another agent's session ending, which the carrier states as a kind
        // and the transcript draws as an event rather than a quote.
        ["AgentMessage": ["from": "scout", "text": "", "kind": "exited"]],
        ["Working": ["secs": 0.2]],
        ["ApiError": ["message": "upstream returned 529"]],
        // The person cut it off.
        ["Interrupted": ["tool_use": false]],
        "Compaction",
        ["Unknown": ["raw": ["type": "something_this_build_has_no_case_for", "value": 1]]],
        "EndTurn",
    ] }

    /// The rows those steps have to become. Named by what the screen calls
    /// them, so a kind that stops being drawn fails here rather than quietly
    /// going missing.
    private static let everyRowKind = [
        "transcript.prompt",
        "transcript.prose",
        "transcript.code",
        "transcript.exploration",
        "transcript.edit",
        "transcript.wrote",
        "transcript.ran",
        "transcript.output",
        "transcript.tool",
        "transcript.denied",
        "transcript.failed",
        "transcript.interrupted",
        "transcript.provider-error",
        "transcript.subagent",
        "transcript.agent-message",
        "transcript.exit",
        "transcript.unreadable",
        "transcript.compaction",
        "transcript.turn-end",
    ]

    // MARK: - Talking to the runner and to the app

    /// One JSON object per line, over a socket that stays open.
    ///
    /// The runner's control channel and the app's door speak the same shape,
    /// so one client serves both.
    private final class Lines {
        private let input: InputStream
        private let output: OutputStream
        private var pending = Data()

        init(address: String) throws {
            let parts = address.split(separator: ":")
            guard parts.count == 2, let port = UInt32(parts[1]) else {
                throw Failure("\(address) is not host:port")
            }
            var readable: InputStream?
            var writable: OutputStream?
            Stream.getStreamsToHost(
                withName: String(parts[0]), port: Int(port), inputStream: &readable,
                outputStream: &writable)
            guard let readable, let writable else { throw Failure("nothing answered at \(address)") }
            input = readable
            output = writable
            input.open()
            output.open()
        }

        deinit {
            input.close()
            output.close()
        }

        /// Says one request and does not wait for it to finish.
        ///
        /// Everything else here is a question. This is for work that has to
        /// still be going when the next thing happens — a turn being played
        /// while somebody opens the conversation it is being played into.
        func say(_ request: Any) throws {
            try write(request)
        }

        /// Reads the answer to something said earlier.
        @discardableResult
        func awaitAnswer() throws -> [String: Any] {
            try read()
        }

        /// Says one request and reads the one line that answers it.
        @discardableResult
        func ask(_ request: Any) throws -> [String: Any] {
            try write(request)
            return try read()
        }

        private func write(_ request: Any) throws {
            var line = try JSONSerialization.data(
                withJSONObject: request, options: [.fragmentsAllowed])
            line.append(0x0A)
            try line.withUnsafeBytes { bytes in
                var written = 0
                while written < line.count {
                    let wrote = output.write(
                        bytes.baseAddress!.advanced(by: written).assumingMemoryBound(to: UInt8.self),
                        maxLength: line.count - written)
                    guard wrote > 0 else { throw Failure("the socket stopped taking bytes") }
                    written += wrote
                }
            }
        }

        private func read() throws -> [String: Any] {
            var buffer = [UInt8](repeating: 0, count: 65536)
            let deadline = Date().addingTimeInterval(120)
            while Date() < deadline {
                if let newline = pending.firstIndex(of: 0x0A) {
                    let line = pending[pending.startIndex..<newline]
                    pending = pending[pending.index(after: newline)...]
                    guard !line.isEmpty else { continue }
                    guard let object = try JSONSerialization.jsonObject(with: Data(line))
                        as? [String: Any]
                    else { throw Failure("the answer was not one JSON object") }
                    if let error = object["Error"] {
                        throw Failure("the runner refused the request: \(error)")
                    }
                    return object
                }
                // A stream that has not finished connecting answers a read
                // with -1 and no error. Only a stream that has actually failed
                // or ended is a failure; everything else is waiting.
                let read = input.hasBytesAvailable
                    ? input.read(&buffer, maxLength: buffer.count) : 0
                if read > 0 {
                    pending.append(contentsOf: buffer[0..<read])
                } else if input.streamStatus == .error || input.streamStatus == .atEnd {
                    throw Failure(
                        "the socket \(input.streamStatus == .atEnd ? "closed" : "failed") while "
                        + "an answer was outstanding: "
                        + "\(input.streamError.map(String.init(describing:)) ?? "no reason given")")
                } else {
                    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
                }
            }
            throw Failure("nothing answered within two minutes")
        }

        struct Failure: Error, CustomStringConvertible {
            let description: String
            init(_ description: String) { self.description = description }
        }
    }

    /// One request for the app's own door, whose fields are the door's.
    private struct DoorRequest {
        var kind: String
        var agent: String
        var text: String?
        var base: String?
        var seconds: Double?
        var path: String?

        var body: [String: Any] {
            var fields: [String: Any] = ["kind": kind, "agent": agent]
            if let text { fields["text"] = text }
            if let base { fields["base"] = base }
            if let seconds { fields["seconds"] = seconds }
            if let path { fields["path"] = path }
            return fields
        }
    }

    /// Opens the door, says one thing, and closes it. One connection at a
    /// time is all the door serves, and holding one open across a test would
    /// stop anything else reaching it.
    private func door(_ runner: Runner, _ request: DoorRequest) throws -> [String: Any] {
        let door = try Lines(address: "127.0.0.1:\(runner.doorPort)")
        let answer = try door.ask(request.body)
        if let complaint = answer["message"] as? String, answer["kind"] as? String == "error" {
            XCTFail("the door refused \(request.kind): \(complaint)")
        }
        return answer
    }

    // MARK: - Reading a screen by the names it declares

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// What a named element says its value is.
    ///
    /// A name lands on more than one element in the tree the system builds,
    /// and only one of them carries the value the screen set; the others carry
    /// an empty string. So the first one with something in it is the answer.
    private func value(_ app: XCUIApplication, _ identifier: String) -> String? {
        app.descendants(matching: .any).matching(identifier: identifier)
            .allElementsBoundByIndex.compactMap { $0.value as? String }
            .first { !$0.isEmpty }
    }

    /// What the panel where the composer will go is saying, in its own words.
    ///
    /// Read off the writing rather than off the row's name, because a name
    /// declared on a screen reaches the system's tree as an identifier alone.
    private func footSays(_ app: XCUIApplication) -> [String] {
        let foot = app.descendants(matching: .any)
            .matching(identifier: "conversation.foot").firstMatch
        guard foot.exists else { return [] }
        return foot.staticTexts.allElementsBoundByIndex.map { $0.label }
    }

    private func identifiers(_ app: XCUIApplication, startingWith prefix: String) -> [String] {
        let matching = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
        var seen: [String] = []
        for element in matching.allElementsBoundByIndex where !seen.contains(element.identifier) {
            seen.append(element.identifier)
        }
        return seen
    }

    private func transcriptRows(_ app: XCUIApplication) -> [String] {
        identifiers(app, startingWith: "transcript.")
    }

    /// Waits until the transcript has stopped growing, and answers with what it
    /// holds. A feed that is still arriving would be read half way through.
    private func waitForRows(_ app: XCUIApplication) throws -> [String] {
        var last: [String] = []
        var still = 0
        let deadline = Date().addingTimeInterval(waiting)
        while Date() < deadline {
            let now = transcriptRows(app)
            // Six identical reads a half-second apart, because a feed that is
            // still catching up pauses between batches and a shorter run
            // reads one of those pauses as the end of it.
            still = now == last && !now.isEmpty ? still + 1 : 0
            last = now
            if still >= 6 { return now }
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        }
        throw Lines.Failure("the transcript never settled; it holds \(last)")
    }

    /// Every kind of row in the whole feed, gathered by scrolling it from the
    /// end to the beginning.
    ///
    /// XCUITest is an accessibility client and sees what an accessibility
    /// client sees: the elements the screen is currently showing. A transcript
    /// is longer than a phone, so the names are unioned across the walk rather
    /// than read once at the bottom.
    /// Scrolls to the end of the feed and answers with what is on screen
    /// there. The rows a turn ends with — what was refused, what failed, what
    /// was cut off — are only ever at the bottom of a long one.
    private func toTheEnd(_ app: XCUIApplication) -> [String] {
        var last: [String] = []
        var unchanged = 0
        for _ in 0..<60 {
            app.swipeUp(velocity: .fast)
            let now = transcriptRows(app)
            unchanged = now == last ? unchanged + 1 : 0
            last = now
            if unchanged >= 3 { break }
        }
        return last
    }

    private func readWholeFeed(_ app: XCUIApplication) -> Set<String> {
        var found = Set(transcriptRows(app))
        var unchanged = 0
        // To the end first: a conversation with history behind it opens onto
        // the beginning of it, and everything this turn did is below that.
        for _ in 0..<60 {
            let before = found.count
            app.swipeUp(velocity: .fast)
            found.formUnion(transcriptRows(app))
            unchanged = found.count == before ? unchanged + 1 : 0
            if unchanged >= 4 { break }
        }
        unchanged = 0
        for _ in 0..<60 {
            let before = found.count
            app.swipeDown(velocity: .fast)
            found.formUnion(transcriptRows(app))
            unchanged = found.count == before ? unchanged + 1 : 0
            if unchanged >= 4 { break }
        }
        return found
    }

    /// Scrolls until the named row is on screen, in whichever direction it is.
    private func reveal(_ app: XCUIApplication, _ identifier: String) {
        for attempt in 0..<40 {
            // On screen is not the same as reachable: a row under the floating
            // chrome or behind the panel at the foot exists and cannot be
            // pressed, and scrolling one more step is what a person does about
            // that.
            let candidates = app.descendants(matching: .any)
                .matching(identifier: identifier).allElementsBoundByIndex
            if candidates.contains(where: { $0.isHittable }) { return }
            if attempt < 20 { app.swipeDown(velocity: .slow) } else { app.swipeUp(velocity: .slow) }
        }
        XCTFail("scrolling the feed never brought \(identifier) somewhere it could be pressed")
    }

    private func waitUntil(_ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(waiting)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        }
        return condition()
    }

    private func pressTab(_ app: XCUIApplication, _ title: String) {
        let button = app.tabBars.buttons[title]
        guard button.waitForExistence(timeout: waiting) else {
            return XCTFail("the tab bar has no \(title) tab")
        }
        button.tap()
    }

    private func press(_ app: XCUIApplication, _ identifier: String) {
        let candidates = app.descendants(matching: .any)
            .matching(identifier: identifier).allElementsBoundByIndex
        let hittable = candidates.first(where: { $0.isHittable })
        guard let target = hittable ?? candidates.first else {
            return XCTFail("nothing on screen is named \(identifier)")
        }
        if target.isHittable {
            target.tap()
        } else {
            target.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    /// Which of the filmed turn's rows are on screen right now.
    private func streamedLines(_ app: XCUIApplication) -> Set<Int> {
        var found = Set<Int>()
        for label in app.staticTexts.allElementsBoundByIndex.map({ $0.label })
        where label.hasPrefix("Streaming line ") {
            let number = label.dropFirst("Streaming line ".count).prefix { $0.isNumber }
            if let line = Int(number) { found.insert(line) }
        }
        return found
    }

    /// Says what this test is doing, where the Mac that started it can read it.
    ///
    /// The Mac cannot see inside a UI test — it starts one and waits — so the
    /// stretch worth filming is marked by writing a word to a file in this
    /// process's own container, which is a real directory on the Mac's disk.
    private func marker(_ word: String) {
        try? word.write(
            to: Self.inContainer("conversation-streaming.marker"),
            atomically: true, encoding: .utf8)
    }

    private func photograph(_ app: XCUIApplication, _ name: String) {
        let shot = XCUIScreen.main.screenshot()
        try? shot.pngRepresentation.write(to: Self.inContainer("\(name).png"))
        let attached = XCTAttachment(screenshot: shot)
        attached.name = name
        attached.lifetime = .keepAlways
        add(attached)
    }

    /// Everything this test read, left where the Mac can collect it. The
    /// journey asserts on this as well as on the test passing, so a failure
    /// afterwards can be read against what was actually on screen.
    private func write() throws {
        let data = try JSONSerialization.data(
            withJSONObject: record, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: Self.inContainer("conversation.json"))
    }
}
