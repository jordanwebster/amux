import Foundation
import XCTest

/// Everything a person does to give this phone machines to work on, against
/// two accounts on one relay and three machines the runner is really running.
///
/// It is a UI test because every act in it is a finger: six digits on a keypad,
/// a fingerprint read and turned down, the same machine trusted a minute later,
/// a machine chosen and a directory picked and an agent started on it, and a
/// key revoked. What the machines themselves then hold is read back over the
/// runner's control channel, because a phone reporting what it asked for would
/// be quoting its own request: the question throughout is whether the far side
/// agrees.
final class HostsTests: JourneyCase {
    /// The machines and the accounts, as the journey named them.
    private struct Cast {
        let laptop: String
        let desktop: String
        let workstation: String
        let laptopKey: String
        let desktopKey: String
        let workToken: String
        let link: String
        let recent: String
        let repository: String
        let typedPath: String
        let refusedPath: String
        /// An agent the topology seeded on the machine whose key is revoked.
        let desktopAgent: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            laptop = try required("AMUX_LAPTOP")
            desktop = try required("AMUX_DESKTOP")
            workstation = try required("AMUX_WORKSTATION")
            laptopKey = try required("AMUX_LAPTOP_FINGERPRINT")
            desktopKey = try required("AMUX_DESKTOP_FINGERPRINT")
            workToken = try required("AMUX_WORK_TOKEN")
            link = try required("AMUX_LINK")
            recent = try required("AMUX_RECENT")
            repository = try required("AMUX_REPOSITORY")
            typedPath = try required("AMUX_TYPED_PATH")
            refusedPath = try required("AMUX_REFUSED_PATH")
            desktopAgent = try required("AMUX_DESKTOP_AGENT")
        }
    }

    /// Every machine the runner is running, whichever account it belongs to.
    private static let machines = ["laptop", "desktop", "workstation"]

    private var runner: Runner!
    private var cast: Cast!
    private var control: Lines!
    /// Every agent this test started, as the machine that started it describes
    /// it. Written out for whoever reads the run afterwards.
    private var created: [[String: Any]] = []
    /// The agents the three machines were already running before this test
    /// touched them, so an agent that appears anywhere afterwards can be told
    /// from one that was always there.
    private var seeded: Set<String> = []
    /// What the layer cards said was chosen each time Start went down, in the
    /// order the presses happened.
    private var layersWhenStarted: [String] = []

    func testTwoAccountsPairCreateRevokeAndSeeOnlyTheirOwnMachines() throws {
        runner = try Runner()
        cast = try Cast(ProcessInfo.processInfo.environment)
        control = try Lines(address: runner.control)
        // Written whatever happens: what the phone read is how a failure here
        // is understood afterwards, and a run that stopped at the first bad
        // assertion has the most to explain.
        defer { try? write("hosts.json") }
        for machine in Self.machines {
            seeded.formUnion(try inventory(machine).agents.compactMap { $0["id"] as? String })
        }
        record["seededAgents"] = seeded.count

        try aLinkThatArrivesBeforeAnybodyHasSignedIn()
        try theSameLinkAgreedToTheSecondTime()
        try aCodeTypedOnTheKeypad()
        try agentsStartedOnAMachineThatSaysWhatTheyAre()
        try aKeyRevokedAndTheMachineLost()
        try theRelayAndAMachineDisturbed()
        try anAccountThatSeesOnlyItsOwnMachines()

        record["createdAgents"] = created
    }

    // MARK: - What a machine itself says it holds

    /// The agents a machine is running and the devices it trusts, in its own
    /// words rather than the phone's.
    private func inventory(_ machine: String) throws -> (agents: [[String: Any]],
                                                         devices: [[String: Any]]) {
        let answer = try control.ask(["Inventory": ["daemon": machine]])
        let ack = answer["Ack"] as? [String: Any]
        return (ack?["agents"] as? [[String: Any]] ?? [],
                ack?["devices"] as? [[String: Any]] ?? [])
    }

    private func bridge() throws -> [String: Any] {
        try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any] ?? [:]
    }

    private func reconciled() throws -> Bool { try bridge()["reconciled"] as? Bool ?? false }

    /// The machines on the phone's Hosts tab, by name.
    private func machinesOnScreen() throws -> [String] {
        try bridge()["hosts"] as? [String] ?? []
    }

    /// The agents this phone's runtime is holding a stream for, off its own
    /// model rather than off what is on screen.
    private func watching() throws -> [String] {
        try bridge()["watching"] as? [String] ?? []
    }

    /// A code that machine has just printed, live for as long as asked.
    private func code(from machine: String, seconds: Int = 600) throws -> String {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": machine, "ttl_secs": seconds]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                             "\(machine) printed no code")
    }

    /// The agents the phone believes in, by name.
    private func fleetOnScreen() throws -> [String] {
        try bridge()["agents"] as? [String] ?? []
    }

    /// Six digits, pressed one key at a time as a person presses them.
    ///
    /// All but the last go first and the keypad is read before the sixth: a
    /// code is sent the moment it is complete, so this is the last moment at
    /// which the screen can be seen to be holding no refusal — which is what
    /// makes the refusal that follows this code's and not the one before it.
    private func typeCode(_ digits: String, into app: XCUIApplication) throws {
        let head = String(digits.dropLast())
        for digit in head { press(app, "pin.key.\(digit)") }
        XCTAssertEqual(try waitForValue(runner, "pin.code", head), head,
                       "the keypad did not take the digits it was given")
        XCTAssertFalse(element(app, "pin.refused").exists,
                       "a refusal from an earlier code is still on the keypad")
        press(app, "pin.key.\(digits.last ?? "0")")
    }

    private func spaceless(_ text: String) -> String {
        text.replacingOccurrences(of: " ", with: "")
    }

    /// Presses Start and waits for the machine's answer, which is either the
    /// conversation the new agent opened into or a refusal in the machine's
    /// own words. The refusal is answered back rather than failed on, because
    /// one of the paths this test types is one the machine is meant to refuse.
    ///
    /// A press that reaches neither is the hardest failure here to read after
    /// the fact — a create that went nowhere leaves the form exactly as it
    /// was — so what the form was showing is said out loud.
    private func pressStart(_ app: XCUIApplication, _ complaint: String) -> String? {
        // Pressed again where a press did not take. The button says whether
        // it did — a create that left the phone reads "starting" until the
        // machine answers — so a second turn round this loop is not a second
        // create: it is the same one press that never landed, which is what a
        // person does with a button that did nothing.
        var answered = false
        for _ in 0..<3 {
            // The button is waited for where it will still be a moment later, not
            // merely where it is. The directory chooser holds the keyboard, and
            // closing it drops the whole floating foot back down across the layer
            // cards; a tap sent into that slide lands on Codex's card instead of
            // on Start, which changes what gets started with nothing on screen
            // afterwards looking wrong. Two readings of the same frame is what
            // says the slide is over.
            let button = element(app, "new-agent.start")
            var previous = CGRect.null
            waitUntil(within: 10) {
                let now = button.exists ? button.frame : .null
                defer { previous = now }
                return now == previous && !now.isNull && button.isHittable
            }

            // And what the screen says will be started, read the moment before it
            // is. The machine's answer is the claim this journey rests on, and it
            // only means the app chose the layer if the app was showing that layer
            // when the button went down.
            let layers = ((try? declared(runner)) ?? [])
                .filter { $0.identifier.hasPrefix("new-agent.provider.") }
            layersWhenStarted.append(layers.map { "\($0.identifier)=\($0.value)" }.sorted()
                                         .joined(separator: " "))
            record["layersWhenStarted"] = layersWhenStarted
            XCTAssertEqual(said(layers, "new-agent.provider.claude")?.value, "chosen",
                           "Start was pressed on a screen showing "
                           + "\(layers.map { "\($0.identifier)=\($0.value)" }.sorted())")

            press(app, "new-agent.start")
            answered = waitUntil(within: 20) {
                self.element(app, "conversation").exists
                    || self.element(app, "new-agent.refusal").exists
                    || self.said((try? self.declared(self.runner)) ?? [],
                                 "new-agent.start")?.value == "starting"
            }
            if answered { break }
        }
        if answered {
            answered = waitUntil {
                self.element(app, "conversation").exists
                    || self.element(app, "new-agent.refusal").exists
            }
        }
        // A conversation is the whole answer: a refusal left over from the
        // path before this one is cleared by choosing another, but reading it
        // first would still let a stale sentence outrank an agent that exists.
        if element(app, "conversation").exists { return nil }
        if element(app, "new-agent.refusal").exists {
            return said((try? declared(runner)) ?? [], "new-agent.refusal")?.value ?? ""
        }
        // What the form itself says, values and all: whether it holds a
        // directory, and whether the button reads as ready or as already
        // starting, is the difference between a press that went nowhere and a
        // machine that never answered.
        let form = ((try? declared(runner)) ?? [])
            .filter { $0.identifier.hasPrefix("new-agent") }
            .map { "\($0.identifier)=\($0.value)" }
        if !answered {
            // What the screen looked like when the press went nowhere: whether
            // anything is standing over the button is not a thing the declared
            // elements can say.
            photograph(app, "start-did-nothing")
        }
        XCTAssertTrue(
            answered,
            "\(complaint); the machine said nothing, the button was "
                + "\(element(app, "new-agent.start").isHittable ? "" : "not ")reachable "
                + "and the form shows \(form)")
        return nil
    }

    // MARK: - A link that lands before there is an account

    /// The app opened by a pairing link on a phone nobody has signed in on.
    ///
    /// Nothing can be asked of the machine yet — there is no runtime and no
    /// account — so the invitation is held rather than spent, and the screen
    /// says nothing has been trusted. Signing in is what puts it to the
    /// machine, and only then is there a name and a key to read.
    private func aLinkThatArrivesBeforeAnybodyHasSignedIn() throws {
        let app = launch(runner, signedIn: false, link: cast.link)
        waitFor(app, "pair-confirm", "a launch opened by a pairing link showed no confirmation")
        XCTAssertTrue(element(app, "pair-confirm.checking").exists,
                      "a link on a signed-out phone claimed something about the machine")
        let beforeSigningIn = try inventory("desktop").devices
        record["desktopDevicesBeforeSigningIn"] = beforeSigningIn.count
        XCTAssertTrue(beforeSigningIn.isEmpty,
                      "a link that only arrived was already trusted by desktop")

        // Signing in, which is the gate this launch stopped at.
        try door(runner, .init(kind: "connect", relay: runner.relay, token: runner.token,
                               user: runner.user))
        waitFor(app, "pair-confirm.trust",
                "signing in never put the held invitation to the machine")
        let offered = try declared(runner)
        let name = said(offered, "pair-confirm.name")?.value ?? ""
        let key = said(offered, "pair-confirm.fingerprint")?.value ?? ""
        record["linkOfferedMachine"] = name
        record["linkOfferedFingerprint"] = key
        XCTAssertEqual(name, "desktop", "the invitation named a machine nobody offered")
        XCTAssertEqual(spaceless(key), cast.desktopKey,
                       "the key on screen is not the key desktop holds")
        photograph(app, "confirm")

        // Turned down. Nothing is written on either side, which is the whole
        // point of the second act being a person's.
        press(app, "pair-confirm.abandon")
        waitFor(app, "hosts", "turning the machine away did not lead back to the machines")
        let afterTurningAway = try inventory("desktop").devices
        record["desktopDevicesAfterCancelling"] = afterTurningAway.count
        XCTAssertTrue(afterTurningAway.isEmpty,
                      "a machine turned down on the confirmation trusted this phone anyway")
        record["cancelledWithoutTrust"] = true
        app.terminate()
    }

    // MARK: - Six digits

    /// A code mistyped, a code that has run out, and a code that works — all
    /// on the keypad, and the first two indistinguishable from each other.
    private func aCodeTypedOnTheKeypad() throws {
        let app = launch(runner)
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the phone never reached the relay")
        // The machines this phone has not paired with are offered on the
        // Hosts tab, and a code is authenticated against exactly one of them.
        pressTab(app, "Hosts")
        waitFor(app, "hosts.pair.\(cast.laptop)", "laptop was never offered to pair with")
        press(app, "hosts.pair.\(cast.laptop)")
        waitFor(app, "pin", "the offer to pair did not lead to the keypad")
        photograph(app, "pin")

        // A code nobody issued.
        try typeCode("135790", into: app)
        waitFor(app, "pin.refused", "a code nobody issued was not refused")
        let mistyped = said(try declared(runner), "pin.refused")?.value ?? ""
        let clearedAfterMistyping = try waitForValue(runner, "pin.code", "")
        record["refusedAfterAWrongCode"] = mistyped
        record["digitsAfterAWrongCode"] = clearedAfterMistyping
        XCTAssertEqual(clearedAfterMistyping, "", "a refused code was left on the keypad")

        // A code that has run out. The machine printed this one, so the only
        // thing wrong with it is that it is late.
        let stale = try code(from: "laptop", seconds: 1)
        Thread.sleep(forTimeInterval: 3)
        try typeCode(stale, into: app)
        waitFor(app, "pin.refused", "an expired code was not refused")
        let expired = said(try declared(runner), "pin.refused")?.value ?? ""
        let clearedAfterExpiring = try waitForValue(runner, "pin.code", "")
        record["refusedAfterAnExpiredCode"] = expired
        record["digitsAfterAnExpiredCode"] = clearedAfterExpiring
        XCTAssertEqual(clearedAfterExpiring, "", "an expired code was left on the keypad")
        XCTAssertEqual(mistyped, expired,
                       "a wrong code and an expired one are told apart on screen")
        XCTAssertFalse(mistyped.isEmpty, "a refused code said nothing at all")

        // And one that works, which authenticates and stops there: the
        // machine's name and key are read before anything is trusted.
        let good = try code(from: "laptop")
        try typeCode(good, into: app)
        waitFor(app, "pair-confirm.trust", "a code the machine printed reached no confirmation")
        let offered = try declared(runner)
        record["codeOfferedMachine"] = said(offered, "pair-confirm.name")?.value ?? ""
        let key = said(offered, "pair-confirm.fingerprint")?.value ?? ""
        XCTAssertEqual(said(offered, "pair-confirm.name")?.value, "laptop",
                       "the code was answered by a machine nobody typed it for")
        XCTAssertEqual(spaceless(key), cast.laptopKey,
                       "the key on screen is not the key laptop holds")
        press(app, "pair-confirm.trust")
        waitFor(app, "pair-confirm.trusted", "trusting laptop was never confirmed")
        press(app, "pair-confirm.done")
        waitFor(app, "hosts", "the pairing did not lead back to the machines")
        waitFor(app, "hosts.row.\(cast.laptop)", "laptop never joined the machines")

        let held = try inventory("laptop").devices
        record["laptopDevicesAfterTheCode"] = held.map { $0["name"] as? String ?? "" }
        XCTAssertEqual(held.count, 1,
                       "laptop holds \(held.count) keys after one phone paired with it")
        let both = waitUntil { (try? self.machinesOnScreen())?.sorted() == ["desktop", "laptop"] }
        let showing = try machinesOnScreen()
        record["machinesAfterBothPairings"] = showing
        XCTAssertTrue(both, "the phone shows \(showing) after pairing with both machines")
        photograph(app, "hosts")
        app.terminate()
    }

    // MARK: - The link, taken up the second time

    /// The machine turned down earlier, offering the same invitation, agreed
    /// to this time.
    ///
    /// The same invitation and not another one: turning a machine away tells
    /// it so and leaves it offering — nothing was spent and nothing was
    /// written — so what a person does next is open the same link again.
    private func theSameLinkAgreedToTheSecondTime() throws {
        let app = launch(runner, link: cast.link)
        waitFor(app, "pair-confirm.trust", "the invitation never named its machine the second time")
        press(app, "pair-confirm.trust")
        waitFor(app, "pair-confirm.trusted", "trusting desktop was never confirmed")
        press(app, "pair-confirm.done")
        let trusted = try inventory("desktop").devices
        record["desktopDevicesAfterConfirming"] = trusted.count
        XCTAssertEqual(trusted.count, 1,
                       "desktop holds \(trusted.count) keys after the invitation was accepted")
        let arrived = waitUntil { (try? self.machinesOnScreen()) == ["desktop"] }
        let showing = try machinesOnScreen()
        record["machinesAfterTheLink"] = showing
        XCTAssertTrue(arrived, "the phone shows \(showing) after trusting desktop")
        app.terminate()
    }

    // MARK: - Starting agents

    /// Three agents started on laptop — from a directory it was used in, from
    /// the repositories it listed, and from a path typed by hand — and what
    /// laptop then says each of them is.
    ///
    /// The claim is about the machine, not the screen: this app creates Claude
    /// sessions on the SDK layer and never on the terminal one, and the only
    /// place that can be checked is the machine that took the request. A
    /// machine refuses a create that leaves the driver unsaid, so an agent
    /// standing there as `claude/sdk` is an agent whose request named it.
    private func agentsStartedOnAMachineThatSaysWhatTheyAre() throws {
        let app = launch(runner)
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the phone never reached the relay")
        let before = try inventory("laptop").agents
        record["agentsBeforeAnyWereStarted"] = before.map { $0["name"] as? String ?? "" }
        let known = Set(before.compactMap { $0["id"] as? String })

        /// What laptop says about the one agent this test has just added.
        ///
        /// Found by identity rather than by position: a machine answers with
        /// its whole inventory in whatever order it holds it, so the agent a
        /// press added is the one identifier that was neither there before any
        /// of this nor written down here already.
        var recorded: Set<String> = []
        func newest(_ complaint: String) throws -> [String: Any] {
            var added: [[String: Any]] = []
            let arrived = waitUntil {
                added = ((try? self.inventory("laptop").agents) ?? []).filter {
                    let id = $0["id"] as? String ?? ""
                    return !known.contains(id) && !recorded.contains(id)
                }
                return added.count == 1
            }
            XCTAssertTrue(arrived, "\(complaint); laptop is running \(added)")
            let agent = try XCTUnwrap(added.first, complaint)
            recorded.insert(agent["id"] as? String ?? "")
            XCTAssertEqual(agent["kind"] as? String, "claude",
                           "the machine started something that is not Claude: \(agent)")
            XCTAssertEqual(agent["driver"] as? String, "sdk",
                           "the machine started Claude on the wrong driver: \(agent)")
            defer { self.record["createdAgents"] = self.created }
            created.append([
                "host": "laptop",
                "id": agent["id"] as? String ?? "",
                "name": agent["name"] as? String ?? "",
                "kind": agent["kind"] as? String ?? "",
                "driver": agent["driver"] as? String ?? "",
            ])
            return agent
        }

        /// Opens New Agent on laptop, whatever the last thing on screen was.
        func openNewAgent() {
            pressTab(app, "Agents")
            waitFor(app, "home.newAgent", "the fleet offered no way to start an agent")
            press(app, "home.newAgent")
            waitFor(app, "new-agent", "New Agent never opened")
            press(app, "new-agent.host.\(cast.laptop)")
        }

        // MARK: A directory the machine was used in.
        openNewAgent()
        waitFor(app, "new-agent.recent.\(cast.recent)",
                "laptop never offered the directory its own agent is running in")
        photograph(app, "new-agent")
        press(app, "new-agent.recent.\(cast.recent)")
        if let refused = pressStart(app, "starting in a recent directory started nothing") {
            XCTFail("laptop refused a directory it had itself been used in: \(refused)")
        }
        waitFor(app, "conversation", "the agent that was started opened no conversation")
        let fromRecents = try newest("starting from a recent directory reached no new agent")
        record["startedFromRecents"] = fromRecents["working_dir"] as? String ?? ""

        // The conversation it opened into is the SDK layer's own, which this
        // build cannot read yet and says so. What it must never be is a
        // terminal transcript: an SDK session re-read as a PTY one would draw
        // somebody else's rows under this agent's name.
        waitFor(app, "transcript.unsupported",
                "the created agent's conversation is not the SDK layer's")
        let drawn = transcriptRows(app)
        record["rowsOnTheCreatedAgent"] = drawn
        let terminalRows = drawn.filter {
            ["transcript.prose", "transcript.code", "transcript.read", "transcript.wrote",
             "transcript.tool", "transcript.turn-end"].contains($0)
        }
        XCTAssertTrue(terminalRows.isEmpty,
                      "an SDK session was drawn as a terminal transcript: \(terminalRows)")

        // MARK: A repository the machine listed.
        openNewAgent()
        press(app, "new-agent.directory")
        waitFor(app, "new-agent.browse", "the directory chooser never opened")
        try door(runner, .init(kind: "type", text: cast.repository,
                               identifier: "new-agent.search"))
        waitFor(app, "new-agent.project.\(cast.repository)",
                "searching laptop's repositories found nothing")
        press(app, "new-agent.project.\(cast.repository)")
        if let refused = pressStart(app, "starting in a listed repository started nothing") {
            XCTFail("laptop refused a repository it had listed itself: \(refused)")
        }
        waitFor(app, "conversation", "the agent started in a listed repository opened nothing")
        let fromListing = try newest("starting in a listed repository reached no new agent")
        record["startedFromTheListing"] = fromListing["working_dir"] as? String ?? ""

        // MARK: A path typed by hand — one the machine will not take, and
        // then one it will.
        openNewAgent()
        press(app, "new-agent.directory")
        waitFor(app, "new-agent.browse", "the directory chooser never opened")
        try door(runner, .init(kind: "type", text: cast.refusedPath,
                               identifier: "new-agent.typed"))
        press(app, "new-agent.typed.use")
        let refusal = pressStart(app, "a path the machine cannot use was not refused on screen")
        record["whatTheMachineSaidAboutAPathItRefused"] = refusal ?? ""
        XCTAssertFalse(refusal?.isEmpty ?? true,
                       "a path the machine cannot use was not refused on screen")
        XCTAssertFalse(element(app, "conversation").exists,
                       "a refused path opened a conversation anyway")

        press(app, "new-agent.directory")
        waitFor(app, "new-agent.browse", "the directory chooser never reopened")
        // The field still holds the path that was refused: typing adds to what
        // is there, so the typo comes out before the right one goes in.
        try door(runner, .init(kind: "clear", identifier: "new-agent.typed"))
        try door(runner, .init(kind: "type", text: cast.typedPath,
                               identifier: "new-agent.typed"))
        press(app, "new-agent.typed.use")
        if let refused = pressStart(app, "starting in a typed path started nothing") {
            XCTFail("laptop refused a path typed by hand: \(refused)")
        }
        waitFor(app, "conversation", "the agent started in a typed path opened nothing")
        let fromATypedPath = try newest("starting in a typed path reached no new agent")
        record["startedFromATypedPath"] = fromATypedPath["working_dir"] as? String ?? ""

        // MARK: And the phone's own fleet agrees with the machine.
        let names = Set(created.compactMap { $0["name"] as? String })
        let agreed = waitUntil { names.isSubset(of: Set((try? self.fleetOnScreen()) ?? [])) }
        let fleet = try fleetOnScreen()
        record["fleetAfterStartingThree"] = fleet
        XCTAssertTrue(agreed,
                      "the machines answered for \(names.sorted()) and the phone shows \(fleet)")

        // MARK: The agents the topology seeded are still what they were.
        //
        // Opened once, with the driver they were started under, and their
        // conversation drawn as a transcript rather than as an unreadable
        // layer. Nothing this test did may have turned a terminal session into
        // something else.
        try control.ask(["AgentPlay": ["agent": "fix-login", "steps": [
            ["Markdown": ["text": "Reading the parser before anything else."]],
        ]]])
        pressTab(app, "Agents")
        waitFor(app, "home.row.\(runner.agent)", "the machine's own agent is not on the fleet")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation", "the seeded agent's conversation did not open")
        XCTAssertTrue(waitUntil { self.transcriptRows(app).contains("transcript.prose") },
                      "a terminal session drew no transcript: \(transcriptRows(app))")
        record["rowsOnTheSeededAgent"] = transcriptRows(app)
        XCTAssertFalse(element(app, "transcript.unsupported").exists,
                       "a terminal session was read as a layer this build cannot read")

        let after = try inventory("laptop").agents
        record["agentsAfterStartingThree"] = after.map {
            "\($0["name"] as? String ?? "") \($0["kind"] as? String ?? "")/"
            + "\($0["driver"] as? String ?? "none")"
        }
        // Nothing anywhere started a terminal Claude session while this ran.
        // The two the topology seeded are the only ones there have ever been.
        for machine in Self.machines {
            let terminalSessions = try inventory(machine).agents.filter {
                $0["kind"] as? String == "claude" && $0["driver"] as? String == "pty"
                    && !seeded.contains($0["id"] as? String ?? "")
            }
            XCTAssertTrue(terminalSessions.isEmpty,
                          "a terminal Claude session appeared on \(machine) while this ran: "
                          + "\(terminalSessions)")
        }
        record["startedAgents"] = created
        app.terminate()
    }

    // MARK: - Taking a machine's key away

    /// A key revoked on the phone, and the machine that held it losing this
    /// phone at once.
    private func aKeyRevokedAndTheMachineLost() throws {
        let app = launch(runner)
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the phone never reached the relay")
        // One of desktop's agents opened and read first, so what the
        // revocation ends is access that existed rather than a row in a list.
        // The stream behind the conversation is read off the runtime's own
        // model, because a screen that has been left says nothing about
        // whether what it was reading is still open.
        pressTab(app, "Agents")
        waitFor(app, "home.row.\(cast.desktopAgent)",
                "the machine about to be revoked has no agent on the fleet")
        press(app, "home.row.\(cast.desktopAgent)")
        waitFor(app, "conversation", "the agent on desktop opened no conversation")
        let held = waitUntil { (try? self.watching())?.contains(self.cast.desktopAgent) == true }
        let streams = try watching()
        record["watchingBeforeRevoking"] = streams
        XCTAssertTrue(held, "reading desktop's agent held no stream: \(streams)")

        pressTab(app, "Hosts")
        waitFor(app, "hosts.row.\(cast.desktop)", "desktop is not among the machines")
        waitFor(app, "hosts.fact.paired-devices", "the Hosts tab does not say what this phone is")
        press(app, "hosts.fact.paired-devices")
        waitFor(app, "hosts.devices.sheet", "the paired devices never opened")
        let keys = try declared(runner)
        record["thisPhone"] = said(keys, "hosts.devices.identity")?.label ?? ""
        record["desktopKeyOnScreen"] = said(keys, "hosts.device.\(cast.desktop)")?.value ?? ""
        XCTAssertEqual(said(keys, "hosts.device.\(cast.desktop)")?.value, cast.desktopKey,
                       "the key beside desktop is not the key desktop holds")

        press(app, "hosts.revoke.\(cast.desktop)")
        let gone = waitUntil { ((try? self.machinesOnScreen()) ?? ["desktop"]) == ["laptop"] }
        let remaining = try machinesOnScreen()
        record["machinesAfterRevoking"] = remaining
        XCTAssertTrue(gone, "revoking desktop left it on the phone: \(remaining)")

        // The access that was open when the key went is closed: the stream the
        // conversation was reading is let go, not left running behind a screen
        // nobody can get back to.
        let closed = waitUntil {
            ((try? self.watching()) ?? [self.cast.desktopAgent]).contains(self.cast.desktopAgent)
                == false
        }
        let left = try watching()
        record["watchingAfterRevoking"] = left
        XCTAssertTrue(closed, "the phone is still streaming the revoked machine's agent: \(left)")

        // What desktop itself still holds. Revoking is one-sided by design as
        // this build stands: the phone stops trusting the machine's key and
        // the link is closed with the reason on it, and the machine's own
        // record of this phone is the machine's to remove. Written down rather
        // than asserted away, so a run says what each side ended up with.
        record["desktopDevicesAfterRevoking"] = try inventory("desktop").devices.count
        // Access ends with the trust: what desktop was running is no longer
        // anything this phone can read.
        let lost = waitUntil { !(((try? self.fleetOnScreen()) ?? []).contains("release-notes")) }
        let fleet = try fleetOnScreen()
        record["fleetAfterRevoking"] = fleet
        XCTAssertTrue(lost, "an agent on a revoked machine is still on the fleet: \(fleet)")
        app.terminate()
    }

    // MARK: - Disturbance

    /// The relay taken away and put back, and a machine restarted underneath —
    /// with nobody pressing anything.
    private func theRelayAndAMachineDisturbed() throws {
        let app = launch(runner)
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the phone never reached the relay")
        pressTab(app, "Hosts")
        waitFor(app, "hosts.row.\(cast.laptop)", "laptop is not among the machines")

        try control.ask("CloudOffline")
        try door(runner, .init(kind: "awaitOffline", seconds: 60))
        record["whileTheRelayWasGone"] = try bridge()["connection"] as? String ?? "unread"
        try control.ask("CloudOnline")
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the relay came back and the phone did not")
        record["afterTheRelayCameBack"] = try machinesOnScreen()

        try control.ask(["RestartDaemon": ["name": "laptop"]])
        XCTAssertTrue(waitUntil { ((try? self.machinesOnScreen()) ?? []).contains("laptop") },
                      "laptop restarted and never came back to the phone")
        XCTAssertTrue(waitUntil { (try? self.reconciled()) == true },
                      "the fleet was never confirmed again after the machine restarted")
        record["afterTheMachineRestarted"] = try machinesOnScreen()
        app.terminate()
    }

    // MARK: - Two accounts

    /// The other account on the same relay, which has one machine of its own
    /// and sees nothing of the first account's.
    private func anAccountThatSeesOnlyItsOwnMachines() throws {
        let app = launch(runner, as: "work", token: cast.workToken)
        let alone = waitUntil {
            ((try? self.bridge()["discovered"] as? [String]) ?? []) == ["workstation"]
        }
        let reached = try bridge()["discovered"] as? [String] ?? []
        record["workReached"] = reached
        XCTAssertTrue(alone, "the work account reached \(reached)")
        pressTab(app, "Hosts")
        waitFor(app, "hosts.pair.\(cast.workstation)", "work was never offered its own machine")
        XCTAssertFalse(element(app, "hosts.row.\(cast.laptop)").exists,
                       "one account's machine is on another account's Hosts tab")
        XCTAssertFalse(element(app, "hosts.pair.\(cast.laptop)").exists,
                       "one account's machine is offered to another account")
        record["workSawBeforePairing"] = try machinesOnScreen()

        // Its own machine, paired under its own identity: the two accounts on
        // this phone present different keys, so trusting one trusts nothing
        // for the other.
        let pin = try code(from: "workstation")
        try door(runner, .init(kind: "pairByCode", host: cast.workstation, pin: pin))
        let itsOwnOnly = waitUntil { (try? self.machinesOnScreen()) == ["workstation"] }
        let workSees = try machinesOnScreen()
        record["workSawAfterPairing"] = workSees
        XCTAssertTrue(itsOwnOnly, "work sees \(workSees) after pairing with its own machine")
        let itsOwn = try inventory("workstation").devices
        record["workstationDevices"] = itsOwn.count
        XCTAssertEqual(itsOwn.count, 1, "workstation holds \(itsOwn.count) keys")

        // And back to the first account, which still has the machines it
        // paired with and none of this one's.
        try door(runner, .init(kind: "connect", relay: runner.relay, token: runner.token,
                               user: runner.user))
        let back = waitUntil { (try? self.machinesOnScreen()) == ["laptop"] }
        let personalSees = try machinesOnScreen()
        record["personalSawAfterTheOtherAccount"] = personalSees
        XCTAssertTrue(back, "the first account sees \(personalSees) after the second signed in")
        app.terminate()
    }
}
